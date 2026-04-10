//! Akasha Daemon - 24/7 runtime with supervision and healthcheck

pub mod agent_profile;
pub mod user_profile;
pub mod agents;
pub mod gateway;
pub mod personality;
pub mod policy_engine;
pub mod protocol_adapter;
pub mod agent_contracts;
pub mod api;
pub mod api_workspace_graph;
pub mod session_state;
pub mod browser;
pub mod channels;
pub mod daemon;
pub mod replication;
pub mod device_bridge;
pub mod health;
pub mod image_generation;
pub mod latency;
pub mod voice;
pub mod memory;
pub mod memory_actor;
pub mod memory_relation_inference;
pub mod memory_orchestrator;
pub mod plugins;
pub mod scheduler;
pub mod skills;
pub mod user_rag;

pub use daemon::{Daemon, RunOutcome};
pub use health::{HealthState, HealthStatus};
