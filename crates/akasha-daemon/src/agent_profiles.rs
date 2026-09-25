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

    fn is_valid_id(id: &str) -> bool {
        !id.contains("..") && !id.contains('/') && !id.contains('\\')
    }

    pub fn ensure_defaults(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        // Composer modes (P6 A2): architect / code / ask — Roo-style presets.
        let mode_defaults: &[(&str, &str, &str, &str, &[&str])] = &[
            (
                "architect",
                "Architect",
                "Plan and design only: architecture, task breakdown, no full implementation.",
                "You are the technical architect. Design the skeleton of the project. Output: proposed architecture, task list, dependencies, execution order, definition of done. Stay at design level; do not write full implementation.",
                &[
                    "read_file",
                    "list_dir",
                    "grep_content",
                    "search_files",
                    "web_search",
                    "web_fetch",
                    "memory_search",
                    "git_status",
                    "git_diff",
                    "git_log",
                ],
            ),
            (
                "code",
                "Code",
                "Implement and edit: files, shell, tests.",
                "You are the code generation agent. Produce correct, readable code. Prefer write_file and workspace:/ paths. Run builds and tests with run_command. Before editing any file, read it first.",
                &[
                    "read_file",
                    "write_file",
                    "write_code",
                    "edit_file",
                    "search_replace",
                    "apply_patch",
                    "list_dir",
                    "grep_content",
                    "run_command",
                    "git_status",
                    "git_diff",
                    "git_log",
                    "diff_unified",
                ],
            ),
            (
                "ask",
                "Ask",
                "Q&A only: read tools, no writes or shell.",
                "You are in Ask mode. Answer questions using read-only tools when needed. Do not modify files, run mutating commands, or create schedules. If the user needs edits, suggest switching to Code or Architect mode.",
                &[
                    "read_file",
                    "list_dir",
                    "grep_content",
                    "search_files",
                    "web_search",
                    "web_fetch",
                    "memory_search",
                    "git_status",
                    "git_diff",
                    "git_log",
                ],
            ),
        ];
        for (id, name, desc, system_prompt, tools) in mode_defaults {
            let p = self.path_for(id);
            if p.exists() {
                continue;
            }
            let mut overlay = HashMap::new();
            overlay.insert(
                "allowed_tools".to_string(),
                serde_json::json!(tools.iter().copied().collect::<Vec<_>>()),
            );
            let def = AgentProfileDef {
                id: id.to_string(),
                name: name.to_string(),
                description: desc.to_string(),
                system_prompt: system_prompt.to_string(),
                preferred_mode: id.to_string(),
                user_rag_top_k: if *id == "code" { 8 } else { 5 },
                graph_expand: *id == "code" || *id == "architect",
                tools_policy_overlay: overlay,
                ..Default::default()
            };
            let yaml = serde_yaml::to_string(&def)?;
            std::fs::write(p, yaml)?;
        }
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
        if !Self::is_valid_id(id) {
            return Ok(None);
        }
        let p = self.path_for(id);
        if !p.is_file() {
            return Ok(None);
        }
        let s = std::fs::read_to_string(p)?;
        Ok(Some(serde_yaml::from_str(&s)?))
    }

    pub fn upsert(&self, profile: &AgentProfileDef) -> anyhow::Result<()> {
        if !Self::is_valid_id(&profile.id) {
            anyhow::bail!("invalid agent profile id");
        }
        std::fs::create_dir_all(&self.dir)?;
        let yaml = serde_yaml::to_string(profile)?;
        std::fs::write(self.path_for(&profile.id), yaml).map_err(|e| anyhow::anyhow!(e))
    }

    pub fn delete(&self, id: &str) -> anyhow::Result<bool> {
        if !Self::is_valid_id(id) {
            return Ok(false);
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn ensure_defaults_creates_composer_modes() {
        let dir = tempdir().unwrap();
        let store = AgentProfilesStore::new(dir.path());
        store.ensure_defaults().unwrap();
        for id in ["architect", "code", "ask", "assistant", "dev", "operator"] {
            let p = store.get(id).unwrap().expect(id);
            assert_eq!(p.id, id);
            if matches!(id, "architect" | "code" | "ask") {
                assert!(!p.system_prompt.is_empty());
                assert!(p.tools_policy_overlay.contains_key("allowed_tools"));
            }
        }
    }
}
