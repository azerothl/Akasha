//! Micro-benchmark for embedded engine + model selection.

use crate::hardware::detect_hardware;
use crate::profiles::{calibration_candidates, StaticCandidate};
use crate::runtime::{save_runtime, BenchResultEntry, EmbeddedRuntime, RUNTIME_VERSION};
use once_cell::sync::Lazy;
use std::sync::{Mutex, RwLock};
use std::time::Instant;

pub const CALIBRATION_PROMPT: &str =
    "Write exactly 32 short numbered lines, one per line, starting at 1.";
pub const CALIBRATION_MAX_TOKENS: usize = 32;

#[derive(Debug, Clone, serde::Serialize, Default)]
pub struct CalibrateProgress {
    pub state: String,
    pub current: usize,
    pub total: usize,
    pub current_config: Option<String>,
    pub percent: f64,
    pub error: Option<String>,
    pub winner: Option<CalibrateWinner>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CalibrateWinner {
    pub model_id: String,
    pub n_gpu_layers: u32,
    pub backend: String,
    pub tok_per_s: f64,
    pub device: String,
}

static CALIBRATE_PROGRESS: Lazy<RwLock<CalibrateProgress>> =
    Lazy::new(|| RwLock::new(CalibrateProgress::default()));
static CALIBRATE_MUTEX: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

pub fn calibrate_progress_snapshot() -> CalibrateProgress {
    CALIBRATE_PROGRESS
        .read()
        .map(|g| g.clone())
        .unwrap_or_default()
}

/// Run calibration synchronously (call from background thread).
pub fn run_calibration(max_configs: usize) -> Result<EmbeddedRuntime, String> {
    let _lock = CALIBRATE_MUTEX
        .lock()
        .map_err(|e| format!("calibrate lock: {e}"))?;

    let profile = detect_hardware();
    let candidates = calibration_candidates(&profile, max_configs)?;
    let total = candidates.len().max(1);

    set_progress_running(&candidates, total);

    let mut bench_results = Vec::new();
    let mut best: Option<(StaticCandidate, f64, String)> = None;

    for (idx, cand) in candidates.iter().enumerate() {
        let config_label = format!("{} ngl={}", cand.model_id, cand.n_gpu_layers);
        set_progress_step(idx + 1, total, &config_label);

        match bench_candidate(cand) {
            Ok((tok_per_s, ttft_s, device)) => {
                bench_results.push(BenchResultEntry {
                    config: config_label.clone(),
                    tok_per_s,
                    ttft_s: Some(ttft_s),
                    error: None,
                });
                let replace = best
                    .as_ref()
                    .map(|(_, t, _)| tok_per_s > *t)
                    .unwrap_or(true);
                if replace {
                    best = Some((cand.clone(), tok_per_s, device));
                }
            }
            Err(e) => {
                bench_results.push(BenchResultEntry {
                    config: config_label,
                    tok_per_s: 0.0,
                    ttft_s: None,
                    error: Some(e.clone()),
                });
            }
        }
    }

    let (winner_cand, winner_tok, winner_device) = match best {
        Some(b) => b,
        None => fallback_static_winner(&profile, &candidates)?,
    };

    let rt = EmbeddedRuntime {
        version: RUNTIME_VERSION,
        tier_id: profile.tier_id.clone(),
        model_id: winner_cand.model_id.clone(),
        backend: "llama_cpp".to_string(),
        n_gpu_layers: winner_cand.n_gpu_layers,
        calibrated_at: chrono_now(),
        bench_results,
        winner_tok_per_s: Some(winner_tok),
    };
    save_runtime(&rt)?;

    crate::EmbeddedLlm::unload();
    apply_runtime_env(&rt);
    let _ = crate::EmbeddedLlm::preload();

    set_progress_done(CalibrateWinner {
        model_id: winner_cand.model_id,
        n_gpu_layers: winner_cand.n_gpu_layers,
        backend: winner_cand.engine.clone(),
        tok_per_s: winner_tok,
        device: winner_device,
    });

    Ok(rt)
}

pub fn start_calibration_background(max_configs: usize) -> Result<(), String> {
    let snap = calibrate_progress_snapshot();
    if snap.state == "running" {
        return Err("calibration already in progress".to_string());
    }
    if let Ok(mut g) = CALIBRATE_PROGRESS.write() {
        *g = CalibrateProgress::default();
    }
    std::thread::spawn(move || {
        if let Err(e) = run_calibration(max_configs) {
            set_progress_error(&e);
        }
    });
    Ok(())
}

fn fallback_static_winner(
    profile: &crate::hardware::HardwareProfile,
    candidates: &[StaticCandidate],
) -> Result<(StaticCandidate, f64, String), String> {
    let cand = candidates
        .iter()
        .find(|c| c.model_id.contains("smollm2"))
        .or_else(|| candidates.first())
        .cloned()
        .ok_or_else(|| "no calibration candidates".to_string())?;
    Ok((cand, 0.0, if profile.cuda_runtime { "cuda" } else { "cpu" }.to_string()))
}

fn bench_candidate(cand: &StaticCandidate) -> Result<(f64, f64, String), String> {
    #[cfg(not(feature = "llama-cpp"))]
    {
        let _ = cand;
        return Err("llama-cpp not compiled".into());
    }
    #[cfg(feature = "llama-cpp")]
    {
        let path = resolve_model_path(&cand.model_id)?;
        crate::EmbeddedLlm::unload();
        std::env::set_var("AKASHA_EMBEDDED_BACKEND", "llama_cpp");
        std::env::set_var("AKASHA_EMBEDDED_GGUF_PATH", path.display().to_string());
        std::env::set_var(
            "AKASHA_EMBEDDED_N_GPU_LAYERS",
            cand.n_gpu_layers.to_string(),
        );

        crate::EmbeddedLlm::preload()
            .map_err(|e| format!("preload {}: {e}", cand.model_id))?;

        let llm = crate::EmbeddedLlm::new();

        let mut first_token: Option<f64> = None;
        let gen_start = Instant::now();
        let text = llm
            .complete_stream(
                CALIBRATION_PROMPT,
                Some(CALIBRATION_MAX_TOKENS),
                Some(0.3),
                |chunk| {
                    if !chunk.is_empty() && first_token.is_none() {
                        first_token = Some(gen_start.elapsed().as_secs_f64());
                    }
                },
            )
            .map_err(|e| format!("inference {}: {e}", cand.model_id))?;
        let gen_s = gen_start.elapsed().as_secs_f64().max(0.001);
        let approx_tokens = text.split_whitespace().count().max(1);
        let tok_per_s = approx_tokens as f64 / gen_s;
        let ttft = first_token.unwrap_or(0.0);
        let device = crate::EmbeddedLlm::device_hint().unwrap_or_else(|| "cpu".into());
        Ok((tok_per_s, ttft, device))
    }
}

fn resolve_model_path(model_id: &str) -> Result<std::path::PathBuf, String> {
    #[cfg(feature = "download")]
    {
        let manifest = crate::download::load_manifest()?;
        let entry = manifest
            .models
            .iter()
            .find(|m| m.id == model_id)
            .ok_or_else(|| format!("unknown model {model_id}"))?;
        let path = crate::config::gguf_path_for_filename(&entry.filename);
        if path.is_file() {
            return Ok(path);
        }
        let client = reqwest::blocking::Client::builder()
            .user_agent("akasha-embedded-calibrate/0.10")
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());
        return crate::download::download_model(&client, Some(model_id));
    }
    #[cfg(not(feature = "download"))]
    {
        let _ = model_id;
        Err("download feature required for calibration".into())
    }
}

pub fn apply_runtime_env(rt: &EmbeddedRuntime) {
    std::env::set_var("AKASHA_EMBEDDED_BACKEND", "llama_cpp");
    std::env::set_var(
        "AKASHA_EMBEDDED_N_GPU_LAYERS",
        rt.n_gpu_layers.to_string(),
    );
    if let Ok(path) = gguf_path_for_model_id(&rt.model_id) {
        std::env::set_var("AKASHA_EMBEDDED_GGUF_PATH", path.display().to_string());
    }
}

pub fn gguf_path_for_model_id(model_id: &str) -> Result<std::path::PathBuf, String> {
    #[cfg(feature = "download")]
    {
        let manifest = crate::download::load_manifest()?;
        let entry = manifest
            .models
            .iter()
            .find(|m| m.id == model_id)
            .ok_or_else(|| format!("unknown model {model_id}"))?;
        Ok(crate::config::gguf_path_for_filename(&entry.filename))
    }
    #[cfg(not(feature = "download"))]
    {
        let _ = model_id;
        Err("download feature not enabled".into())
    }
}

fn chrono_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}")
}

fn set_progress_running(candidates: &[StaticCandidate], total: usize) {
    if let Ok(mut g) = CALIBRATE_PROGRESS.write() {
        *g = CalibrateProgress {
            state: "running".into(),
            current: 0,
            total,
            current_config: candidates.first().map(|c| format!("{} ngl={}", c.model_id, c.n_gpu_layers)),
            percent: 0.0,
            error: None,
            winner: None,
        };
    }
}

fn set_progress_step(current: usize, total: usize, config: &str) {
    if let Ok(mut g) = CALIBRATE_PROGRESS.write() {
        g.current = current;
        g.total = total;
        g.current_config = Some(config.to_string());
        g.percent = if total > 0 {
            (current as f64 / total as f64 * 100.0).min(99.0)
        } else {
            0.0
        };
    }
}

fn set_progress_done(winner: CalibrateWinner) {
    if let Ok(mut g) = CALIBRATE_PROGRESS.write() {
        g.state = "done".into();
        g.percent = 100.0;
        g.winner = Some(winner);
    }
}

fn set_progress_error(msg: &str) {
    if let Ok(mut mut_g) = CALIBRATE_PROGRESS.write() {
        mut_g.state = "error".into();
        mut_g.error = Some(msg.to_string());
    }
}
