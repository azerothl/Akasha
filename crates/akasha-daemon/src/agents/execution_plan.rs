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
    /// Optional definition-of-done / acceptance criteria for this step.
    #[serde(default)]
    pub acceptance_criteria: Option<String>,
    /// Optional list of expected deliverables (paths, filenames).
    #[serde(default)]
    pub deliverables: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPlan {
    pub plan_id: Uuid,
    pub steps: Vec<PlanStep>,
}

/// Max characters for shared plan injected into sub-agents (env override).
pub fn plan_context_max_chars() -> usize {
    std::env::var("AKASHA_PLAN_CONTEXT_MAX_CHARS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(12_000)
}

const MAX_INTENT_CHARS: usize = 8000;
const MAX_ACCEPTANCE_CHARS: usize = 4000;
const MAX_DELIVERABLES: usize = 24;
const MAX_DELIVERABLE_ITEM_LEN: usize = 500;

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
                acceptance_criteria: None,
                deliverables: None,
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

    /// Markdown block: full user request summary + all steps (truncated if needed) + highlighted current step.
    pub fn shared_context_markdown(&self, user_request: &str, current: &PlanStep) -> String {
        let max_total = plan_context_max_chars();
        let user_trim: String = user_request.chars().take(2000).collect();
        let mut out = String::new();
        out.push_str("## User request (root)\n\n");
        out.push_str(user_trim.trim());
        out.push_str("\n\n## Execution plan (all steps)\n\n");
        out.push_str("| step_id | agent | depends_on | intent (preview) |\n");
        out.push_str("|---------|-------|------------|------------------|\n");
        let other_limit = 400usize;
        for s in &self.steps {
            let intent_preview: String = if s.step_id == current.step_id {
                s.intent.clone()
            } else {
                truncate_chars(&s.intent, other_limit)
            };
            let deps = if s.depends_on.is_empty() {
                "(none)".to_string()
            } else {
                s.depends_on.join(", ")
            };
            let row = format!(
                "| {} | {} | {} | {} |\n",
                s.step_id,
                s.agent_type,
                deps,
                intent_preview.replace('\n', " ").replace('|', "\\|")
            );
            out.push_str(&row);
        }
        out.push_str("\n### Step details\n\n");
        for s in &self.steps {
            out.push_str(&format!("- **{}** (`{}`):\n", s.step_id, s.agent_type));
            let intent_line = if s.step_id == current.step_id {
                s.intent.as_str()
            } else {
                &truncate_chars(&s.intent, other_limit)
            };
            out.push_str("  - intent: ");
            out.push_str(&intent_line.replace('\n', "\n    "));
            out.push('\n');
            if let Some(ref a) = s.acceptance_criteria {
                let t = truncate_chars(a, 800);
                out.push_str("  - acceptance_criteria: ");
                out.push_str(&t);
                out.push('\n');
            }
            if let Some(ref d) = s.deliverables {
                if !d.is_empty() {
                    out.push_str("  - deliverables: ");
                    out.push_str(&d.join(", "));
                    out.push('\n');
                }
            }
        }
        out.push_str("\n## Your assignment (this step only)\n\n");
        out.push_str(&format!(
            "- **step_id**: `{}`\n- **agent_type**: `{}`\n",
            current.step_id, current.agent_type
        ));
        if !current.depends_on.is_empty() {
            out.push_str(&format!(
                "- **depends_on**: {}\n",
                current.depends_on.join(", ")
            ));
        }
        out.push_str("\n### Intent\n\n");
        out.push_str(&current.intent);
        if let Some(ref a) = current.acceptance_criteria {
            out.push_str("\n\n### Acceptance criteria\n\n");
            out.push_str(a);
        }
        if let Some(ref d) = current.deliverables {
            if !d.is_empty() {
                out.push_str("\n\n### Expected deliverables\n\n");
                for x in d {
                    out.push_str(&format!("- {}\n", x));
                }
            }
        }
        out.push_str("\n\n---\nFollow your step only; other steps run in parallel or in other waves as scheduled.\n");

        if out.len() <= max_total {
            return out;
        }
        // Truncate: keep head through "## Your assignment" start, then full current intent section.
        let marker = "## Your assignment (this step only)";
        if let Some(pos) = out.find(marker) {
            let head = truncate_bytes_safe(&out[..pos], max_total.saturating_sub(4000));
            let tail = &out[pos..];
            let tail_trim = truncate_bytes_safe(tail, max_total - head.len());
            format!("{}\n\n_(Earlier plan rows truncated for context limit.)_\n\n{}", head, tail_trim)
        } else {
            truncate_bytes_safe(&out, max_total)
        }
    }
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    s.chars().take(max_chars).chain(std::iter::once('…')).collect()
}

fn truncate_bytes_safe(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        return s.to_string();
    }
    let mut end = max_len;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

fn sanitize_plan_step(s: &mut PlanStep) {
    if s.intent.len() > MAX_INTENT_CHARS {
        s.intent = truncate_bytes_safe(&s.intent, MAX_INTENT_CHARS);
    }
    if let Some(ref mut a) = s.acceptance_criteria {
        if a.len() > MAX_ACCEPTANCE_CHARS {
            *a = truncate_bytes_safe(a, MAX_ACCEPTANCE_CHARS);
        }
        if a.trim().is_empty() {
            s.acceptance_criteria = None;
        }
    }
    if let Some(ref mut d) = s.deliverables {
        d.retain(|x| !x.trim().is_empty());
        d.truncate(MAX_DELIVERABLES);
        for x in d.iter_mut() {
            if x.len() > MAX_DELIVERABLE_ITEM_LEN {
                *x = truncate_bytes_safe(x, MAX_DELIVERABLE_ITEM_LEN);
            }
        }
        if d.is_empty() {
            s.deliverables = None;
        }
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
    let mut raw: Raw = serde_json::from_str(json_str).ok()?;
    if raw.steps.is_empty() {
        return None;
    }
    const MAX_STEPS: usize = 32;
    if raw.steps.len() > MAX_STEPS {
        return None;
    }
    for s in raw.steps.iter_mut() {
        if s.step_id.is_empty() {
            return None;
        }
        if s.agent_type.is_empty() || s.intent.is_empty() {
            return None;
        }
        sanitize_plan_step(s);
    }
    for s in &raw.steps {
        if raw.steps.iter().filter(|x| x.step_id == s.step_id).count() > 1 {
            return None;
        }
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
                    acceptance_criteria: None,
                    deliverables: None,
                },
                PlanStep {
                    step_id: "s1".into(),
                    agent_type: "code".into(),
                    intent: "use X".into(),
                    depends_on: vec!["s0".into()],
                    parallel_group: None,
                    acceptance_criteria: None,
                    deliverables: None,
                },
            ],
        };
        let w = p.execution_waves().unwrap();
        assert_eq!(w.len(), 2);
        assert_eq!(w[0], vec![0]);
        assert_eq!(w[1], vec![1]);
    }

    #[test]
    fn parse_plan_json_enriched_fields() {
        let j = r#"{"steps":[{"step_id":"s0","agent_type":"analyst","intent":"Full intent here","depends_on":[],"acceptance_criteria":"Must output backlog","deliverables":["workspace:/out/a.md"]}]}"#;
        let p = parse_plan_json(j).expect("parse");
        assert_eq!(p.steps.len(), 1);
        assert_eq!(p.steps[0].acceptance_criteria.as_deref(), Some("Must output backlog"));
        assert_eq!(p.steps[0].deliverables.as_ref().map(|v| v.len()), Some(1));
    }

    #[test]
    fn shared_context_includes_current_step_full_intent() {
        let p = ExecutionPlan {
            plan_id: Uuid::nil(),
            steps: vec![
                PlanStep {
                    step_id: "s0".into(),
                    agent_type: "documentalist".into(),
                    intent: "Read PDF".into(),
                    depends_on: vec![],
                    parallel_group: None,
                    acceptance_criteria: None,
                    deliverables: None,
                },
                PlanStep {
                    step_id: "s1".into(),
                    agent_type: "code".into(),
                    intent: "Implement all features from PDF".into(),
                    depends_on: vec!["s0".into()],
                    parallel_group: None,
                    acceptance_criteria: Some("Tests pass".into()),
                    deliverables: Some(vec!["workspace:/x.py".into()]),
                },
            ],
        };
        let ctx = p.shared_context_markdown("User wants exam deliverables", &p.steps[1]);
        assert!(ctx.contains("User wants exam"));
        assert!(ctx.contains("s0"));
        assert!(ctx.contains("s1"));
        assert!(ctx.contains("Implement all features"));
        assert!(ctx.contains("Tests pass"));
        assert!(ctx.contains("workspace:/x.py"));
        assert!(ctx.contains("Your assignment"));
    }
}
