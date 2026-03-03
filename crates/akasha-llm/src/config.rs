//! Routing config — load from YAML, task_type -> primary + fallback_chain.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Per-model metadata from Ollama /api/show (context max, num_ctx, family, etc.).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelOption {
    pub context_length_max: Option<u64>,
    pub num_ctx: Option<u64>,
    pub family: Option<String>,
    pub parameter_size: Option<String>,
    #[serde(default)]
    pub capabilities: Option<Vec<String>>,
    pub modified_at: Option<String>,
    /// Extra fields from Ollama show (e.g. details, parameters preview).
    #[serde(default)]
    pub extra: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingConfig {
    #[serde(default)]
    pub global: GlobalConfig,
    #[serde(default)]
    pub task_types: HashMap<String, TaskTypeConfig>,
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
    /// Optional per-model metadata (from Ollama show); used by CLI/config, router ignores.
    #[serde(default)]
    pub model_options: HashMap<String, ModelOption>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GlobalConfig {
    pub enable_metrics: Option<bool>,
    pub enable_fallback: Option<bool>,
    pub default_timeout_secs: Option<u64>,
    pub default_max_retries: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskTypeConfig {
    pub primary: Option<RouteEntry>,
    #[serde(default)]
    pub fallback: Vec<RouteEntry>,
    #[serde(default)]
    pub constraints: Option<RouteConstraints>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteEntry {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub config: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteConstraints {
    pub max_cost_per_request: Option<f64>,
    pub max_latency_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub api_key_ref: Option<String>,
    pub base_url: Option<String>,
    pub organization: Option<String>,
    pub version: Option<String>,
    #[serde(default)]
    pub always_available: Option<bool>,
}

impl Default for TaskTypeConfig {
    fn default() -> Self {
        Self {
            primary: None,
            fallback: vec![],
            constraints: None,
        }
    }
}

impl RoutingConfig {
    pub fn load_from_path(path: &Path) -> anyhow::Result<Self> {
        let s = std::fs::read_to_string(path)?;
        let config: Self = serde_yaml::from_str(&s)?;
        Ok(config)
    }

    pub fn default_config() -> Self {
        let mut task_types = HashMap::new();
        task_types.insert(
            "conversation".into(),
            TaskTypeConfig {
                primary: Some(RouteEntry {
                    provider: "ollama".into(),
                    model: "llama3.2".into(),
                    config: None,
                }),
                fallback: vec![RouteEntry {
                    provider: "akasha_core".into(),
                    model: "core".into(),
                    config: None,
                }],
                constraints: None,
            },
        );
        task_types.insert(
            "code_generation".into(),
            TaskTypeConfig {
                primary: Some(RouteEntry {
                    provider: "ollama".into(),
                    model: "codellama".into(),
                    config: None,
                }),
                fallback: vec![RouteEntry {
                    provider: "akasha_core".into(),
                    model: "core".into(),
                    config: None,
                }],
                constraints: None,
            },
        );
        task_types.insert(
            "system_diagnostic".into(),
            TaskTypeConfig {
                primary: Some(RouteEntry {
                    provider: "ollama".into(),
                    model: "llama3.2".into(),
                    config: None,
                }),
                fallback: vec![RouteEntry {
                    provider: "akasha_core".into(),
                    model: "core".into(),
                    config: None,
                }],
                constraints: None,
            },
        );
        Self {
            global: GlobalConfig {
                enable_metrics: Some(true),
                enable_fallback: Some(true),
                default_timeout_secs: Some(300),
                default_max_retries: Some(2),
            },
            task_types,
            providers: HashMap::new(),
            model_options: HashMap::new(),
        }
    }

    pub fn save_to_path(&self, path: &Path) -> anyhow::Result<()> {
        let s = serde_yaml::to_string(self)?;
        std::fs::write(path, s)?;
        Ok(())
    }

    pub fn get_route(&self, task_type: &str) -> Option<&TaskTypeConfig> {
        self.task_types.get(task_type)
    }

    /// Collect all (provider, model) from routing config for listing in UI.
    pub fn list_models_by_provider(&self) -> std::collections::HashMap<String, Vec<String>> {
        let mut by_provider: HashMap<String, HashSet<String>> = HashMap::new();
        for tc in self.task_types.values() {
            if let Some(ref p) = tc.primary {
                by_provider
                    .entry(p.provider.clone())
                    .or_default()
                    .insert(p.model.clone());
            }
            for entry in &tc.fallback {
                by_provider
                    .entry(entry.provider.clone())
                    .or_default()
                    .insert(entry.model.clone());
            }
        }
        by_provider
            .into_iter()
            .map(|(k, v)| (k, v.into_iter().collect::<Vec<_>>()))
            .collect()
    }
}
