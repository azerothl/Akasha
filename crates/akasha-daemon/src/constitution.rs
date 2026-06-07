//! Constitution governance (E1 / S-RAG-02): load `data_dir/constitution.yaml` and filter memory recall.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct Constitution {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub governance: Governance,
    #[serde(default)]
    pub defaults: ConstitutionDefaults,
    #[serde(default)]
    pub recall_filter: RecallFilter,
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct Governance {
    #[serde(default)]
    pub level_1: GovernanceLevel,
    #[serde(default)]
    pub level_2: Level2Governance,
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct GovernanceLevel {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub rules: Vec<GovernanceRule>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct GovernanceRule {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub text: String,
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct Level2Governance {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub profiles: std::collections::HashMap<String, OperationalProfile>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct OperationalProfile {
    #[serde(default)]
    pub approvals_required_for: Vec<String>,
    #[serde(default)]
    pub memory: ProfileMemory,
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct ProfileMemory {
    #[serde(default)]
    pub retention_days: Option<u32>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct ConstitutionDefaults {
    #[serde(default)]
    pub active_profile: String,
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct RecallFilter {
    /// Substrings that block a memory entry from recall (case-insensitive).
    #[serde(default)]
    pub blocked_substrings: Vec<String>,
}

impl Constitution {
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join("constitution.yaml");
        if !path.is_file() {
            return Self::default();
        }
        match std::fs::read_to_string(&path) {
            Ok(s) => serde_yaml::from_str(&s).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("constitution.yaml")
    }

    pub fn is_configured(&self) -> bool {
        !self.governance.level_1.rules.is_empty()
            || !self.defaults.active_profile.is_empty()
            || !self.recall_filter.blocked_substrings.is_empty()
    }

    pub fn active_profile_name(&self) -> &str {
        if self.defaults.active_profile.is_empty() {
            "balanced"
        } else {
            &self.defaults.active_profile
        }
    }

    /// Read-only constitution rules injected into recall policy block.
    pub fn recall_policy_text(&self) -> String {
        let mut lines = Vec::new();
        if !self.governance.level_1.name.is_empty() {
            lines.push(format!("[{}]", self.governance.level_1.name));
        }
        for rule in &self.governance.level_1.rules {
            if rule.text.is_empty() {
                continue;
            }
            if rule.id.is_empty() {
                lines.push(format!("- {}", rule.text));
            } else {
                lines.push(format!("- {}: {}", rule.id, rule.text));
            }
        }
        let profile_name = self.active_profile_name();
        if let Some(profile) = self.governance.level_2.profiles.get(profile_name) {
            if !self.governance.level_2.name.is_empty() {
                lines.push(format!(
                    "[{} — profile {}]",
                    self.governance.level_2.name, profile_name
                ));
            }
            for approval in &profile.approvals_required_for {
                lines.push(format!("- Approbation requise: {approval}"));
            }
            if let Some(days) = profile.memory.retention_days {
                lines.push(format!("- Rétention mémoire max: {days} jours"));
            }
        }
        lines.join("\n")
    }

    fn blocked_patterns(&self) -> Vec<String> {
        let mut patterns = self.recall_filter.blocked_substrings.clone();
        if patterns.is_empty() {
            patterns = default_blocked_substrings();
        }
        patterns
    }

    /// Returns true when recalled content violates constitution recall constraints.
    pub fn blocks_recall_content(&self, content: &str) -> bool {
        if content.trim().is_empty() {
            return false;
        }
        let lower = content.to_lowercase();
        self.blocked_patterns()
            .iter()
            .any(|pat| !pat.is_empty() && lower.contains(&pat.to_lowercase()))
    }
}

fn default_blocked_substrings() -> Vec<String> {
    vec![
        "api_key=".into(),
        "password=".into(),
        "secret=".into(),
        "AKASHA_MEMORY_KEY".into(),
        "BEGIN PRIVATE KEY".into(),
        "BEGIN RSA PRIVATE KEY".into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_secret_like_content() {
        let c = Constitution::default();
        assert!(c.blocks_recall_content("stored password=abc123 in memory"));
        assert!(!c.blocks_recall_content("user prefers dark mode"));
    }

    #[test]
    fn recall_policy_includes_level_1_rules() {
        let yaml = r#"
version: 1
governance:
  level_1:
    name: Core
    rules:
      - id: safety
        text: Never destructive by default.
defaults:
  active_profile: balanced
"#;
        let c: Constitution = serde_yaml::from_str(yaml).unwrap();
        let text = c.recall_policy_text();
        assert!(text.contains("safety"));
        assert!(text.contains("Never destructive"));
    }
}
