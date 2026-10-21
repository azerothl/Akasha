//! Loadable skills for agents: definition format and registry.
//! Supports Agent Skills spec (agentskills.io): directory with SKILL.md (front matter + body),
//! and legacy flat .yaml files.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::sync::RwLock;

async fn read_text_bounded(path: &Path, limit: u64) -> anyhow::Result<String> {
    let file = tokio::fs::File::open(path).await?;
    let mut content = String::new();
    file.take(limit + 1).read_to_string(&mut content).await?;
    anyhow::ensure!(
        content.len() as u64 <= limit,
        "skill file exceeds bounded context"
    );
    Ok(content)
}

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
    #[serde(default)]
    pub metadata: serde_json::Value,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_root: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
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
    let content = read_text_bounded(path, 64 * 1024).await?;
    let (fm, _body_start) = match parse_skill_front_matter(&content) {
        Some(x) => x,
        None => return Ok(None),
    };
    anyhow::ensure!(
        fm.name.len() <= 64
            && !fm.name.is_empty()
            && fm
                .name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_'),
        "invalid skill name"
    );
    anyhow::ensure!(
        !crate::api::AVAILABLE_TOOLS
            .iter()
            .any(|(name, _)| *name == fm.name)
            || fm.tool_ref == fm.name,
        "skill cannot shadow a builtin tool with another capability"
    );
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
        source_root: path.parent().map(Path::to_path_buf),
        sha256: Some(format!("{:x}", Sha256::digest(content.as_bytes()))),
        version: fm
            .metadata
            .get("version")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
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
            } else if path
                .extension()
                .map(|e| e == "yaml" || e == "yml")
                .unwrap_or(false)
            {
                if let Ok(content) = read_text_bounded(&path, 64 * 1024).await {
                    if let Ok(mut skill) = serde_yaml::from_str::<SkillDef>(&content) {
                        anyhow::ensure!(!crate::api::AVAILABLE_TOOLS.iter().any(|(name,_)|*name==skill.name) || skill.tool_ref==skill.name,"skill cannot shadow a builtin tool with another capability");
                        skill.body_path = None;
                        skill.source_root = path.parent().map(Path::to_path_buf);
                        skill.sha256 = Some(format!("{:x}", Sha256::digest(content.as_bytes())));
                        guard.insert(skill.name.clone(), skill);
                        count += 1;
                    }
                }
            }
        }
        Ok(count)
    }

    /// Clear and load from data_dir/skills and spec_dir/skills. Used at startup and for reload.
    pub async fn load_all(&self, data_dir: &Path, spec_dir: &Path) -> anyhow::Result<usize> {
        // Pack < user data. Build a new registry before publishing it atomically.
        let next = Self::new();
        let n1 = next.load_from_dir(&spec_dir.join("skills")).await?;
        let n2 = next.load_from_dir(&data_dir.join("skills")).await?;
        *self.inner.write().await = next.inner.read().await.clone();
        Ok(n1 + n2)
    }

    /// A task-local overlay, never published to other projects. Ancestors are bounded by root.
    pub async fn for_project(&self, root: &Path) -> anyhow::Result<Self> {
        let root = root.canonicalize()?;
        let scoped = Self {
            inner: Arc::new(RwLock::new(self.inner.read().await.clone())),
        };
        for relative in [".agents/skills", ".akasha/skills"] {
            let dir = root.join(relative);
            if !dir.exists() {
                continue;
            }
            let dir = dir.canonicalize()?;
            anyhow::ensure!(
                dir.starts_with(&root),
                "project skill directory escapes workspace"
            );
            let mut entries = tokio::fs::read_dir(&dir).await?;
            let mut count = 0usize;
            while let Some(entry) = entries.next_entry().await? {
                count += 1;
                anyhow::ensure!(count <= 128, "too many project skills");
                let path = entry.path().canonicalize()?;
                anyhow::ensure!(
                    path.starts_with(&dir),
                    "skill entry escapes project skill directory"
                );
                let md = if path.is_dir() {
                    path.join("SKILL.md")
                } else {
                    path.clone()
                };
                if md.is_file() {
                    let actual = md.canonicalize()?;
                    anyhow::ensure!(
                        actual.starts_with(&dir),
                        "skill source escapes project skill directory"
                    );
                    anyhow::ensure!(
                        std::fs::metadata(&actual)?.len() <= 64 * 1024,
                        "skill source too large"
                    );
                    if path.is_dir() {
                        anyhow::ensure!(
                            load_skill_from_md(&actual).await?.is_some(),
                            "invalid project SKILL.md front matter"
                        );
                    }
                }
            }
            scoped.load_from_dir(&dir).await?;
        }
        Ok(scoped)
    }

    pub async fn get_for_agent(&self, name: &str, agent: &str) -> Option<SkillDef> {
        self.get(name).await.filter(|skill| {
            skill.agents.is_empty() || skill.agents.iter().any(|a| a.eq_ignore_ascii_case(agent))
        })
    }

    /// Read instructions/resources under their pinned source directory; never execute a resource.
    pub async fn activate(
        &self,
        name: &str,
        agent: &str,
        resource: Option<&str>,
    ) -> Option<(String, SkillDef)> {
        let skill = self.get_for_agent(name, agent).await?;
        let source = skill.body_path.as_ref()?;
        let content = read_text_bounded(source, 64 * 1024).await.ok()?;
        if skill.sha256.as_deref()
            != Some(format!("{:x}", Sha256::digest(content.as_bytes())).as_str())
        {
            return None;
        }
        let body = if let Some(relative) = resource {
            if relative.contains(':') {
                return None;
            }
            let relative = Path::new(relative);
            if relative.is_absolute()
                || relative
                    .components()
                    .any(|c| !matches!(c, std::path::Component::Normal(_)))
            {
                return None;
            }
            let root = source.parent()?.canonicalize().ok()?;
            let file = root.join(relative).canonicalize().ok()?;
            if !file.starts_with(&root)
                || !file.is_file()
                || std::fs::metadata(&file).ok()?.len() > 32 * 1024
            {
                return None;
            }
            read_text_bounded(&file, 32 * 1024).await.ok()?
        } else {
            let (_, start) = parse_skill_front_matter(&content)?;
            content.get(start..)?.trim().to_owned()
        };
        Some((body, skill))
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
        let content = read_text_bounded(&path, 64 * 1024).await.ok()?;
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
        let mut skills: Vec<_> = guard.values().cloned().collect();
        skills.sort_by(|a, b| a.name.cmp(&b.name));
        skills
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
            .filter(|s| {
                s.agents.is_empty() || s.agents.iter().any(|a| a.eq_ignore_ascii_case(agent_type))
            })
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
        assert!(root.is_file(), "P9 pilot missing at {}", root.display());
        let def = load_skill_from_md(&root)
            .await
            .expect("io")
            .expect("front matter");
        assert_eq!(def.name, "morning-brief");
        assert!(!def.description.is_empty());
        assert_eq!(def.body_path.as_ref(), Some(&root));
    }
}
