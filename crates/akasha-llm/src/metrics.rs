//! Metrics collector — latency, cost, errors, fallback count (in-memory + optional persistence).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::collections::VecDeque;

const LATENCY_SAMPLE_CAP: usize = 1000;

/// Optional persistence for metrics (e.g. SQLite). Implemented by the daemon.
pub trait MetricsPersistence: Send + Sync {
    fn record_event(
        &self,
        at: DateTime<Utc>,
        provider: &str,
        model: &str,
        success: bool,
        latency_ms: u64,
        tokens: u64,
        cost_usd: f64,
        fallback_triggered: bool,
        fallback_success: bool,
    );
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelMetrics {
    pub total_requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub total_latency_ms: u64,
    pub total_tokens: u64,
    pub total_cost_usd: f64,
    pub fallback_triggered: u64,
    pub fallback_success: u64,
    pub last_success: Option<DateTime<Utc>>,
    pub last_failure: Option<DateTime<Utc>>,
    #[serde(skip)]
    pub latency_samples: VecDeque<u64>,
}

impl Default for ModelMetrics {
    fn default() -> Self {
        Self {
            total_requests: 0,
            successful_requests: 0,
            failed_requests: 0,
            total_latency_ms: 0,
            total_tokens: 0,
            total_cost_usd: 0.0,
            fallback_triggered: 0,
            fallback_success: 0,
            last_success: None,
            last_failure: None,
            latency_samples: VecDeque::new(),
        }
    }
}

pub struct MetricsCollector {
    by_provider_model: RwLock<HashMap<String, ModelMetrics>>,
    persistence: Option<Arc<dyn MetricsPersistence>>,
}

impl Default for MetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl MetricsCollector {
    pub fn new() -> Self {
        Self {
            by_provider_model: RwLock::new(HashMap::new()),
            persistence: None,
        }
    }

    pub fn with_persistence(persistence: Arc<dyn MetricsPersistence>) -> Self {
        Self {
            by_provider_model: RwLock::new(HashMap::new()),
            persistence: Some(persistence),
        }
    }

    fn key(provider: &str, model: &str) -> String {
        format!("{}::{}", provider, model)
    }

    pub fn record_success(&self, provider: &str, model: &str, latency_ms: u64, tokens: u64, cost: f64) {
        self.record_success_with_fallback(provider, model, latency_ms, tokens, cost, false, false);
    }

    pub fn record_success_with_fallback(
        &self,
        provider: &str,
        model: &str,
        latency_ms: u64,
        tokens: u64,
        cost: f64,
        fallback_triggered: bool,
        fallback_success: bool,
    ) {
        let now = Utc::now();
        if let Some(ref p) = self.persistence {
            p.record_event(now, provider, model, true, latency_ms, tokens, cost, fallback_triggered, fallback_success);
        }
        let mut g = self.by_provider_model.write().unwrap();
        let m = g.entry(Self::key(provider, model)).or_default();
        m.total_requests += 1;
        m.successful_requests += 1;
        m.total_latency_ms += latency_ms;
        m.total_tokens += tokens;
        m.total_cost_usd += cost;
        if fallback_triggered {
            m.fallback_triggered += 1;
        }
        if fallback_success {
            m.fallback_success += 1;
        }
        m.last_success = Some(now);
        m.latency_samples.push_back(latency_ms);
        if m.latency_samples.len() > LATENCY_SAMPLE_CAP {
            m.latency_samples.pop_front();
        }
    }

    pub fn record_failure(&self, provider: &str, model: &str) {
        self.record_failure_with_fallback(provider, model, false);
    }

    pub fn record_failure_with_fallback(&self, provider: &str, model: &str, fallback_triggered: bool) {
        let now = Utc::now();
        if let Some(ref p) = self.persistence {
            p.record_event(now, provider, model, false, 0, 0, 0.0, fallback_triggered, false);
        }
        let mut g = self.by_provider_model.write().unwrap();
        let m = g.entry(Self::key(provider, model)).or_default();
        m.total_requests += 1;
        m.failed_requests += 1;
        if fallback_triggered {
            m.fallback_triggered += 1;
        }
        m.last_failure = Some(now);
    }

    pub fn record_fallback_triggered(&self, provider: &str, model: &str) {
        let mut g = self.by_provider_model.write().unwrap();
        let m = g.entry(Self::key(provider, model)).or_default();
        m.fallback_triggered += 1;
    }

    pub fn record_fallback_success(&self, provider: &str, model: &str) {
        let mut g = self.by_provider_model.write().unwrap();
        let m = g.entry(Self::key(provider, model)).or_default();
        m.fallback_success += 1;
    }

    pub fn get(&self, provider: &str, model: &str) -> ModelMetrics {
        let g = self.by_provider_model.read().unwrap();
        g.get(&Self::key(provider, model)).cloned().unwrap_or_default()
    }

    pub fn list(&self) -> HashMap<String, ModelMetrics> {
        let g = self.by_provider_model.read().unwrap();
        g.iter()
            .map(|(k, m)| {
                let mut out = m.clone();
                out.latency_samples = VecDeque::new();
                (k.clone(), out)
            })
            .collect()
    }

    /// Percentiles (P50, P95, P99) for latency per provider/model. Returns (p50, p95, p99) in ms or None if no samples.
    pub fn latency_percentiles(&self, provider: &str, model: &str) -> Option<(u64, u64, u64)> {
        let g = self.by_provider_model.read().unwrap();
        let m = g.get(&Self::key(provider, model))?;
        let mut samples: Vec<u64> = m.latency_samples.iter().copied().collect();
        if samples.is_empty() {
            return None;
        }
        samples.sort_unstable();
        let n = samples.len();
        let p50 = samples[((n as f64 * 0.50) as usize).min(n.saturating_sub(1))];
        let p95 = samples[((n as f64 * 0.95) as usize).min(n.saturating_sub(1))];
        let p99 = samples[((n as f64 * 0.99) as usize).min(n.saturating_sub(1))];
        Some((p50, p95, p99))
    }

    /// Summary for all provider/models: includes latency percentiles where available.
    pub fn summary(&self) -> HashMap<String, serde_json::Value> {
        let g = self.by_provider_model.read().unwrap();
        g.iter()
            .map(|(key, m)| {
                let mut samples: Vec<u64> = m.latency_samples.iter().copied().collect();
                samples.sort_unstable();
                let n = samples.len();
                let percentiles = if n > 0 {
                    Some((
                        samples[((n as f64 * 0.50) as usize).min(n.saturating_sub(1))],
                        samples[((n as f64 * 0.95) as usize).min(n.saturating_sub(1))],
                        samples[((n as f64 * 0.99) as usize).min(n.saturating_sub(1))],
                    ))
                } else {
                    None
                };
                let mut obj = serde_json::json!({
                    "total_requests": m.total_requests,
                    "successful_requests": m.successful_requests,
                    "failed_requests": m.failed_requests,
                    "total_latency_ms": m.total_latency_ms,
                    "total_tokens": m.total_tokens,
                    "total_cost_usd": m.total_cost_usd,
                    "fallback_triggered": m.fallback_triggered,
                    "fallback_success": m.fallback_success,
                });
                if let Some((p50, p95, p99)) = percentiles {
                    obj["latency_p50_ms"] = serde_json::json!(p50);
                    obj["latency_p95_ms"] = serde_json::json!(p95);
                    obj["latency_p99_ms"] = serde_json::json!(p99);
                }
                (key.clone(), obj)
            })
            .collect()
    }
}
