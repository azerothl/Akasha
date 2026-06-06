//! Named agent profiles (Dev, Sysadmin, etc.) — user-managed, not agent-created.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentProfileDef {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub avatar: String,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub preferred_mode: String,
    #[serde(default)]
    pub user_rag_top_k: usize,
    #[serde(default)]
    pub graph_expand: bool,
    #[serde(default)]
    pub tools_policy_overlay: HashMap<String, serde_json::Value>,
}

pub struct AgentProfilesStore {
    dir: PathBuf,
}

impl AgentProfilesStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join("agent_profiles"),
        }
    }

    fn path_for(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.yaml"))
    }

    pub fn ensure_defaults(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let defaults = [
            (
                "assistant",
                "Assistant",
                "General-purpose helpful assistant.",
            ),
            (
                "dev",
                "Developer",
                "Code-focused: files, terminal, project graph.",
            ),
            (
                "operator",
                "Operator",
                "System operations, schedules, diagnostics.",
            ),
        ];
        for (id, name, desc) in defaults {
            let p = self.path_for(id);
            if p.exists() {
                continue;
            }
            let def = AgentProfileDef {
                id: id.to_string(),
                name: name.to_string(),
                description: desc.to_string(),
                preferred_mode: id.to_string(),
                user_rag_top_k: if id == "dev" { 8 } else { 5 },
                graph_expand: id == "dev",
                ..Default::default()
            };
            let yaml = serde_yaml::to_string(&def)?;
            std::fs::write(p, yaml)?;
        }
        Ok(())
    }

    pub fn list(&self) -> anyhow::Result<Vec<AgentProfileDef>> {
        self.ensure_defaults()?;
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
                continue;
            }
            if let Ok(s) = std::fs::read_to_string(&path) {
                if let Ok(p) = serde_yaml::from_str::<AgentProfileDef>(&s) {
                    out.push(p);
                }
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    pub fn get(&self, id: &str) -> anyhow::Result<Option<AgentProfileDef>> {
        let p = self.path_for(id);
        if !p.is_file() {
            return Ok(None);
        }
        let s = std::fs::read_to_string(p)?;
        Ok(Some(serde_yaml::from_str(&s)?))
    }

    pub fn upsert(&self, profile: &AgentProfileDef) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let yaml = serde_yaml::to_string(profile)?;
        std::fs::write(self.path_for(&profile.id), yaml)
            .map_err(|e| anyhow::anyhow!(e))
    }

    pub fn delete(&self, id: &str) -> anyhow::Result<bool> {
        if id == "assistant" {
            anyhow::bail!("cannot delete default assistant profile");
        }
        let p = self.path_for(id);
        if p.is_file() {
            std::fs::remove_file(p)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}
