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

/// Declarative network policy for WASM plugins that import `akasha::http_fetch`.
/// Used only when `permissions` contains `"network"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginNetworkConfig {
    /// Only HTTPS URLs starting with one of these prefixes are allowed (e.g. `https://router.project-osrm.org`).
    #[serde(default)]
    pub allowed_url_prefixes: Vec<String>,
    #[serde(default = "default_max_response_bytes")]
    pub max_response_bytes: u64,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_https_only")]
    pub https_only: bool,
    #[serde(default = "default_max_requests_per_run")]
    pub max_requests_per_run: u32,
}

fn default_max_response_bytes() -> u64 {
    2_000_000
}

fn default_timeout_ms() -> u64 {
    20_000
}

fn default_https_only() -> bool {
    true
}

fn default_max_requests_per_run() -> u32 {
    8
}

impl Default for PluginNetworkConfig {
    fn default() -> Self {
        Self {
            allowed_url_prefixes: Vec::new(),
            max_response_bytes: default_max_response_bytes(),
            timeout_ms: default_timeout_ms(),
            https_only: default_https_only(),
            max_requests_per_run: default_max_requests_per_run(),
        }
    }
}

impl PluginNetworkConfig {
    /// Effective policy when manifest declares `network` permission.
    /// Empty allowlist means all remote HTTP is denied until the user configures prefixes.
    pub fn from_manifest_permissions(
        permissions: &[String],
        network: Option<&PluginNetworkConfig>,
    ) -> Option<PluginNetworkConfig> {
        if !permissions.iter().any(|p| p.eq_ignore_ascii_case("network")) {
            return None;
        }
        Some(network.cloned().unwrap_or_default())
    }
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
    /// Optional hook event subscriptions (e.g. `task_completed`) handled by plugin hook bus.
    #[serde(default)]
    pub hook_events: Vec<String>,
    /// Optional HTTP sandbox when `permissions` includes `"network"`.
    #[serde(default)]
    pub network: Option<PluginNetworkConfig>,
}

/// Returns true if `id` is safe to use as a directory name under `plugins/` (no path traversal).
pub fn is_safe_plugin_id(id: &str) -> bool {
    if id.is_empty() || id == "." || id == ".." {
        return false;
    }
    if !id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return false;
    }
    use std::path::Component;
    let mut comps = std::path::Path::new(id).components();
    matches!(comps.next(), Some(Component::Normal(_))) && comps.next().is_none()
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
