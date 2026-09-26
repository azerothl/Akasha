//! Loadable skills for agents: definition format and registry.
//! Supports Agent Skills spec (agentskills.io): directory with SKILL.md (front matter + body),
//! and legacy flat .yaml files.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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

/// Front matter parsed from SKILL.md (Agent Skills spec). Only fields we use.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
struct SkillFrontMatter {
    pub name: String,
    pub description: String,
    /// Optional: bind to an internal tool (e.g. run_command). Extension to Agent Skills spec.
    #[serde(default)]
    pub tool_ref: String,
    /// Optional: agent types that can use this skill (empty = all).
    #[serde(default)]
    pub agents: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SkillDef {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub parameters: Vec<SkillParameter>,
    /// Internal tool name (e.g. "read_file", "run_command") or plugin id. Empty for doc-only skills.
    #[serde(default)]
    pub tool_ref: String,
    /// Optional: agent types that can use this skill (empty = all).
    #[serde(default)]
    pub agents: Vec<String>,
    /// Path to SKILL.md for loading body on activation. Not exposed via API.
    #[serde(skip)]
    pub body_path: Option<PathBuf>,
}

/// Registry of loaded skills. Load from directories (SKILL.md per subdir) and/or flat YAML files.
#[derive(Clone)]
pub struct SkillRegistry {
    inner: Arc<RwLock<HashMap<String, SkillDef>>>,
}

/// Extracts YAML front matter (between first --- and second ---) from SKILL.md content.
fn parse_skill_front_matter(content: &str) -> Option<(SkillFrontMatter, usize)> {
    let rest = content.strip_prefix("---")?;
    let end = rest.find("\n---")?;
    let yaml_str = rest[..end].trim();
    let fm: SkillFrontMatter = serde_yaml::from_str(yaml_str).ok()?;
    let body_start = content.find("\n---")? + 4; // after second ---
    Some((fm, body_start))
}

/// Load one skill from a SKILL.md path (Agent Skills spec). Returns SkillDef with body_path set.
async fn load_skill_from_md(path: &Path) -> anyhow::Result<Option<SkillDef>> {
    let content = tokio::fs::read_to_string(path).await?;
    let (fm, _body_start) = match parse_skill_front_matter(&content) {
        Some(x) => x,
        None => return Ok(None),
    };
    let dir_name = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("");
    if !fm.name.is_empty() && dir_name != fm.name {
        tracing::warn!(
            skill_dir = %dir_name,
            frontmatter_name = %fm.name,
            "SKILL.md name should match directory name (Agent Skills spec)"
        );
    }
    let def = SkillDef {
        name: fm.name.clone(),
        description: fm.description.clone(),
        parameters: Vec::new(),
        tool_ref: fm.tool_ref.clone(),
        agents: fm.agents.clone(),
        body_path: Some(path.to_path_buf()),
    };
    Ok(Some(def))
}

impl SkillRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Load from one directory: (1) subdirs containing SKILL.md (Agent Skills format),
    /// (2) flat .yaml/.yml files. Merges into existing map (overwrites by name).
    pub async fn load_from_dir(&self, dir: &Path) -> anyhow::Result<usize> {
        let mut count = 0;
        if !dir.is_dir() {
            return Ok(0);
        }
        let mut entries = tokio::fs::read_dir(dir).await?;
        let mut guard = self.inner.write().await;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.is_dir() {
                let skill_md = path.join("SKILL.md");
                if skill_md.is_file() {
                    if let Ok(Some(def)) = load_skill_from_md(&skill_md).await {
                        guard.insert(def.name.clone(), def);
                        count += 1;
                    }
                }
            } else if path.extension().map(|e| e == "yaml" || e == "yml").unwrap_or(false) {
                if let Ok(content) = tokio::fs::read_to_string(&path).await {
                    if let Ok(mut skill) = serde_yaml::from_str::<SkillDef>(&content) {
                        skill.body_path = None;
                        guard.insert(skill.name.clone(), skill);
                        count += 1;
                    }
                }
            }
        }
        Ok(count)
    }

    /// Clear and load from data_dir/skills and spec_dir/skills. Used at startup and for reload.
    pub async fn load_all(
        &self,
        data_dir: &Path,
        spec_dir: &Path,
    ) -> anyhow::Result<usize> {
        let mut guard = self.inner.write().await;
        guard.clear();
        drop(guard);
        let skills_data = data_dir.join("skills");
        let skills_spec = spec_dir.join("skills");
        let n1 = self.load_from_dir(&skills_data).await?;
        let n2 = self.load_from_dir(&skills_spec).await?;
        Ok(n1 + n2)
    }

    /// Reload all skills from disk (hot reload). Same as load_all.
    pub async fn reload(&self, data_dir: &Path, spec_dir: &Path) -> anyhow::Result<usize> {
        self.load_all(data_dir, spec_dir).await
    }

    /// Load the Markdown body of a skill (content after front matter). Returns None if no body_path or read error.
    pub async fn get_body(&self, name: &str) -> Option<String> {
        let path = {
            let guard = self.inner.read().await;
            let def = guard.get(name)?;
            def.body_path.clone()?
        };
        let content = tokio::fs::read_to_string(&path).await.ok()?;
        let (_, body_start) = parse_skill_front_matter(&content)?;
        let body = content.get(body_start..).unwrap_or("").trim();
        if body.is_empty() {
            None
        } else {
            Some(body.to_string())
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dual_os_front_matter_loads_name_and_description() {
        let content = r#"---
name: morning-brief
description: Short local morning briefing from memory, open tasks, and notes — no network.
license: MIT
when_to_use: >
  User asks for a morning briefing or daily recap.
tools:
  - memory.recall
  - tasks.list
  - goal.complete
metadata:
  version: "1.0.0"
---

# Morning brief

Body text.
"#;
        let (fm, body_start) = parse_skill_front_matter(content).expect("parse dual SKILL.md");
        assert_eq!(fm.name, "morning-brief");
        assert!(fm.description.contains("morning briefing"));
        assert!(fm.tool_ref.is_empty());
        assert!(fm.agents.is_empty());
        let body = content[body_start..].trim();
        assert!(body.starts_with("# Morning brief"));
    }

    #[tokio::test]
    async fn load_morning_brief_pilot_from_spec_skills() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("spec/skills/morning-brief/SKILL.md");
        assert!(
            root.is_file(),
            "P9 pilot missing at {}",
            root.display()
        );
        let def = load_skill_from_md(&root)
            .await
            .expect("io")
            .expect("front matter");
        assert_eq!(def.name, "morning-brief");
        assert!(!def.description.is_empty());
        assert_eq!(def.body_path.as_ref(), Some(&root));
    }
}
