//! Health monitoring - Heartbeat every 5s (NFR: healthcheck interval 5s max)

use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

/// Health status for the daemon (Phase 4: degraded mode).
#[derive(Debug, Clone)]
pub struct HealthState {
    pub healthy: bool,
    pub started_at: std::time::Instant,
    pub last_heartbeat: std::time::Instant,
    /// When true, daemon is operational but in degraded mode (e.g. LLM or memory unavailable).
    pub degraded: bool,
    /// Reason for degraded mode, if set.
    pub degraded_reason: Option<String>,
}

impl HealthState {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            healthy: true,
            started_at: now,
            last_heartbeat: now,
            degraded: false,
            degraded_reason: None,
        }
    }

    pub fn tick(&mut self) {
        self.last_heartbeat = Instant::now();
    }

    pub fn set_degraded(&mut self, reason: impl Into<String>) {
        self.degraded = true;
        self.degraded_reason = Some(reason.into());
    }

    pub fn clear_degraded(&mut self) {
        self.degraded = false;
        self.degraded_reason = None;
    }
}

pub type HealthStatus = Arc<RwLock<HealthState>>;

static RUNTIME_HEALTH: std::sync::OnceLock<HealthStatus> = std::sync::OnceLock::new();
pub(crate) fn register_runtime(health: HealthStatus) { let _ = RUNTIME_HEALTH.set(health); }
pub(crate) async fn runtime_status(data_dir: &std::path::Path) -> serde_json::Value {
    let state = match RUNTIME_HEALTH.get() { Some(health) => Some(health.read().await.clone()), None => None };
    serde_json::json!({
        "status":"ok", "service":"akasha-daemon", "pid":std::process::id(),
        "version":env!("CARGO_PKG_VERSION"), "data_dir":data_dir,
        "uptime_seconds":state.as_ref().map(|state|state.started_at.elapsed().as_secs()),
        "healthy":state.as_ref().map(|state|state.healthy),
        "degraded":state.as_ref().map(|state|state.degraded),
        "degraded_reason":state.as_ref().and_then(|state|state.degraded_reason.clone()),
        "heartbeat_age_seconds":state.as_ref().map(|state|state.last_heartbeat.elapsed().as_secs()),
    })
}

#[cfg(test)]
mod runtime_tests {
    #[tokio::test]
    async fn status_identifies_the_process_without_changing_legacy_status_field() {
        let value=super::runtime_status(std::path::Path::new("data-dir")).await;
        assert_eq!(value["status"],"ok");assert_eq!(value["service"],"akasha-daemon");
        assert_eq!(value["pid"],std::process::id());assert_eq!(value["version"],env!("CARGO_PKG_VERSION"));
        assert_eq!(value["data_dir"],"data-dir");
    }
}
