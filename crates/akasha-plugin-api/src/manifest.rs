//! Plugin manifest (metadata) loaded from disk or catalog.

use crate::PluginKind;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PluginRoutingRule {
    /// Optional semantic intent key (e.g. "geolocation_distance", "transport").
    #[serde(default)]
    pub intent: Option<String>,
    /// Optional keyword list (lower-cased match against user message).
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Optional preferred tool names for this context.
    #[serde(default)]
    pub preferred_tools: Vec<String>,
    /// Optional tool names to avoid in this context.
    #[serde(default)]
    pub forbidden_tools: Vec<String>,
    /// Human-readable instruction injected into prompt when rule matches.
    #[serde(default)]
    pub instruction: String,
    /// Lower means higher priority.
    #[serde(default = "default_rule_priority")]
    pub priority: u32,
}

fn default_rule_priority() -> u32 {
    100
}

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
    /// Optional declarative prompt routing rules loaded automatically when the plugin is installed.
    #[serde(default)]
    pub routing_rules: Vec<PluginRoutingRule>,
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
