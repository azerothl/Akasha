//! Metrics collector — latency, cost, errors, fallback count (in-memory + optional persistence).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::collections::VecDeque;

const LATENCY_SAMPLE_CAP: usize = 1000;
const EVENT_BUFFER_CAP: usize = 256;

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

/// Optional in-memory fanout sink for lightweight analytics events.
pub trait MetricsEventSink: Send + Sync {
    fn on_event(&self, event: &serde_json::Value);
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
    stability: RwLock<StabilityMetrics>,
    event_buffer: RwLock<VecDeque<serde_json::Value>>,
    event_sinks: RwLock<Vec<Arc<dyn MetricsEventSink>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StabilityMetrics {
    pub plan_runs: u64,
    pub plan_stability_score_sum: f64,
    pub retry_chain_depth_sum: u64,
    pub retry_chain_depth_max: u64,
    pub qa_gate_runs: u64,
    pub qa_gate_failures: u64,
    pub deterministic_replay_delta_sum: f64,
    pub deterministic_replay_delta_samples: u64,
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
            stability: RwLock::new(StabilityMetrics::default()),
            event_buffer: RwLock::new(VecDeque::new()),
            event_sinks: RwLock::new(Vec::new()),
        }
    }

    pub fn with_persistence(persistence: Arc<dyn MetricsPersistence>) -> Self {
        Self {
            by_provider_model: RwLock::new(HashMap::new()),
            persistence: Some(persistence),
            stability: RwLock::new(StabilityMetrics::default()),
            event_buffer: RwLock::new(VecDeque::new()),
            event_sinks: RwLock::new(Vec::new()),
        }
    }

    pub fn attach_event_sink(&self, sink: Arc<dyn MetricsEventSink>) {
        self.event_sinks.write().unwrap().push(sink);
        self.flush_event_buffer();
    }

    fn emit_event(&self, mut event: serde_json::Value) {
        // guardrail: strip protocol-level private fields if present
        if let Some(obj) = event.as_object_mut() {
            obj.retain(|k, _| !k.starts_with("_PROTO_"));
        }
        let sinks = self.event_sinks.read().unwrap().clone();
        if sinks.is_empty() {
            let mut b = self.event_buffer.write().unwrap();
            b.push_back(event);
            if b.len() > EVENT_BUFFER_CAP {
                b.pop_front();
            }
            return;
        }
        for sink in &sinks {
            sink.on_event(&event);
        }
    }

    fn flush_event_buffer(&self) {
        let sinks = self.event_sinks.read().unwrap().clone();
        if sinks.is_empty() {
            return;
        }
        let mut buffered = self.event_buffer.write().unwrap();
        while let Some(event) = buffered.pop_front() {
            for sink in &sinks {
                sink.on_event(&event);
            }
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
        self.emit_event(serde_json::json!({
            "kind": "llm_success",
            "provider": provider,
            "model": model,
            "latency_ms": latency_ms,
            "tokens": tokens,
            "cost_usd": cost,
            "fallback_triggered": fallback_triggered,
            "fallback_success": fallback_success
        }));
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
        self.emit_event(serde_json::json!({
            "kind": "llm_failure",
            "provider": provider,
            "model": model,
            "fallback_triggered": fallback_triggered
        }));
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

    pub fn record_plan_stability_score(&self, score: f64) {
        let mut s = self.stability.write().unwrap();
        s.plan_runs = s.plan_runs.saturating_add(1);
        s.plan_stability_score_sum += score.clamp(0.0, 1.0);
    }

    pub fn record_retry_chain_depth(&self, depth: u64) {
        let mut s = self.stability.write().unwrap();
        s.retry_chain_depth_sum = s.retry_chain_depth_sum.saturating_add(depth);
        s.retry_chain_depth_max = s.retry_chain_depth_max.max(depth);
    }

    pub fn record_qa_gate_result(&self, passed: bool) {
        let mut s = self.stability.write().unwrap();
        s.qa_gate_runs = s.qa_gate_runs.saturating_add(1);
        if !passed {
            s.qa_gate_failures = s.qa_gate_failures.saturating_add(1);
        }
    }

    pub fn record_deterministic_replay_delta(&self, delta: f64) {
        let mut s = self.stability.write().unwrap();
        s.deterministic_replay_delta_sum += delta.max(0.0);
        s.deterministic_replay_delta_samples = s.deterministic_replay_delta_samples.saturating_add(1);
    }

    pub fn stability_summary(&self) -> serde_json::Value {
        let s = self.stability.read().unwrap().clone();
        let avg_plan_stability_score = if s.plan_runs == 0 {
            0.0
        } else {
            s.plan_stability_score_sum / s.plan_runs as f64
        };
        let avg_retry_chain_depth = if s.plan_runs == 0 {
            0.0
        } else {
            s.retry_chain_depth_sum as f64 / s.plan_runs as f64
        };
        let qa_gate_fail_rate = if s.qa_gate_runs == 0 {
            0.0
        } else {
            s.qa_gate_failures as f64 / s.qa_gate_runs as f64
        };
        let deterministic_replay_delta = if s.deterministic_replay_delta_samples == 0 {
            0.0
        } else {
            s.deterministic_replay_delta_sum / s.deterministic_replay_delta_samples as f64
        };
        serde_json::json!({
            "plan_stability_score": avg_plan_stability_score,
            "retry_chain_depth_avg": avg_retry_chain_depth,
            "retry_chain_depth_max": s.retry_chain_depth_max,
            "qa_gate_fail_rate": qa_gate_fail_rate,
            "deterministic_replay_delta": deterministic_replay_delta,
            "samples": {
                "plan_runs": s.plan_runs,
                "qa_gate_runs": s.qa_gate_runs,
                "replay_samples": s.deterministic_replay_delta_samples
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct TestSink {
        events: Mutex<Vec<serde_json::Value>>,
    }

    impl MetricsEventSink for TestSink {
        fn on_event(&self, event: &serde_json::Value) {
            self.events.lock().unwrap().push(event.clone());
        }
    }

    #[test]
    fn stability_metrics_are_recorded_and_exposed() {
        let m = MetricsCollector::new();
        m.record_plan_stability_score(1.0);
        m.record_plan_stability_score(0.5);
        m.record_retry_chain_depth(2);
        m.record_retry_chain_depth(4);
        m.record_qa_gate_result(true);
        m.record_qa_gate_result(false);
        m.record_deterministic_replay_delta(0.2);

        let summary = m.stability_summary();
        let score = summary.get("plan_stability_score").and_then(|v| v.as_f64()).unwrap_or(-1.0);
        let retry_max = summary.get("retry_chain_depth_max").and_then(|v| v.as_u64()).unwrap_or(0);
        let fail_rate = summary.get("qa_gate_fail_rate").and_then(|v| v.as_f64()).unwrap_or(-1.0);

        assert!(score > 0.0);
        assert_eq!(retry_max, 4);
        assert!(fail_rate > 0.0);
    }

    #[test]
    fn buffered_events_are_flushed_on_sink_attach() {
        let m = MetricsCollector::new();
        m.record_failure("p", "m");
        let sink = Arc::new(TestSink::default());
        m.attach_event_sink(sink.clone());
        let guard = sink.events.lock().unwrap();
        assert!(!guard.is_empty());
    }
}
