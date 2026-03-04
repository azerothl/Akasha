//! LLM Router — classifier + config + fallback engine + provider registry.

use crate::classifier::classify_task_type;
use crate::config::RoutingConfig;
use crate::fallback::{FallbackEngine, ProviderResolver};
use crate::metrics::MetricsCollector;
use crate::provider::{CompletionRequest, CompletionResponse, LLMProvider};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::info;

pub struct LLMRouter {
    config: Arc<RoutingConfig>,
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
            config: Arc::new(config),
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

    pub fn metrics(&self) -> Arc<MetricsCollector> {
        self.metrics.clone()
    }

    /// List models per provider from routing config (for GET /api/router/models).
    pub fn list_models_from_config(&self) -> std::collections::HashMap<String, Vec<String>> {
        self.config.list_models_by_provider()
    }

    /// Base URL of the Ollama provider from config (if set).
    pub fn ollama_base_url(&self) -> Option<String> {
        self.config
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

    fn resolve(&self) -> ProviderResolver {
        let providers = self.providers.clone();
        Arc::new(move |name: &str| providers.get(name).cloned())
    }

    /// Complete using classifier to get task type, then routing config and fallback.
    pub async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse, String> {
        let (task_type, _confidence) = classify_task_type(&request.prompt);
        let task_type_str = task_type.as_str();
        info!(task_type = task_type_str, "Router classify");

        let task_config = self
            .config
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
}
