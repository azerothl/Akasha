//! Metrics collector — latency, cost, errors, fallback count (in-memory v1).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::RwLock;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
}

pub struct MetricsCollector {
    by_provider_model: RwLock<HashMap<String, ModelMetrics>>,
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
        }
    }

    fn key(provider: &str, model: &str) -> String {
        format!("{}::{}", provider, model)
    }

    pub fn record_success(&self, provider: &str, model: &str, latency_ms: u64, tokens: u64, cost: f64) {
        let mut g = self.by_provider_model.write().unwrap();
        let m = g.entry(Self::key(provider, model)).or_default();
        m.total_requests += 1;
        m.successful_requests += 1;
        m.total_latency_ms += latency_ms;
        m.total_tokens += tokens;
        m.total_cost_usd += cost;
        m.last_success = Some(Utc::now());
    }

    pub fn record_failure(&self, provider: &str, model: &str) {
        let mut g = self.by_provider_model.write().unwrap();
        let m = g.entry(Self::key(provider, model)).or_default();
        m.total_requests += 1;
        m.failed_requests += 1;
        m.last_failure = Some(Utc::now());
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
        g.clone()
    }
}
