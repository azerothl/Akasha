//! Phase 2 — Agents v1: Main Agent, Orchestrator, Workers

mod bus;
mod contract;
mod interpretation;
mod main_agent;
pub mod orchestrator;
mod progress_subscriber;
mod prompts;
mod supervisor;
mod worker;

pub use bus::{new_event_bus, subscribe, EventBus};
pub use main_agent::{MainAgent, OrchestratorSender, OrchestratorTask, TaskPriority};
pub use interpretation::interpret_message;
pub use supervisor::{classify_execution_mode, ExecutionMode};
pub use orchestrator::Orchestrator;
pub use progress_subscriber::{run_events_subscriber, run_progress_subscriber};
pub use worker::run_worker;
