//! Download curated embedded GGUF models (manifest in spec/embedded_models.json).

use crate::config::{default_gguf_path, embedded_models_manifest_path};
use std::fs;
use std::io::{Read, Write};
use std::path::Path;

#[derive(Debug, serde::Deserialize)]
struct Manifest {
    default_id: String,
    models: Vec<ManifestModel>,
}

#[derive(Debug, serde::Deserialize)]
struct ManifestModel {
    id: String,
    label: String,
    filename: String,
    url: String,
    #[serde(default)]
    size_bytes_hint: u64,
}

pub fn download_default_model(client: &reqwest::blocking::Client) -> Result<std::path::PathBuf, String> {
    download_model(client, None)
}

pub fn download_model(
    client: &reqwest::blocking::Client,
    model_id: Option<&str>,
) -> Result<std::path::PathBuf, String> {
    let manifest_path = embedded_models_manifest_path();
    let raw = fs::read_to_string(&manifest_path)
        .map_err(|e| format!("read manifest {}: {e}", manifest_path.display()))?;
    let manifest: Manifest =
        serde_json::from_str(&raw).map_err(|e| format!("parse manifest: {e}"))?;
    let id = model_id.unwrap_or(&manifest.default_id);
    let entry = manifest
        .models
        .iter()
        .find(|m| m.id == id)
        .ok_or_else(|| format!("unknown embedded model id {id:?}"))?;

    let dest = default_gguf_path();
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }

    let tmp = dest.with_extension("gguf.part");
    download_url(client, &entry.url, &tmp, entry.size_bytes_hint)?;

    if dest.exists() {
        fs::remove_file(&dest).map_err(|e| format!("remove old model: {e}"))?;
    }
    fs::rename(&tmp, &dest).map_err(|e| format!("rename model: {e}"))?;

    Ok(dest)
}

fn download_url(
    client: &reqwest::blocking::Client,
    url: &str,
    dest: &Path,
    size_hint: u64,
) -> Result<(), String> {
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
