//! Persisted runtime selection after hardware calibration.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const RUNTIME_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchResultEntry {
    pub config: String,
    pub tok_per_s: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttft_s: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddedRuntime {
    pub version: u32,
    pub tier_id: String,
    pub model_id: String,
    pub backend: String,
    pub n_gpu_layers: u32,
    pub calibrated_at: String,
    #[serde(default)]
    pub bench_results: Vec<BenchResultEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub winner_tok_per_s: Option<f64>,
}

pub fn runtime_path() -> PathBuf {
    crate::config::akasha_data_dir().join("embedded_runtime.json")
}

pub fn load_runtime() -> Option<EmbeddedRuntime> {
    let path = runtime_path();
    let raw = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn save_runtime(rt: &EmbeddedRuntime) -> Result<(), String> {
    let path = runtime_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create data dir: {e}"))?;
    }
    let json = serde_json::to_string_pretty(rt).map_err(|e| format!("serialize runtime: {e}"))?;
    std::fs::write(&path, json).map_err(|e| format!("write {}: {e}", path.display()))
}

pub fn calibration_done() -> bool {
    load_runtime()
        .map(|r| r.version == RUNTIME_VERSION && !r.model_id.is_empty())
        .unwrap_or(false)
}

pub fn clear_runtime() -> Result<(), String> {
    let path = runtime_path();
    if path.is_file() {
        std::fs::remove_file(&path).map_err(|e| format!("remove runtime: {e}"))?;
    }
    Ok(())
}
