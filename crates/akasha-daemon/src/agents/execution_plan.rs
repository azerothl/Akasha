//! Structured execution plan (JSON) + legacy line-based decomposition.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    pub step_id: String,
    pub agent_type: String,
    /// Message / intent for the sub-agent.
    pub intent: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub parallel_group: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPlan {
    pub plan_id: Uuid,
    pub steps: Vec<PlanStep>,
}

impl ExecutionPlan {
    pub fn from_legacy(subtasks: &[(String, String)]) -> Self {
        let steps: Vec<PlanStep> = subtasks
            .iter()
            .enumerate()
            .map(|(i, (agent, msg))| PlanStep {
                step_id: format!("s{}", i),
                agent_type: agent.clone(),
                intent: msg.clone(),
                depends_on: Vec::new(),
                parallel_group: Some(0),
            })
            .collect();
        Self {
            plan_id: Uuid::new_v4(),
            steps,
        }
    }

    /// Topological levels: steps with same level can run in parallel (no inter-deps within level).
    pub fn execution_waves(&self) -> anyhow::Result<Vec<Vec<usize>>> {
        let n = self.steps.len();
        if n == 0 {
            return Ok(vec![]);
        }
        let id_to_idx: std::collections::HashMap<&str, usize> = self
            .steps
            .iter()
            .enumerate()
            .map(|(i, s)| (s.step_id.as_str(), i))
            .collect();
        let mut level = vec![0usize; n];
        for _ in 0..n {
            let mut changed = false;
            for i in 0..n {
                let mut lv = 0usize;
                for dep in &self.steps[i].depends_on {
                    if let Some(&j) = id_to_idx.get(dep.as_str()) {
                        lv = lv.max(level[j].saturating_add(1));
                    }
                }
                if lv > level[i] {
                    level[i] = lv;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        if level.iter().any(|&l| l >= n) {
            anyhow::bail!("invalid plan: cyclic or unresolved dependencies");
        }
        let max_lv = *level.iter().max().unwrap_or(&0);
        let mut waves: Vec<Vec<usize>> = vec![vec![]; max_lv + 1];
        for i in 0..n {
            waves[level[i]].push(i);
        }
        Ok(waves)
    }

    pub fn to_subtasks(&self) -> Vec<(String, String)> {
        self.steps
            .iter()
            .map(|s| (s.agent_type.clone(), s.intent.clone()))
            .collect()
    }
}

/// Parse JSON plan from LLM output. Accepts raw JSON or fenced ```json block.
pub fn parse_plan_json(text: &str) -> Option<ExecutionPlan> {
    let t = text.trim();
    let json_str = if let Some(start) = t.find('{') {
        let sub = &t[start..];
        if let Some(end) = sub.rfind('}') {
            &sub[..=end]
        } else {
            return None;
        }
    } else {
        return None;
    };
    #[derive(Deserialize)]
    struct Raw {
        steps: Vec<PlanStep>,
        #[serde(default)]
        plan_id: Option<Uuid>,
    }
    let raw: Raw = serde_json::from_str(json_str).ok()?;
    if raw.steps.is_empty() {
        return None;
    }
    const MAX_STEPS: usize = 32;
    if raw.steps.len() > MAX_STEPS {
        return None;
    }
    for (i, s) in raw.steps.iter().enumerate() {
        if s.step_id.is_empty() {
            return None;
        }
        if s.agent_type.is_empty() || s.intent.is_empty() {
            return None;
        }
        // Ensure unique step_ids
        if raw.steps.iter().filter(|x| x.step_id == s.step_id).count() > 1 {
            return None;
        }
        let _ = i;
    }
    Some(ExecutionPlan {
        plan_id: raw.plan_id.unwrap_or_else(Uuid::new_v4),
        steps: raw.steps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waves_independent_all_same_level() {
        let p = ExecutionPlan::from_legacy(&[
            ("a".into(), "1".into()),
            ("b".into(), "2".into()),
        ]);
        let w = p.execution_waves().unwrap();
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].len(), 2);
    }

    #[test]
    fn waves_with_dep() {
        let p = ExecutionPlan {
            plan_id: Uuid::nil(),
            steps: vec![
                PlanStep {
                    step_id: "s0".into(),
                    agent_type: "search".into(),
                    intent: "find X".into(),
                    depends_on: vec![],
                    parallel_group: None,
                },
                PlanStep {
                    step_id: "s1".into(),
                    agent_type: "code".into(),
                    intent: "use X".into(),
                    depends_on: vec!["s0".into()],
                    parallel_group: None,
                },
            ],
        };
        let w = p.execution_waves().unwrap();
        assert_eq!(w.len(), 2);
        assert_eq!(w[0], vec![0]);
        assert_eq!(w[1], vec![1]);
    }
}
