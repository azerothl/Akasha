//! Fallback engine — try primary then chain, retry policy, log switches.

use crate::config::{RouteEntry, TaskTypeConfig};
use crate::metrics::MetricsCollector;
use crate::provider::{CompletionRequest, CompletionResponse, LLMProvider, ProviderError};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::warn;

pub struct FallbackEngine {
    pub max_retries: u32,
    pub timeout_per_call: Duration,
}

impl Default for FallbackEngine {
    fn default() -> Self {
        Self {
            max_retries: 2,
            timeout_per_call: Duration::from_secs(300),
        }
    }
}

/// Resolve provider by name to a dyn LLMProvider. Caller passes a registry.
pub type ProviderResolver = Arc<dyn Fn(&str) -> Option<Arc<dyn LLMProvider>> + Send + Sync>;

impl FallbackEngine {
    pub async fn complete(
        &self,
        request: &CompletionRequest,
        task_config: &TaskTypeConfig,
        resolve: &ProviderResolver,
        metrics: &MetricsCollector,
        degraded_only: bool,
    ) -> Result<CompletionResponse, String> {
        let mut chain: Vec<&RouteEntry> = vec![];
        if let Some(ref p) = task_config.primary {
            chain.push(p);
        }
        chain.extend(task_config.fallback.iter());

        let mut last_error: Option<String> = None;
        for (i, entry) in chain.iter().enumerate() {
            if degraded_only && !self.is_local_provider(entry.provider.as_str(), resolve) {
                continue;
            }
            let provider = match resolve(entry.provider.as_str()) {
                Some(p) => p,
                None => {
                    warn!(provider = %entry.provider, "Provider not found, skip");
                    last_error = Some(format!("{}: provider not registered", entry.provider));
                    continue;
                }
            };
            let mut disable_thinking_after_empty = false;
            // Do not call provider.is_available() here: OllamaProvider uses blocking reqwest
            // which would block the async runtime and cause "response ended prematurely".
            for attempt in 0..self.max_retries {
                let start = Instant::now();
                // Clone request and apply model-specific config from routing entry.
                let mut request_with_config = request.clone();
                entry.apply_config_to_request(&mut request_with_config);
                if disable_thinking_after_empty {
                    request_with_config.thinking_level = Some("off".to_string());
                }
                match provider.complete(&request_with_config, self.timeout_per_call, Some(&entry.model)).await {
                    Ok(resp) => {
                        // Treat empty/whitespace-only responses as failure so we can retry/fallback.
                        // This happens in practice when a model is still loading or returns an empty completion.
                        if resp.text.trim().is_empty() {
                            let had_thinking = resp
                                .thinking
                                .as_ref()
                                .map(|t| !t.trim().is_empty())
                                .unwrap_or(false);
                            let thinking_was_on = request_with_config
                                .thinking_level
                                .as_deref()
                                .map(|s| !s.eq_ignore_ascii_case("off"))
                                .unwrap_or(false);

                            metrics.record_failure(entry.provider.as_str(), &entry.model);
                            if i > 0 {
                                metrics.record_fallback_triggered(entry.provider.as_str(), &entry.model);
                            }
                            warn!(
                                provider = %entry.provider,
                                model = %entry.model,
                                attempt = attempt + 1,
                                "Provider returned empty text"
                            );
                            last_error = Some(format!("{}: empty response", entry.provider));

                            // Some reasoning-capable models may emit long `thinking` but empty final text.
                            // Retry once with thinking disabled before moving to next provider.
                            if had_thinking && thinking_was_on {
                                disable_thinking_after_empty = true;
                                warn!(
                                    provider = %entry.provider,
                                    model = %entry.model,
                                    attempt = attempt + 1,
                                    "Empty response with non-empty thinking; retrying with thinking disabled"
                                );
                            }

                            if attempt + 1 < self.max_retries {
                                let backoff_secs = (1u64 << attempt).min(16);
                                tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                                continue;
                            }
                            // Give next provider in chain a chance.
                            break;
                        }

                        // Log detailed response metadata for performance analysis and debugging
                        {
                            let max_tokens_used = request_with_config.max_tokens;
                            let thinking_len = resp.thinking.as_ref().map(|t| t.len()).unwrap_or(0);
                            let text_len = resp.text.len();
                            let is_truncated = resp.done_reason.as_deref() == Some("length");
                            let eval_count = resp.eval_count.unwrap_or(0);

                            if is_truncated {
                                tracing::warn!(
                                    provider = %entry.provider,
                                    model = %entry.model,
                                    max_tokens = ?max_tokens_used,
                                    done_reason = %resp.done_reason.as_deref().unwrap_or("N/A"),
                                    text_length = text_len,
                                    thinking_length = thinking_len,
                                    eval_count = eval_count,
                                    "Model response truncated due to max_tokens limit; response may be incomplete"
                                );
                            } else if text_len < 50 {
                                tracing::warn!(
                                    provider = %entry.provider,
                                    model = %entry.model,
                                    max_tokens = ?max_tokens_used,
                                    done_reason = %resp.done_reason.as_deref().unwrap_or("stop"),
                                    text_length = text_len,
                                    thinking_length = thinking_len,
                                    eval_count = eval_count,
                                    "Short response from model (may indicate issues)"
                                );
                            } else {
                                tracing::debug!(
                                    provider = %entry.provider,
                                    model = %entry.model,
                                    max_tokens = ?max_tokens_used,
                                    done_reason = %resp.done_reason.as_deref().unwrap_or("stop"),
                                    text_length = text_len,
                                    thinking_length = thinking_len,
                                    eval_count = eval_count,
                                    "Model response completed successfully"
                                );
                            }
                        }

                        let latency_ms = start.elapsed().as_millis() as u64;
                        let tokens = resp.usage.as_ref().map(|u| u.prompt_tokens + u.completion_tokens).unwrap_or(0);
                        let cost = provider.get_cost(resp.usage.as_ref().unwrap_or(&crate::provider::TokenUsage {
                            prompt_tokens: 0,
                            completion_tokens: 0,
                        }));
                        if i > 0 {
                            metrics.record_success_with_fallback(
                                entry.provider.as_str(),
                                &entry.model,
                                latency_ms,
                                tokens,
                                cost,
                                true,
                                true,
                            );
                        } else {
                            metrics.record_success(entry.provider.as_str(), &entry.model, latency_ms, tokens, cost);
                        }
                        let mut out = resp;
                        out.cost_usd = Some(cost);
                        return Ok(out);
                    }
                    Err(e) => {
                        if i > 0 {
                            metrics.record_failure_with_fallback(entry.provider.as_str(), &entry.model, true);
                        } else {
                            metrics.record_failure(entry.provider.as_str(), &entry.model);
                        }
                        last_error = Some(format!("{}: {}", entry.provider, e));
                        let retry = matches!(e, ProviderError::Timeout | ProviderError::RateLimit);
                        if retry && attempt + 1 < self.max_retries {
                            let backoff_secs = (1u64 << attempt).min(16);
                            tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                            continue;
                        }
                        warn!(provider = %entry.provider, error = %e, "Attempt failed, try next in chain");
                    }
                }
            }
        }
        let msg = match last_error {
            Some(e) => format!("All providers in fallback chain failed (last: {}).", e),
            None => "All providers in fallback chain failed.".to_string(),
        };
        Err(msg)
    }

    fn is_local_provider(&self, name: &str, resolve: &ProviderResolver) -> bool {
        resolve(name).map(|p| p.is_local()).unwrap_or(false)
    }
}
