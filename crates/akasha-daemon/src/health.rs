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
