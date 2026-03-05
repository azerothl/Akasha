//! LLM Router — classifier + config + fallback engine + provider registry.

use crate::classifier::classify_task_type;
use crate::config::{RoutingConfig, TaskTypeConfig};
use crate::fallback::{FallbackEngine, ProviderResolver};
use crate::metrics::MetricsCollector;
use crate::provider::{CompletionRequest, CompletionResponse, LLMProvider};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tracing::info;

pub struct LLMRouter {
    config: Arc<RwLock<RoutingConfig>>,
    fallback: FallbackEngine,
    metrics: Arc<MetricsCollector>,
    providers: HashMap<String, Arc<dyn LLMProvider>>,
    degraded_mode: bool,
}

impl LLMRouter {
    pub fn new(config: RoutingConfig) -> Self {
        let timeout_secs = config
            .global
            .default_timeout_secs
            .unwrap_or(300);
        let max_retries = config
            .global
            .default_max_retries
            .unwrap_or(2);
        let fallback = FallbackEngine {
            max_retries,
            timeout_per_call: std::time::Duration::from_secs(timeout_secs),
        };
        Self {
            config: Arc::new(RwLock::new(config)),
            fallback,
            metrics: Arc::new(MetricsCollector::new()),
            providers: HashMap::new(),
            degraded_mode: false,
        }
    }

    pub fn set_degraded_mode(&mut self, on: bool) {
        self.degraded_mode = on;
    }

    pub fn register_provider(&mut self, provider: Arc<dyn LLMProvider>) {
        self.providers.insert(provider.name().to_string(), provider);
    }

    /// Returns true if the given provider name is registered with the router.
    pub fn is_provider_registered(&self, provider: &str) -> bool {
        self.providers.contains_key(provider)
    }

    pub fn metrics(&self) -> Arc<MetricsCollector> {
        self.metrics.clone()
    }

    /// List models per provider from routing config (for GET /api/router/models).
    pub fn list_models_from_config(&self) -> std::collections::HashMap<String, Vec<String>> {
        self.config.read().unwrap_or_else(|e| e.into_inner()).list_models_by_provider()
    }

    /// Routes by category (primary + fallback) for GET /api/router/routes and CLI/TUI.
    pub fn routes_by_category(&self) -> HashMap<String, TaskTypeConfig> {
        self.config.read().unwrap_or_else(|e| e.into_inner()).task_types.clone()
    }

    /// Base URL of the Ollama provider from config (if set).
    pub fn ollama_base_url(&self) -> Option<String> {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .providers
            .get("ollama")
            .and_then(|c| c.base_url.clone())
    }

    /// Whether the embedded provider (akasha_embedded) is registered and available.
    pub fn embedded_available(&self) -> bool {
        self.providers
            .get("akasha_embedded")
            .map(|p| p.is_available())
            .unwrap_or(false)
    }

    /// Whether the embedded model is already loaded in memory (after first successful completion).
    /// If false, the next request will trigger download+load and may exceed the usual timeout.
    #[cfg(feature = "embedded")]
    pub fn embedded_loaded(&self) -> bool {
        akasha_embedded_llm::EmbeddedLlm::is_loaded()
    }

    #[cfg(not(feature = "embedded"))]
    pub fn embedded_loaded(&self) -> bool {
        false
    }

    /// Unload the embedded model from memory. Next completion will load it again.
    #[cfg(feature = "embedded")]
    pub fn embedded_unload(&self) {
        akasha_embedded_llm::EmbeddedLlm::unload();
    }

    #[cfg(not(feature = "embedded"))]
    pub fn embedded_unload(&self) {}

    /// Preload the embedded model in the current thread (blocking). Call from spawn_blocking at daemon startup to reduce first-request latency.
    #[cfg(feature = "embedded")]
    pub fn embedded_preload(&self) -> Result<(), String> {
        akasha_embedded_llm::EmbeddedLlm::preload()
            .map_err(|e| e.to_string())
    }

    #[cfg(not(feature = "embedded"))]
    pub fn embedded_preload(&self) -> Result<(), String> {
        Ok(())
    }

    /// Set the primary provider/model for a task type (e.g. conversation, code_generation). Applied immediately.
    pub fn set_primary_route(&self, task_type: &str, entry: crate::config::RouteEntry) {
        match self.config.write() {
            Ok(mut cfg) => {
                cfg.set_primary_route(task_type, entry);
            }
            Err(poisoned) => {
                let mut cfg = poisoned.into_inner();
                cfg.set_primary_route(task_type, entry);
            }
        }
    }

    fn resolve(&self) -> ProviderResolver {
        let providers = self.providers.clone();
        Arc::new(move |name: &str| providers.get(name).cloned())
    }

    /// Complete using preferred_task_type if set, else classifier on prompt; then routing config and fallback.
    pub async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse, String> {
        let preferred = request.preferred_task_type.as_deref().filter(|s| !s.is_empty());
        let task_type_str = preferred.unwrap_or_else(|| {
            let (task_type, _) = classify_task_type(&request.prompt);
            task_type.as_str()
        });
        if preferred.is_some() {
            info!(task_type = task_type_str, "Router using preferred task type (system/memory)");
        } else {
            info!(task_type = task_type_str, "Router classify");
        }

        let task_config = self
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get_route(task_type_str)
            .cloned()
            .unwrap_or_else(|| {
                crate::config::TaskTypeConfig {
                    primary: Some(crate::config::RouteEntry {
                        provider: "akasha_embedded".into(),
                        model: "default".into(),
                        config: None,
                    }),
                    fallback: vec![crate::config::RouteEntry {
                        provider: "akasha_core".into(),
                        model: "core".into(),
                        config: None,
                    }],
                    constraints: None,
                }
            });

        let resolve = self.resolve();
        self.fallback
            .complete(
                request,
                &task_config,
                &resolve,
                self.metrics.as_ref(),
                self.degraded_mode,
            )
            .await
    }

    /// Complete with streaming: chunks are sent to `chunk_tx`. Uses preferred_task_type if set, else classifier.
    pub async fn complete_stream(
        &self,
        request: &CompletionRequest,
        chunk_tx: std::sync::mpsc::Sender<String>,
    ) -> Result<CompletionResponse, String> {
        let task_type_str = request
            .preferred_task_type
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                let (task_type, _) = classify_task_type(&request.prompt);
                task_type.as_str()
            });
        info!(task_type = task_type_str, "Router classify (stream)");

        let task_config = self
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get_route(task_type_str)
            .cloned()
            .unwrap_or_else(|| {
                crate::config::TaskTypeConfig {
                    primary: Some(crate::config::RouteEntry {
                        provider: "akasha_embedded".into(),
                        model: "default".into(),
                        config: None,
                    }),
                    fallback: vec![crate::config::RouteEntry {
                        provider: "akasha_core".into(),
                        model: "core".into(),
                        config: None,
                    }],
                    constraints: None,
                }
            });

        let resolve = self.resolve();
        let primary_entry = task_config.primary.as_ref();
        if let Some(entry) = primary_entry {
            if let Some(provider) = resolve(entry.provider.as_str()) {
                if provider.supports_streaming() {
                    let timeout = self
                        .config
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .global
                        .default_timeout_secs
                        .unwrap_or(300);
                    let timeout = std::time::Duration::from_secs(timeout);
                    return provider
                        .complete_stream(
                            request,
                            timeout,
                            Some(&entry.model),
                            chunk_tx,
                        )
                        .await
                        .map_err(|e| e.to_string());
                }
            }
        }

        // Primary does not support streaming: complete then send full text once
        let response = self.complete(request).await?;
        let _ = chunk_tx.send(response.text.clone());
        Ok(response)
    }
}
