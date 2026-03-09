//! Phase 2 — Agents v1: Main Agent, Orchestrator, Workers

mod bus;
mod main_agent;
mod orchestrator;
mod progress_subscriber;
mod worker;

pub use bus::{new_event_bus, EventBus};
pub use main_agent::{MainAgent, OrchestratorSender, OrchestratorTask, TaskPriority};
pub use orchestrator::Orchestrator;
pub use progress_subscriber::{run_events_subscriber, run_progress_subscriber};
pub use worker::run_worker;
