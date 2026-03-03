//! Plugin manifest (metadata) loaded from disk or catalog.

use crate::PluginKind;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub kind: PluginKind,
    /// Path to .wasm file relative to manifest dir or absolute
    pub wasm_path: Option<String>,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub description: String,
}

impl PluginManifest {
    /// Load manifest from a TOML or JSON file.
    pub fn load_from_path(path: &Path) -> anyhow::Result<Self> {
        let s = std::fs::read_to_string(path)?;
        if path.extension().map(|e| e == "json").unwrap_or(false) {
            serde_json::from_str(&s).map_err(Into::into)
        } else {
            toml::from_str(&s).map_err(Into::into)
        }
    }
}
