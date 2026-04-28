//! Phase 5 — Plugin framework: registry, WASM host, reputation, catalog.

pub mod metrics;
pub mod registry;
pub mod reputation;
pub mod selection;

pub use registry::{PluginEntry, PluginRegistry};
pub use reputation::{PluginReputation, ReputationStore};
