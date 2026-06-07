//! Phase 5 — Plugin framework: registry, WASM host, reputation, catalog.

pub mod memory_delegate;
pub mod metrics;
pub mod registry;
pub mod reputation;
pub mod selection;
pub mod state;

pub use registry::{PluginEntry, PluginRegistry};
pub use reputation::{PluginReputation, ReputationStore};
pub use state::PluginStateStore;
