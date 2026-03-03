//! Health monitoring - Heartbeat every 5s (NFR: healthcheck interval 5s max)

use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

/// Health status for the daemon
#[derive(Debug, Clone)]
pub struct HealthState {
    pub healthy: bool,
    pub started_at: std::time::Instant,
    pub last_heartbeat: std::time::Instant,
}

impl HealthState {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            healthy: true,
            started_at: now,
            last_heartbeat: now,
        }
    }

    pub fn tick(&mut self) {
        self.last_heartbeat = Instant::now();
    }
}

pub type HealthStatus = Arc<RwLock<HealthState>>;
