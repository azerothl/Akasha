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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteEntry {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub config: Option<serde_json::Value>,
}

impl RouteEntry {
    /// Extract model config from serde_json::Value and apply to CompletionRequest.
    /// Supports: temperature, top_p, top_k, frequency_penalty, presence_penalty, repeat_penalty, num_ctx, num_gpu, max_tokens.
    pub fn apply_config_to_request(&self, request: &mut crate::provider::CompletionRequest) {
        if let Some(ref config) = self.config {
            // Accept both YAML styles:
            // 1) config: { max_tokens: 40960, temperature: 0.7 }
            // 2) config: [ { max_tokens: 40960 }, { temperature: 0.7 } ]
            // The second style is not ideal, but we support it for backward compatibility.
            let get_value = |key: &str| -> Option<&serde_json::Value> {
                match config {
                    serde_json::Value::Object(map) => map.get(key),
                    serde_json::Value::Array(arr) => arr.iter().find_map(|item| item.get(key)),
                    _ => None,
                }
            };

            if let Some(temp) = get_value("temperature").and_then(|v| v.as_f64()) {
                request.temperature = Some(temp as f32);
            }
            if let Some(tp) = get_value("top_p").and_then(|v| v.as_f64()) {
                request.top_p = Some(tp as f32);
            }
            if let Some(tk) = get_value("top_k").and_then(|v| v.as_u64()) {
                request.top_k = Some(tk as u32);
            }
            if let Some(fp) = get_value("frequency_penalty").and_then(|v| v.as_f64()) {
                request.frequency_penalty = Some(fp as f32);
            }
            if let Some(pp) = get_value("presence_penalty").and_then(|v| v.as_f64()) {
                request.presence_penalty = Some(pp as f32);
            }
            if let Some(rp) = get_value("repeat_penalty").and_then(|v| v.as_f64()) {
                request.repeat_penalty = Some(rp as f32);
            }
            if let Some(nc) = get_value("num_ctx").and_then(|v| v.as_u64()) {
                request.num_ctx = Some(nc as u32);
            }
            if let Some(ng) = get_value("num_gpu").and_then(|v| v.as_u64()) {
                request.num_gpu = Some(ng as u32);
            }
            // Accept both max_tokens (preferred) and max_token (legacy typo).
            if let Some(mt) = get_value("max_tokens")
                .and_then(|v| v.as_u64())
                .or_else(|| get_value("max_token").and_then(|v| v.as_u64()))
            {
                request.max_tokens = Some(mt as u32);
            }
        }
    }
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
    /// Optional site URL for OpenRouter (HTTP-Referer header).
    #[serde(default)]
    pub site_url: Option<String>,
    /// Optional app/site name for OpenRouter (X-OpenRouter-Title header).
    #[serde(default)]
    pub app_title: Option<String>,
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

    /// Default routing: embedded model (akasha_embedded) as primary for all task types.
    /// Users can switch to Ollama/OpenAI/etc. via `akasha config models set <category> <provider> <model>`.
    pub fn default_config() -> Self {
        let internal = RouteEntry {
            provider: "akasha_embedded".into(),
            model: "default".into(),
            config: None,
        };
        let fallback_entry = RouteEntry {
            provider: "akasha_core".into(),
            model: "core".into(),
            config: None,
        };
        let mut task_types = HashMap::new();
        for name in ["conversation", "code_generation", "system_diagnostic", "system"] {
            task_types.insert(
                name.into(),
                TaskTypeConfig {
                    primary: Some(internal.clone()),
                    fallback: vec![fallback_entry.clone()],
                    constraints: None,
                },
            );
        }
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

    /// Set the primary provider/model for a task type (e.g. from TUI or API). Creates the entry if missing.
    /// If there was already a primary, it is moved to the front of the fallback list (no duplicate added if already present).
    pub fn set_primary_route(&mut self, task_type: &str, entry: RouteEntry) {
        let tt = self.task_types.entry(task_type.to_string()).or_insert_with(|| TaskTypeConfig {
            primary: None,
            fallback: vec![
                RouteEntry { provider: "akasha_embedded".into(), model: "default".into(), config: None },
                RouteEntry { provider: "akasha_core".into(), model: "core".into(), config: None },
            ],
            constraints: None,
        });
        if let Some(old) = tt.primary.take() {
            if old != entry {
                let already_in_fallback = tt.fallback.iter().any(|e| e.provider == old.provider && e.model == old.model);
                if !already_in_fallback {
                    tt.fallback.insert(0, old);
                }
            }
        }
        tt.primary = Some(entry);
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
