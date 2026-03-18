//! Soft validation of agent replies against YAML contracts in spec/agent_contracts/.

use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Deserialize, Clone)]
pub struct AgentContract {
    pub agent_type: String,
    #[serde(default)]
    pub min_response_chars: Option<usize>,
    /// All substrings must appear in reply (case-sensitive unless ignore_case).
    #[serde(default)]
    pub response_must_contain: Vec<String>,
    #[serde(default)]
    pub ignore_case: bool,
}

#[derive(Clone, Default)]
pub struct ContractRegistry {
    by_agent: HashMap<String, AgentContract>,
}

impl ContractRegistry {
    pub fn load(spec_dir: &Path) -> Self {
        let mut by_agent = HashMap::new();
        let dir = spec_dir.join("agent_contracts");
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.extension().map(|x| x == "yaml" || x == "yml").unwrap_or(false) {
                    if let Ok(raw) = std::fs::read_to_string(&p) {
                        if let Ok(c) = serde_yaml::from_str::<AgentContract>(&raw) {
                            by_agent.insert(c.agent_type.to_lowercase(), c);
                        }
                    }
                }
            }
        }
        Self { by_agent }
    }

    pub fn check(&self, agent_type: &str, reply: &str) -> Option<String> {
        let c = self.by_agent.get(&agent_type.to_lowercase())?;
        let text = if c.ignore_case {
            reply.to_lowercase()
        } else {
            reply.to_string()
        };
        if let Some(min) = c.min_response_chars {
            let len = reply.trim().chars().count();
            if len < min {
                return Some(format!(
                    "response length {} < min {}",
                    len, min
                ));
            }
        }
        for sub in &c.response_must_contain {
            let needle = if c.ignore_case {
                sub.to_lowercase()
            } else {
                sub.clone()
            };
            let hay = if c.ignore_case {
                text.as_str()
            } else {
                reply
            };
            if !hay.contains(needle.as_str()) {
                return Some(format!("missing required substring: {:?}", sub));
            }
        }
        None
    }
}

pub type SharedContractRegistry = Arc<RwLock<ContractRegistry>>;
