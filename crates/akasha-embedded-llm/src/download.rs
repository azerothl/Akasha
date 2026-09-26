//! Download curated embedded GGUF models (manifest in spec/embedded_models.json).

use crate::config::{embedded_models_manifest_path, gguf_path_for_filename};
use once_cell::sync::Lazy;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};
use std::thread;

#[derive(Debug, Clone, serde::Serialize, Default)]
pub struct DownloadProgress {
    pub state: String,
    pub percent: f64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub model_id: Option<String>,
    pub error: Option<String>,
}

static DOWNLOAD_PROGRESS: Lazy<RwLock<DownloadProgress>> =
    Lazy::new(|| RwLock::new(DownloadProgress::default()));

static DOWNLOAD_MUTEX: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ManifestModel {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub label_en: Option<String>,
    #[serde(default)]
    pub label_fr: Option<String>,
    pub filename: String,
    pub url: String,
    #[serde(default)]
    pub size_bytes_hint: u64,
    #[serde(default)]
    pub min_vram_mb: u32,
    #[serde(default)]
    pub min_ram_mb: u32,
    #[serde(default)]
    pub arch: String,
    #[serde(default)]
    pub engine_candidates: Vec<String>,
    #[serde(default)]
    pub profile_tiers: Vec<String>,
    /// Camelid-style class: supported | evidence_only | groundwork_only.
    #[serde(default)]
    pub compatibility: Option<String>,
    /// Whether the architecture is multimodal (vision) — requires mmproj for llama.cpp.
    #[serde(default)]
    pub vision: bool,
    /// If true, chat-with-image needs a companion mmproj GGUF.
    #[serde(default)]
    pub mmproj_required: bool,
    /// Optional mmproj filename under `models/embedded/`.
    #[serde(default)]
    pub mmproj_filename: Option<String>,
    /// Optional mmproj download URL (wizard / bench tooling).
    #[serde(default)]
    pub mmproj_url: Option<String>,
    /// Bench tier label: tier1 | tier2 | tier3.
    #[serde(default)]
    pub bench_tier: Option<String>,
    /// Role: default | wizard_variant | bench_candidate | watch.
    #[serde(default)]
    pub bench_role: Option<String>,
    /// Free-form evidence note for `/api/capabilities`.
    #[serde(default)]
    pub evidence_notes: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct Manifest {
    pub default_id: String,
    pub models: Vec<ManifestModel>,
}

pub fn load_manifest() -> Result<Manifest, String> {
    let manifest_path = embedded_models_manifest_path();
    let raw = fs::read_to_string(&manifest_path)
        .map_err(|e| format!("read manifest {}: {e}", manifest_path.display()))?;
    serde_json::from_str(&raw).map_err(|e| format!("parse manifest: {e}"))
}

pub fn download_progress_snapshot() -> DownloadProgress {
    DOWNLOAD_PROGRESS
        .read()
        .map(|g| g.clone())
        .unwrap_or_default()
}

pub fn download_default_model(client: &reqwest::blocking::Client) -> Result<PathBuf, String> {
    download_model(client, None)
}

pub fn download_model(
    client: &reqwest::blocking::Client,
    model_id: Option<&str>,
) -> Result<PathBuf, String> {
    let manifest = load_manifest()?;
    let id = model_id.unwrap_or(&manifest.default_id);
    let entry = manifest
        .models
        .iter()
        .find(|m| m.id == id)
        .ok_or_else(|| format!("unknown embedded model id {id:?}"))?;

    let dest = gguf_path_for_filename(&entry.filename);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }

    set_progress_running(id, entry.size_bytes_hint);

    let tmp = dest.with_extension("gguf.part");
    let download_result = download_url(client, &entry.url, &tmp, entry.size_bytes_hint, |done, total| {
        set_progress_bytes(done, total);
    });

    match download_result {
        Ok(()) => {
            if dest.exists() {
                fs::remove_file(&dest).map_err(|e| format!("remove old model: {e}"))?;
            }
            fs::rename(&tmp, &dest).map_err(|e| format!("rename model: {e}"))?;
            set_progress_done();
            Ok(dest)
        }
        Err(e) => {
            set_progress_error(&e);
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Start download on a background thread; poll with `download_progress_snapshot`.
pub fn start_download_background(model_id: Option<String>) -> Result<(), String> {
    let snap = download_progress_snapshot();
    if snap.state == "running" {
        return Err("download already in progress".to_string());
    }
    if let Ok(mut g) = DOWNLOAD_PROGRESS.write() {
        *g = DownloadProgress::default();
    }
    thread::spawn(move || {
        let _lock = DOWNLOAD_MUTEX.lock().expect("download mutex");
        let client = reqwest::blocking::Client::builder()
            .user_agent("akasha-embedded-download/0.10")
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());
        let _ = download_model(&client, model_id.as_deref());
    });
    Ok(())
}

fn set_progress_running(model_id: &str, size_hint: u64) {
    if let Ok(mut g) = DOWNLOAD_PROGRESS.write() {
        *g = DownloadProgress {
            state: "running".into(),
            percent: 0.0,
            bytes_done: 0,
            bytes_total: size_hint,
            model_id: Some(model_id.to_string()),
            error: None,
        };
    }
}

fn set_progress_bytes(done: u64, total: u64) {
    if let Ok(mut g) = DOWNLOAD_PROGRESS.write() {
        g.bytes_done = done;
        g.bytes_total = total.max(g.bytes_total);
        g.percent = if total > 0 {
            (done as f64 / total as f64 * 100.0).min(100.0)
        } else {
            0.0
        };
    }
}

fn set_progress_done() {
    if let Ok(mut g) = DOWNLOAD_PROGRESS.write() {
        g.state = "done".into();
        g.percent = 100.0;
    }
}

fn set_progress_error(msg: &str) {
    if let Ok(mut g) = DOWNLOAD_PROGRESS.write() {
        g.state = "error".into();
        g.error = Some(msg.to_string());
    }
}

fn download_url<F>(
    client: &reqwest::blocking::Client,
    url: &str,
    dest: &Path,
    size_hint: u64,
    mut on_progress: F,
) -> Result<(), String>
where
    F: FnMut(u64, u64),
{
    let mut resp = client
        .get(url)
        .send()
        .map_err(|e| format!("GET {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("GET {url}: HTTP {}", resp.status()));
    }
    let total = resp
        .content_length()
        .unwrap_or(size_hint)
        .max(size_hint);
    let mut file = fs::File::create(dest).map_err(|e| format!("create {}: {e}", dest.display()))?;
    let mut buf = [0u8; 64 * 1024];
    let mut done: u64 = 0;
    loop {
        let n = resp
            .read(&mut buf)
            .map_err(|e| format!("read body: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .map_err(|e| format!("write {}: {e}", dest.display()))?;
        done += n as u64;
        on_progress(done, total);
        if total > 0 {
            let pct = (done as f64 / total as f64 * 100.0).min(100.0);
            eprint!("\r  download: {:.1}% ({done}/{total} bytes)", pct);
        } else {
            eprint!("\r  download: {done} bytes");
        }
    }
    eprintln!();
    Ok(())
}
