//! LLM Router — classifier + config + fallback engine + provider registry.

use crate::classifier::classify_task_type;
use crate::config::{RoutingConfig, TaskTypeConfig};
use crate::fallback::{FallbackEngine, ProviderResolver};
use crate::metrics::{MetricsCollector, MetricsPersistence};
use crate::provider::{CompletionRequest, CompletionResponse, LLMProvider};
use crate::retry::RetryPolicy;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tracing::{info, warn, Instrument};

fn is_custom_route(config: &TaskTypeConfig) -> bool {
    config
        .primary
        .as_ref()
        .map(|p| {
            p.provider != "akasha_embedded" && p.provider != "akasha_core"
        })
        .unwrap_or(false)
}

/// Agent type -> (primary task_type, fallback task_types for routing when primary has no custom route).
/// Plan: Architecture agents et pipeline — Phase 2 (new roles).
fn agent_task_type_and_fallbacks(agent: &str) -> (&'static str, &'static [&'static str]) {
    match agent {
        "financial" => ("financial", &["data_analysis", "conversation"][..]),
        "documentalist" => ("documentalist", &["conversation"][..]),
        "project_manager" => ("project_manager", &["conversation"][..]),
        "technical_writer" => ("technical_writer", &["creative_writing", "conversation"][..]),
        "research" => ("research", &["scientific_analysis", "conversation"][..]),
        "security_audit" => (
            "security_audit",
            &["code_generation", "system_diagnostic", "conversation"][..],
        ),
        "creative" => ("creative", &["creative_writing", "conversation"][..]),
        "code" => ("code_generation", &["conversation"][..]),
        "search" => ("conversation", &[]),
        "conversation" => ("conversation", &[]),
        "analyst" => ("conversation", &["creative_writing"][..]),
        "architect" => ("code_generation", &["system_diagnostic", "conversation"][..]),
        "frontend" | "backend" | "integration" => ("code_generation", &["conversation"][..]),
        "database" => ("data_analysis", &["code_generation", "conversation"][..]),
        "qa" => ("system_diagnostic", &["code_generation", "conversation"][..]),
        "system" => ("system_diagnostic", &["conversation"][..]),
        "image_generation" => ("image_generation", &["creative_writing", "conversation"][..]),
        _ => ("conversation", &[]),
    }
}

fn resolve_task_type_for_agent_impl(assigned_agent: &str, config: &RoutingConfig) -> String {
    let (primary, fallbacks) = agent_task_type_and_fallbacks(assigned_agent);
    if let Some(route) = config.get_route(primary) {
        if is_custom_route(route) {
            return primary.to_string();
        }
    }
    for &fallback in fallbacks {
        if let Some(route) = config.get_route(fallback) {
            if is_custom_route(route) {
                return fallback.to_string();
            }
        }
    }
    primary.to_string()
}

pub struct LLMRouter {
    config: Arc<RwLock<RoutingConfig>>,
    fallback: FallbackEngine,
    metrics: Arc<MetricsCollector>,
    providers: HashMap<String, Arc<dyn LLMProvider>>,
    degraded_mode: bool,
}

impl LLMRouter {
    pub fn new(config: RoutingConfig) -> Self {
        Self::new_with_persistence(config, None)
    }

    pub fn new_with_persistence(config: RoutingConfig, persistence: Option<Arc<dyn MetricsPersistence>>) -> Self {
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
            retry_policy: RetryPolicy {
                max_retries,
                ..RetryPolicy::default()
            },
        };
        let metrics = match persistence {
            Some(p) => Arc::new(MetricsCollector::with_persistence(p)),
            None => Arc::new(MetricsCollector::new()),
        };
        Self {
            config: Arc::new(RwLock::new(config)),
            fallback,
            metrics,
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

    /// Global per-call timeout (seconds) from routing config.
    /// Priority: env AKASHA_LLM_TIMEOUT_SECS -> llm_router.yaml global.default_timeout_secs -> 300s
    pub fn default_timeout_secs(&self) -> u64 {
        if let Ok(env_val) = std::env::var("AKASHA_LLM_TIMEOUT_SECS") {
            if let Ok(secs) = env_val.parse::<u64>() {
                return secs;
            }
        }
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .global
            .default_timeout_secs
            .unwrap_or(300)
    }

    /// List models per provider from routing config (for GET /api/router/models).
    /// Get the global configuration settings (timeout, retries, metrics, fallback).
    pub fn global_config(&self) -> crate::config::GlobalConfig {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .global
            .clone()
    }

    /// List models per provider from routing config (for GET /api/router/models).
    pub fn list_models_from_config(&self) -> std::collections::HashMap<String, Vec<String>> {
        self.config.read().unwrap_or_else(|e| e.into_inner()).list_models_by_provider()
    }

    /// Routes by category (primary + fallback) for GET /api/router/routes and CLI/TUI.
    pub fn routes_by_category(&self) -> HashMap<String, TaskTypeConfig> {
        self.config.read().unwrap_or_else(|e| e.into_inner()).task_types.clone()
    }

    /// Resolve which task_type to use for routing given an assigned_agent. If the agent's task_type
    /// has no custom route, tries similar task_types in order until one has a custom route.
    pub fn resolve_task_type_for_agent(&self, assigned_agent: &str) -> String {
        let config = self.config.read().unwrap_or_else(|e| e.into_inner());
        resolve_task_type_for_agent_impl(assigned_agent, &config)
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

    /// Returns true when the primary provider for `task_type` is Ollama.
    /// Used to inflate per-operation timeouts and account for Ollama model loading time.
    pub fn is_ollama_primary(&self, task_type: &str) -> bool {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get_route(task_type)
            .and_then(|c| c.primary.as_ref())
            .map(|p| p.provider == "ollama")
            .unwrap_or(false)
    }

    /// Returns true when Ollama is registered as a provider (regardless of which task types use it).
    /// Used as a broadbrush signal that Ollama model cold-start latency must be accounted for.
    pub fn is_ollama_registered(&self) -> bool {
        self.providers.contains_key("ollama")
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
        let span = tracing::info_span!("llm_call", task_type = task_type_str);
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
            .instrument(span)
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
                    // Keep a second handle: on stream failure (e.g. rate limit) we still need to push
                    // the non-streaming fallback response to the same consumer.
                    let chunk_tx_fallback = chunk_tx.clone();
                    match provider
                        .complete_stream(
                            request,
                            timeout,
                            Some(&entry.model),
                            chunk_tx,
                        )
                        .await
                    {
                        Ok(resp) => return Ok(resp),
                        Err(e) => {
                            warn!(
                                error = %e,
                                provider = %entry.provider,
                                model = %entry.model,
                                "Streaming failed; falling back to non-streaming completion (retries + fallback providers)"
                            );
                            let response = self
                                .fallback
                                .complete(
                                    request,
                                    &task_config,
                                    &resolve,
                                    self.metrics.as_ref(),
                                    self.degraded_mode,
                                )
                                .await?;
                            let _ = chunk_tx_fallback.send(response.text.clone());
                            return Ok(response);
                        }
                    }
                }
            }
        }

        // Primary does not support streaming: complete then send full text once
        let response = self.complete(request).await?;
        let _ = chunk_tx.send(response.text.clone());
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RouteEntry;
    use crate::provider::{ProviderError, TokenUsage};
    use async_trait::async_trait;
    use std::time::Duration;

    /// Mock local provider for tests: returns a fixed response without calling any real model.
    struct MockLocalProvider {
        response_text: String,
    }

    #[async_trait]
    impl LLMProvider for MockLocalProvider {
        fn name(&self) -> &str {
            "mock_local"
        }
        fn is_available(&self) -> bool {
            true
        }
        fn is_local(&self) -> bool {
            true
        }
        async fn complete(
            &self,
            _request: &crate::provider::CompletionRequest,
            _timeout: Duration,
            _model_override: Option<&str>,
        ) -> Result<crate::provider::CompletionResponse, ProviderError> {
            Ok(crate::provider::CompletionResponse {
                text: self.response_text.clone(),
                usage: Some(TokenUsage {
                    prompt_tokens: 0,
                    completion_tokens: 1,
                }),
                model_used: "mock".into(),
                cost_usd: None,
                thinking: None,
                done_reason: None,
                eval_count: None,
                total_duration_ns: None,
            })
        }
    }

    #[tokio::test]
    async fn router_complete_uses_registered_local_provider() {
        let config = RoutingConfig::default_config();
        let mut router = LLMRouter::new(config);
        let mock_text = "mock local model reply";
        router.register_provider(Arc::new(MockLocalProvider {
            response_text: mock_text.to_string(),
        }));
        router.set_primary_route(
            "conversation",
            RouteEntry {
                provider: "mock_local".into(),
                model: "default".into(),
                config: None,
            },
        );
        let request = CompletionRequest {
            prompt: "Hello".into(),
            max_tokens: Some(10),
            temperature: Some(0.0),
            preferred_task_type: Some("conversation".into()),
            system_prompt: None,
            image_data_urls: None,
            top_p: None,
            top_k: None,
            frequency_penalty: None,
            presence_penalty: None,
            repeat_penalty: None,
            num_ctx: None,
            num_gpu: None,
            thinking_level: None,
        };
        let response = router.complete(&request).await.expect("complete should succeed");
        assert_eq!(response.text, mock_text);
        assert_eq!(response.model_used, "mock");
    }

    #[test]
    fn router_with_embedded_registered_reports_embedded_available() {
        let config = RoutingConfig::default_config();
        let mut router = LLMRouter::new(config);
        router.register_provider(Arc::new(crate::provider::AkashaEmbeddedProvider::new()));
        #[cfg(feature = "embedded")]
        assert_eq!(
            router.embedded_available(),
            akasha_embedded_llm::EmbeddedLlm::is_available(),
        );
        #[cfg(not(feature = "embedded"))]
        assert!(!router.embedded_available());
    }

    #[test]
    fn router_resolve_task_type_conversation_returns_conversation() {
        let config = RoutingConfig::default_config();
        let router = LLMRouter::new(config);
        assert_eq!(router.resolve_task_type_for_agent("conversation"), "conversation");
    }

    #[test]
    fn router_routes_by_category_contains_orchestrator_when_configured() {
        // The decomposer uses routes_by_category() to check for an "orchestrator" route
        // and sets preferred_task_type: "orchestrator" when found (0.7.0 feature).
        let mut config = RoutingConfig::default_config();
        config.task_types.insert(
            "orchestrator".into(),
            crate::config::TaskTypeConfig {
                primary: Some(RouteEntry {
                    provider: "openai".into(),
                    model: "gpt-4o-mini".into(),
                    config: None,
                }),
                fallback: vec![],
                constraints: None,
            },
        );
        let router = LLMRouter::new(config);
        let routes = router.routes_by_category();
        assert!(
            routes.contains_key("orchestrator"),
            "routes_by_category must expose the 'orchestrator' route when it is configured"
        );
    }

    #[test]
    fn router_routes_by_category_no_orchestrator_in_default_config() {
        // Without an explicit "orchestrator" task_type, the decomposer falls back to
        // "system" for backward compatibility (pre-0.7.0 llm_router.yaml).
        let config = RoutingConfig::default_config();
        let router = LLMRouter::new(config);
        let routes = router.routes_by_category();
        assert!(
            !routes.contains_key("orchestrator"),
            "default config must NOT have an 'orchestrator' route; decomposer falls back to 'system'"
        );
    }
}
