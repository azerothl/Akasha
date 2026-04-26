//! Operator metrics for WASM plugin load cycles (Hermes parity / observability).

use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};

static LOAD_CYCLES: AtomicU64 = AtomicU64::new(0);
static LAST_LOAD_MS: AtomicU64 = AtomicU64::new(0);
static LAST_PLUGINS_LOADED: AtomicU64 = AtomicU64::new(0);
static LAST_LOAD_ERRORS: AtomicU64 = AtomicU64::new(0);
static TOTAL_LOAD_MS: AtomicU64 = AtomicU64::new(0);

/// Record one `PluginRegistry::load_all` completion.
pub fn record_plugin_load(elapsed_ms: u64, loaded_ok: u64, load_errors: u64) {
    LOAD_CYCLES.fetch_add(1, Ordering::Relaxed);
    LAST_LOAD_MS.store(elapsed_ms, Ordering::Relaxed);
    LAST_PLUGINS_LOADED.store(loaded_ok, Ordering::Relaxed);
    LAST_LOAD_ERRORS.store(load_errors, Ordering::Relaxed);
    TOTAL_LOAD_MS.fetch_add(elapsed_ms, Ordering::Relaxed);
}

#[derive(Serialize)]
pub struct PluginLoadMetrics {
    pub load_cycles: u64,
    pub last_load_ms: u64,
    pub last_plugins_loaded: u64,
    pub last_load_errors: u64,
    pub total_load_ms: u64,
}

pub fn snapshot() -> PluginLoadMetrics {
    PluginLoadMetrics {
        load_cycles: LOAD_CYCLES.load(Ordering::Relaxed),
        last_load_ms: LAST_LOAD_MS.load(Ordering::Relaxed),
        last_plugins_loaded: LAST_PLUGINS_LOADED.load(Ordering::Relaxed),
        last_load_errors: LAST_LOAD_ERRORS.load(Ordering::Relaxed),
        total_load_ms: TOTAL_LOAD_MS.load(Ordering::Relaxed),
    }
}
