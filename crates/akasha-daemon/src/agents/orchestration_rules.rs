//! Deterministic rules before LLM decomposition (hybrid orchestration).

use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
struct RuleEntry {
    /// Case-insensitive substring match on user message.
    #[serde(default)]
    contains: Option<String>,
    /// Regex (optional); if both set, both must match.
    #[serde(default)]
    regex: Option<String>,
    /// Force single step to this agent with original message.
    agent_type: String,
    #[serde(default)]
    priority: u32,
}

#[derive(Debug, Deserialize)]
struct RulesFile {
    #[serde(default)]
    rules: Vec<RuleEntry>,
}

pub struct OrchestrationRules {
    entries: Vec<(Option<regex::Regex>, Option<String>, String, u32)>,
}

impl OrchestrationRules {
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join("orchestration_rules.yaml");
        let mut entries = Vec::new();
        if let Ok(raw) = std::fs::read_to_string(&path) {
            if let Ok(f) = serde_yaml::from_str::<RulesFile>(&raw) {
                let mut rules = f.rules;
                rules.sort_by_key(|r| std::cmp::Reverse(r.priority));
                for r in rules {
                    let re = r.regex.as_ref().and_then(|p| regex::Regex::new(p).ok());
                    let sub = r.contains.as_ref().map(|s| s.to_lowercase());
                    entries.push((re, sub, r.agent_type, r.priority));
                }
            }
        }
        Self { entries }
    }

    /// If a rule matches, return (agent_type, user_message) for a single-step plan.
    pub fn match_message(&self, message: &str) -> Option<(String, String)> {
        let lower = message.to_lowercase();
        for (re_opt, sub_opt, agent, _) in &self.entries {
            let sub_ok = sub_opt
                .as_ref()
                .map(|s| lower.contains(s.as_str()))
                .unwrap_or(true);
            let re_ok = re_opt
                .as_ref()
                .map(|re| re.is_match(message))
                .unwrap_or(true);
            if sub_opt.is_some() || re_opt.is_some() {
                if sub_ok && re_ok {
                    return Some((agent.clone(), message.to_string()));
                }
            }
        }
        None
    }
}

