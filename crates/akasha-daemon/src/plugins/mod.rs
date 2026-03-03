//! Phase 5 — Plugin framework: registry, WASM host, reputation, catalog.

pub mod registry;
pub mod reputation;

pub use registry::{PluginEntry, PluginRegistry};
pub use reputation::{PluginReputation, ReputationStore};
