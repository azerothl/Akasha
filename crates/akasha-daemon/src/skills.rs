//! Loadable skills for agents: definition format and registry.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SkillParameter {
    pub name: String,
    #[serde(default)]
    pub param_type: String,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SkillDef {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub parameters: Vec<SkillParameter>,
    /// Internal tool name (e.g. "read_file", "run_command") or plugin id.
    #[serde(default)]
    pub tool_ref: String,
    /// Optional: agent types that can use this skill (empty = all).
    #[serde(default)]
    pub agents: Vec<String>,
}

/// Registry of loaded skills. Load from a directory of YAML files.
#[derive(Clone)]
pub struct SkillRegistry {
    inner: Arc<RwLock<HashMap<String, SkillDef>>>,
}

impl SkillRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Load all .yaml files from the given directory. Merges with existing (overwrites by name).
    pub async fn load_from_dir(&self, dir: &Path) -> anyhow::Result<usize> {
        let mut count = 0;
        if !dir.is_dir() {
            return Ok(0);
        }
        let mut entries = tokio::fs::read_dir(dir).await?;
        let mut guard = self.inner.write().await;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().map(|e| e == "yaml" || e == "yml").unwrap_or(false) {
                if let Ok(content) = tokio::fs::read_to_string(&path).await {
                    if let Ok(skill) = serde_yaml::from_str::<SkillDef>(&content) {
                        guard.insert(skill.name.clone(), skill);
                        count += 1;
                    }
                }
            }
        }
        Ok(count)
    }

    pub async fn list(&self) -> Vec<SkillDef> {
        let guard = self.inner.read().await;
        guard.values().cloned().collect()
    }

    pub async fn get(&self, name: &str) -> Option<SkillDef> {
        let guard = self.inner.read().await;
        guard.get(name).cloned()
    }

    /// Skills allowed for an agent type (if skill.agents is empty, it's allowed for all).
    pub async fn for_agent(&self, agent_type: &str) -> Vec<SkillDef> {
        let guard = self.inner.read().await;
        guard
            .values()
            .filter(|s| s.agents.is_empty() || s.agents.iter().any(|a| a == agent_type))
            .cloned()
            .collect()
    }
}

impl Default for SkillRegistry {
    fn default() -> Self {
        Self::new()
    }
}
