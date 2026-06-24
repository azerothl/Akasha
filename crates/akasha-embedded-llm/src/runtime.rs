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

pub fn engine_mode_from_ngl(n_gpu_layers: u32) -> &'static str {
    crate::config::engine_mode_from_ngl(n_gpu_layers)
}

fn manual_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "manual".into())
}

/// Apply user-selected model + engine mode, persist, and unload for reload.
pub fn apply_manual_runtime(model_id: &str, engine_mode: &str) -> Result<EmbeddedRuntime, String> {
    let profile = crate::hardware::detect_hardware();
    let n_gpu_layers = match engine_mode {
        "cuda" | "llama_cpp_cuda" => 99u32,
        "cpu" | "llama_cpp_cpu" => 0u32,
        "auto" => crate::config::static_n_gpu_layers_for_tier(),
        _ => return Err(format!("unknown engine_mode {engine_mode:?} (use auto, cpu, or cuda)")),
    };

    #[cfg(feature = "download")]
    {
        let manifest = crate::download::load_manifest()?;
        if !manifest.models.iter().any(|m| m.id == model_id) {
            return Err(format!("unknown embedded model id {model_id:?}"));
        }
        let entry = manifest
            .models
            .iter()
            .find(|m| m.id == model_id)
            .unwrap();
        let path = crate::config::gguf_path_for_filename(&entry.filename);
        if !path.is_file() {
            return Err(format!(
                "GGUF not on disk for {model_id} — run embedded-download first"
            ));
        }
    }
    #[cfg(not(feature = "download"))]
    {
        let _ = model_id;
    }

    let rt = EmbeddedRuntime {
        version: RUNTIME_VERSION,
        tier_id: profile.tier_id,
        model_id: model_id.to_string(),
        backend: "llama_cpp".to_string(),
        n_gpu_layers,
        calibrated_at: manual_timestamp(),
        bench_results: Vec::new(),
        winner_tok_per_s: None,
    };
    save_runtime(&rt)?;
    #[cfg(all(feature = "llama-cpp", feature = "download"))]
    crate::calibrate::apply_runtime_env(&rt);
    crate::EmbeddedLlm::unload();
    Ok(rt)
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct EmbeddedModelOption {
    pub id: String,
    pub label: String,
    pub filename: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct EmbeddedSettingsView {
    pub active_model_id: Option<String>,
    pub engine_mode: String,
    pub n_gpu_layers: u32,
    pub hardware_tier: String,
    pub active_engine_policy: String,
    pub runtime: Option<EmbeddedRuntime>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<EmbeddedModelOption>,
}

pub fn settings_view() -> Result<EmbeddedSettingsView, String> {
    let profile = crate::hardware::detect_hardware();
    let n_gpu_layers = crate::config::n_gpu_layers();
    let mut models = Vec::new();
    #[cfg(feature = "download")]
    if let Ok(manifest) = crate::download::load_manifest() {
        models = manifest
            .models
            .into_iter()
            .filter(|entry| {
                entry.profile_tiers.is_empty()
                    || entry
                        .profile_tiers
                        .iter()
                        .any(|t| t == &profile.tier_id)
            })
            .map(|m| EmbeddedModelOption {
                id: m.id,
                label: m.label,
                filename: m.filename,
            })
            .collect();
    }

    Ok(EmbeddedSettingsView {
        active_model_id: crate::config::active_model_id(),
        engine_mode: crate::config::active_engine_mode(),
        n_gpu_layers,
        hardware_tier: profile.tier_id,
        active_engine_policy: crate::config::active_engine_policy(),
        runtime: load_runtime(),
        models,
    })
}
