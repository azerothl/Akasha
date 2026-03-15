//! Akasha Daemon - 24/7 runtime with supervision and healthcheck

pub mod agent_profile;
pub mod agents;
pub mod personality;
pub mod api;
pub mod debug_log;
pub mod channels;
pub mod daemon;
pub mod replication;
pub mod device_bridge;
pub mod health;
pub mod image_generation;
pub mod memory;
pub mod memory_actor;
pub mod memory_orchestrator;
pub mod plugins;
pub mod scheduler;
pub mod skills;
pub mod user_rag;

pub use daemon::{Daemon, RunOutcome};
pub use health::{HealthState, HealthStatus};
