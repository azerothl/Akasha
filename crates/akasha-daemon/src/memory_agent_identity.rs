//! Agent identity manifest: load and promote constitution into long-term memory.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Deserialize, Default)]
pub struct AgentIdentityManifest {
    pub name: Option<String>,
    pub role: Option<String>,
    pub values: Option<Vec<String>>,
    pub constraints: Option<Vec<String>>,
    pub tone: Option<String>,
}

impl AgentIdentityManifest {
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join("agent_identity.yaml");
        if !path.is_file() {
            return Self::default();
        }
        match std::fs::read_to_string(&path) {
            Ok(s) => serde_yaml::from_str(&s).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn to_memory_content(&self) -> Option<String> {
        if self.name.is_none() && self.role.is_none() && self.tone.is_none() {
            return None;
        }
        let mut lines = vec!["[Identité agent Akasha]".to_string()];
        if let Some(n) = &self.name {
            lines.push(format!("Nom: {n}"));
        }
        if let Some(r) = &self.role {
            lines.push(format!("Rôle: {r}"));
        }
        if let Some(t) = &self.tone {
            lines.push(format!("Ton: {t}"));
        }
        if let Some(v) = &self.values {
            lines.push(format!("Valeurs: {}", v.join(", ")));
        }
        if let Some(c) = &self.constraints {
            lines.push(format!("Contraintes: {}", c.join("; ")));
        }
        Some(lines.join("\n"))
    }

    pub fn prompt_block(&self) -> String {
        self.to_memory_content().unwrap_or_default()
    }
}

/// At daemon startup: promote identity manifest if present (scope=agent, importance=permanent).
pub fn bootstrap_agent_identity(
    data_dir: &Path,
    client: Option<&crate::memory_actor::LongTermMemoryClient>,
) {
    let Some(client) = client else { return };
    let manifest = AgentIdentityManifest::load(data_dir);
    let Some(content) = manifest.to_memory_content() else {
        return;
    };
    let _ = client.promote(
        content,
        "agent_identity".to_string(),
        None,
        None,
        None,
        Some(4),
        Some("agent".to_string()),
        None,
        None,
    );
}

pub fn manifest_path(data_dir: &Path) -> PathBuf {
    data_dir.join("agent_identity.yaml")
}
