//! LLM Router — classifier + config + fallback engine + provider registry.

use crate::classifier::classify_task_type;
use crate::config::{prepare_ollama_request, RoutingConfig, TaskTypeConfig};
use crate::fallback::{FallbackEngine, ProviderResolver};
use crate::metrics::{MetricsCollector, MetricsPersistence};
use crate::provider::{
    provider_error_is_context_window_exceeded, CompletionRequest, CompletionResponse, LLMProvider,
};
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
    resolve_task_type_with_fallback_chain(primary, fallbacks, config)
}

/// Fallback chain for explicit `preferred_task_type` (API routes, compare synthesis, etc.).
fn preferred_task_type_fallbacks(task_type: &str) -> &'static [&'static str] {
    match task_type {
        "research" => &["scientific_analysis", "conversation"],
        _ => &[],
    }
}

fn resolve_task_type_with_fallback_chain(
    primary: &str,
    fallbacks: &[&str],
    config: &RoutingConfig,
) -> String {
    if let Some(route) = config.get_route(primary) {
        if is_custom_route(route) {
            return primary.to_string();
        }
    }
    for &fallback in fallbacks {
        if let Some(route) = config.get_route(fallback) {
            if is_custom_route(route) {
                if fallback != primary {
                    info!(
                        requested_task_type = primary,
                        resolved_task_type = fallback,
                        "Router task_type fallback"
                    );
                }
                return fallback.to_string();
            }
        }
    }
    primary.to_string()
}

fn resolve_preferred_task_type(preferred: &str, config: &RoutingConfig) -> String {
    let fallbacks = preferred_task_type_fallbacks(preferred);
    resolve_task_type_with_fallback_chain(preferred, fallbacks, config)
}

/// When `enable_fallback` is false, only the primary route is attempted.
fn task_config_for_completion(task_config: &TaskTypeConfig, enable_fallback: bool) -> TaskTypeConfig {
    if enable_fallback {
        return task_config.clone();
    }
    let mut cfg = task_config.clone();
    cfg.fallback.clear();
    cfg
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

    /// Get the global configuration settings (timeout, retries, metrics, fallback).
    pub fn global_config(&self) -> crate::config::GlobalConfig {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .global
            .clone()
    }

    /// List models per provider from routing config (for GET /api/router/models).
    pub fn provider_configs(&self) -> std::collections::HashMap<String, crate::config::ProviderConfig> {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .providers
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

    /// Resolve explicit preferred task type with category fallbacks (e.g. research → scientific_analysis → conversation).
    pub fn resolve_preferred_task_type(&self, preferred: &str) -> String {
        let config = self.config.read().unwrap_or_else(|e| e.into_inner());
        resolve_preferred_task_type(preferred, &config)
    }

    /// Primary route `(provider, model)` for a task type from routing config (for token estimates, metrics labels).
    pub fn primary_route_for_task_type(&self, task_type: &str) -> Option<(String, String)> {
        let config = self.config.read().unwrap_or_else(|e| e.into_inner());
        config
            .get_route(task_type)
            .and_then(|c| c.primary.as_ref())
            .map(|p| (p.provider.clone(), p.model.clone()))
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

    /// Status snapshot for `/api/router/embedded-status`.
    #[cfg(feature = "embedded")]
    pub fn embedded_status(&self) -> akasha_embedded_llm::EmbeddedStatus {
        akasha_embedded_llm::EmbeddedLlm::status_snapshot()
    }

    /// Camelid-style evidence-gated capabilities for `/api/capabilities`.
    #[cfg(feature = "embedded")]
    pub fn embedded_capabilities(&self) -> serde_json::Value {
        let _ = self;
        akasha_embedded_llm::capabilities::build_capabilities()
    }

    /// List models from embedded_models.json manifest.
    #[cfg(all(feature = "embedded", feature = "embedded-download"))]
    pub fn embedded_models_manifest(
        &self,
    ) -> Result<akasha_embedded_llm::download::Manifest, String> {
        akasha_embedded_llm::download::load_manifest()
    }

    /// Start GGUF download on a background thread.
    #[cfg(all(feature = "embedded", feature = "embedded-download"))]
    pub fn embedded_start_download(&self, model_id: Option<String>) -> Result<(), String> {
        let _ = self;
        akasha_embedded_llm::download::start_download_background(model_id)
    }

    /// Poll download progress.
    #[cfg(all(feature = "embedded", feature = "embedded-download"))]
    pub fn embedded_download_status(
        &self,
    ) -> akasha_embedded_llm::download::DownloadProgress {
        let _ = self;
        akasha_embedded_llm::download::download_progress_snapshot()
    }

    /// Hardware profile + static calibration candidates.
    #[cfg(feature = "embedded")]
    pub fn embedded_hardware(&self) -> Result<serde_json::Value, String> {
        let _ = self;
        let profile = akasha_embedded_llm::hardware::detect_hardware();
        let candidates =
            akasha_embedded_llm::profiles::calibration_candidates(&profile, 3).unwrap_or_default();
        let models_for_tier = akasha_embedded_llm::profiles::models_for_tier(&profile.tier_id)
            .unwrap_or_default();
        Ok(serde_json::json!({
            "profile": profile,
            "static_candidates": candidates,
            "models_for_tier": models_for_tier,
        }))
    }

    /// Start embedded micro-bench calibration (background thread).
    #[cfg(all(feature = "embedded", feature = "embedded-download", feature = "embedded-llama-cpp"))]
    pub fn embedded_start_calibrate(&self, max_configs: usize) -> Result<(), String> {
        let _ = self;
        akasha_embedded_llm::calibrate::start_calibration_background(max_configs)
    }

    /// Poll calibration progress.
    #[cfg(all(feature = "embedded", feature = "embedded-download", feature = "embedded-llama-cpp"))]
    pub fn embedded_calibrate_status(
        &self,
    ) -> akasha_embedded_llm::calibrate::CalibrateProgress {
        let _ = self;
        akasha_embedded_llm::calibrate::calibrate_progress_snapshot()
    }

    /// Apply persisted runtime config (call at daemon startup).
    #[cfg(feature = "embedded")]
    pub fn embedded_apply_runtime(&self) {
        let _ = self;
        akasha_embedded_llm::config::apply_persisted_runtime();
    }

    /// Runtime settings snapshot (model, engine mode, tier).
    #[cfg(feature = "embedded")]
    pub fn embedded_settings_view(
        &self,
    ) -> Result<akasha_embedded_llm::runtime::EmbeddedSettingsView, String> {
        let _ = self;
        akasha_embedded_llm::runtime::settings_view()
    }

    /// Persist manual model + engine mode and unload for reload.
    #[cfg(all(feature = "embedded", feature = "embedded-download"))]
    pub fn embedded_set_runtime(
        &self,
        model_id: &str,
        engine_mode: &str,
    ) -> Result<akasha_embedded_llm::runtime::EmbeddedRuntime, String> {
        let _ = self;
        akasha_embedded_llm::runtime::apply_manual_runtime(model_id, engine_mode)
    }

    /// Replace in-memory routing config (task_types, providers metadata, global) from disk or API reload.
    /// Registered provider clients (Ollama, OpenRouter, etc.) are unchanged — route/model switches take effect immediately.
    pub fn reload_routing_config(&self, config: RoutingConfig) {
        match self.config.write() {
            Ok(mut cfg) => *cfg = config,
            Err(poisoned) => *poisoned.into_inner() = config,
        }
    }

    /// Complete using a single route entry — no fallback chain (model compare slots).
    pub async fn complete_for_entry(
        &self,
        request: &CompletionRequest,
        entry: &crate::config::RouteEntry,
    ) -> Result<CompletionResponse, String> {
        let resolve = self.resolve();
        let provider = resolve(entry.provider.as_str()).ok_or_else(|| {
            format!(
                "provider '{}' is not registered",
                entry.provider
            )
        })?;
        if self.degraded_mode && !provider.is_local() {
            return Err(format!(
                "degraded mode: provider '{}' unavailable",
                entry.provider
            ));
        }
        let timeout_secs = self
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .global
            .default_timeout_secs
            .unwrap_or(300);
        let timeout = std::time::Duration::from_secs(timeout_secs);
        let mut req = request.clone();
        let model_options = self
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .model_options
            .clone();
        prepare_ollama_request(entry, &mut req, &model_options);
        provider
            .complete(&req, timeout, Some(entry.model.as_str()))
            .await
            .map_err(|e| e.to_string())
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

    /// Append provider/model to fallback chain for a task type. Applied immediately.
    pub fn add_fallback_route(&self, task_type: &str, entry: crate::config::RouteEntry) {
        match self.config.write() {
            Ok(mut cfg) => {
                cfg.add_fallback_route(task_type, entry);
            }
            Err(poisoned) => {
                let mut cfg = poisoned.into_inner();
                cfg.add_fallback_route(task_type, entry);
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
        let task_type_str = if let Some(p) = preferred {
            let config = self.config.read().unwrap_or_else(|e| e.into_inner());
            resolve_preferred_task_type(p, &config)
        } else {
            let (task_type, _) = classify_task_type(&request.prompt);
            task_type.as_str().to_string()
        };
        let task_type_str = task_type_str.as_str();
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

        let enable_fallback = self
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .global
            .enable_fallback
            .unwrap_or(true);
        let task_config = task_config_for_completion(&task_config, enable_fallback);

        let resolve = self.resolve();
        let model_options = self
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .model_options
            .clone();
        self.fallback
            .complete(
                request,
                &task_config,
                &resolve,
                self.metrics.as_ref(),
                self.degraded_mode,
                &model_options,
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
        let preferred = request.preferred_task_type.as_deref().filter(|s| !s.is_empty());
        let task_type_str = if let Some(p) = preferred {
            let config = self.config.read().unwrap_or_else(|e| e.into_inner());
            resolve_preferred_task_type(p, &config)
        } else {
            let (task_type, _) = classify_task_type(&request.prompt);
            task_type.as_str().to_string()
        };
        let task_type_str = task_type_str.as_str();
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

        let enable_fallback = self
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .global
            .enable_fallback
            .unwrap_or(true);
        let task_config = task_config_for_completion(&task_config, enable_fallback);

        let resolve = self.resolve();
        let model_options = self
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .model_options
            .clone();
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
                    // Apply route config + Ollama num_ctx (stream path previously skipped this).
                    let mut stream_req = request.clone();
                    prepare_ollama_request(entry, &mut stream_req, &model_options);
                    // Use a proxy channel to detect whether streaming emitted any chunks before a failure.
                    // This prevents sending the full fallback text on top of already-streamed partial content.
                    let chunk_tx_fallback = chunk_tx.clone();
                    let (proxy_tx, proxy_rx) = std::sync::mpsc::channel::<String>();
                    let any_chunk_sent =
                        Arc::new(std::sync::atomic::AtomicBool::new(false));
                    let flag_bridge = any_chunk_sent.clone();
                    let real_tx = chunk_tx;
                    let bridge_handle = tokio::task::spawn_blocking(move || {
                        for chunk in proxy_rx {
                            flag_bridge.store(true, std::sync::atomic::Ordering::Relaxed);
                            let _ = real_tx.send(chunk);
                        }
                    });
                    match provider
                        .complete_stream(
                            &stream_req,
                            timeout,
                            Some(&entry.model),
                            proxy_tx,
                        )
                        .await
                    {
                        Ok(resp) => {
                            // proxy_tx was moved into complete_stream and is now dropped.
                            // Await the bridge so all queued chunks are forwarded before returning.
                            let _ = bridge_handle.await;
                            return Ok(resp);
                        }
                        Err(e) => {
                            if provider_error_is_context_window_exceeded(&e) {
                                warn!(
                                    error = %e,
                                    provider = %entry.provider,
                                    model = %entry.model,
                                    "Streaming failed: prompt exceeds this model's context window; falling back to non-streaming chain which may include larger-context providers."
                                );
                            } else {
                                warn!(
                                    error = %e,
                                    provider = %entry.provider,
                                    model = %entry.model,
                                    "Streaming failed; falling back to non-streaming completion (retries + fallback providers)"
                                );
                            }
                            // Wait for the bridge to drain any already-queued chunks before checking.
                            let _ = bridge_handle.await;
                            let response = self
                                .fallback
                                .complete(
                                    request,
                                    &task_config,
                                    &resolve,
                                    self.metrics.as_ref(),
                                    self.degraded_mode,
                                    &model_options,
                                )
                                .await?;
                            // Only forward the fallback as a chunk if streaming emitted nothing;
                            // otherwise partial chunks + full fallback would duplicate content.
                            if !any_chunk_sent.load(std::sync::atomic::Ordering::Relaxed) {
                                let _ = chunk_tx_fallback.send(response.text.clone());
                            }
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

    #[test]
    fn reload_routing_config_updates_primary_route() {
        let mut config = RoutingConfig::default_config();
        config.set_primary_route(
            "conversation",
            RouteEntry {
                provider: "akasha_embedded".into(),
                model: "default".into(),
                config: None,
            },
        );
        let router = LLMRouter::new(config);
        let mut new_config = RoutingConfig::default_config();
        new_config.set_primary_route(
            "conversation",
            RouteEntry {
                provider: "openrouter".into(),
                model: "qwen/test".into(),
                config: None,
            },
        );
        router.reload_routing_config(new_config);
        let route = router.primary_route_for_task_type("conversation");
        assert_eq!(
            route,
            Some(("openrouter".to_string(), "qwen/test".to_string()))
        );
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
    fn resolve_preferred_task_type_research_falls_back_to_scientific_analysis() {
        let mut config = RoutingConfig::default_config();
        config.task_types.insert(
            "scientific_analysis".into(),
            crate::config::TaskTypeConfig {
                primary: Some(RouteEntry {
                    provider: "openai".into(),
                    model: "gpt-4".into(),
                    config: None,
                }),
                fallback: vec![],
                constraints: None,
            },
        );
        assert_eq!(
            resolve_preferred_task_type("research", &config),
            "scientific_analysis"
        );
    }

    #[test]
    fn resolve_preferred_task_type_research_uses_explicit_research_route() {
        let mut config = RoutingConfig::default_config();
        config.task_types.insert(
            "research".into(),
            crate::config::TaskTypeConfig {
                primary: Some(RouteEntry {
                    provider: "ollama".into(),
                    model: "llama3.2".into(),
                    config: None,
                }),
                fallback: vec![],
                constraints: None,
            },
        );
        assert_eq!(resolve_preferred_task_type("research", &config), "research");
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
