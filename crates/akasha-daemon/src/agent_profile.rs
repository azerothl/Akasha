//! Agent profile: name, personality, rules, capabilities and restrictions.
//! Persisted in data_dir/agent_profile.json and injected into the LLM context so the agent follows it.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentProfile {
    /// Name given to the agent by the user (e.g. "Akasha", "Assistant").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Personality or tone description (e.g. "bienveillant et concis", "technique").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub personality: Option<String>,
    /// Rules the agent must follow (one per line).
    #[serde(default)]
    pub rules: Vec<String>,
    /// What the agent can do (allowed behaviours).
    #[serde(default)]
    pub can_do: Vec<String>,
    /// What the agent cannot do (restrictions).
    #[serde(default)]
    pub cannot_do: Vec<String>,
}

const FILENAME: &str = "agent_profile.json";

impl AgentProfile {
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.personality.is_none()
            && self.rules.is_empty()
            && self.can_do.is_empty()
            && self.cannot_do.is_empty()
    }

    /// Load profile from data_dir/agent_profile.json. Returns default empty profile if file missing or invalid.
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(FILENAME);
        let Ok(data) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        serde_json::from_str(&data).unwrap_or_default()
    }

    /// Save profile to data_dir/agent_profile.json.
    pub fn save(&self, data_dir: &Path) -> Result<(), std::io::Error> {
        let path = data_dir.join(FILENAME);
        std::fs::create_dir_all(data_dir)?;
        let json = serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string());
        std::fs::write(path, json)
    }

    /// Merge an extracted value into the profile (from LLM extraction). Does not save.
    pub fn apply_extracted(&mut self, kind: &str, value: String) {
        let value = value.trim().to_string();
        if value.is_empty() {
            return;
        }
        match kind {
            "AGENT_NAME" => self.name = Some(value),
            "AGENT_PERSONALITY" => self.personality = Some(value),
            "AGENT_RULE" => {
                if !self.rules.contains(&value) {
                    self.rules.push(value);
                }
            }
            "AGENT_CAN" => {
                if !self.can_do.contains(&value) {
                    self.can_do.push(value);
                }
            }
            "AGENT_CANNOT" => {
                if !self.cannot_do.contains(&value) {
                    self.cannot_do.push(value);
                }
            }
            _ => {}
        }
    }

    /// Default agent name when none is set (so the agent always has an identity in the prompt).
    pub const DEFAULT_NAME: &'static str = "Akasha";

    /// Build the context block to inject into the LLM prompt.
    /// The name is always present (default "Akasha" if unset) so the agent always has an identity and "remembers" it.
    pub fn format_for_prompt(&self) -> String {
        let name = self.name.as_deref().unwrap_or(Self::DEFAULT_NAME);
        let mut out = String::from("[Profil et consignes de l'agent]\n");
        out.push_str(&format!(
            "- Tu es « {} ». C'est ton nom. Tu te souviens de ton nom et tu peux te présenter ainsi quand c'est pertinent.\n",
            name
        ));
        if let Some(ref p) = self.personality {
            out.push_str(&format!("- Personnalité / ton : {}. Tu adoptes ce ton et cette personnalité à chaque réponse.\n", p));
        }
        if !self.rules.is_empty() {
            out.push_str("- Règles à respecter :\n");
            for r in &self.rules {
                out.push_str(&format!("  • {}\n", r));
            }
        }
        if !self.can_do.is_empty() {
            out.push_str("- Tu peux (autorisé) :\n");
            for c in &self.can_do {
                out.push_str(&format!("  • {}\n", c));
            }
        }
        if !self.cannot_do.is_empty() {
            out.push_str("- Tu ne dois pas :\n");
            for c in &self.cannot_do {
                out.push_str(&format!("  • {}\n", c));
            }
        }
        out.push_str("\n");
        out
    }
}
