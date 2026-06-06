//! Install WASM plugins from the Akasha_plugins catalog (jsDelivr).

use akasha_plugin_api::{is_safe_plugin_id, PluginManifest};
use std::path::{Path, PathBuf};
use std::time::Duration;

const DEFAULT_CATALOG_URL: &str =
    "https://cdn.jsdelivr.net/gh/azerothl/Akasha_plugins@main/plugins.json";
const CDN_BASE: &str = "https://cdn.jsdelivr.net/gh/azerothl/Akasha_plugins@main";
const MAX_DOWNLOAD_BYTES: usize = 20 * 1024 * 1024;

fn http_client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(Duration::from_secs(90))
        .build()?)
}

async fn fetch_bytes(url: &str) -> anyhow::Result<Vec<u8>> {
    if !url.starts_with("https://") {
        anyhow::bail!("only HTTPS URLs are allowed");
    }
    let client = http_client()?;
    let resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("download failed: HTTP {}", resp.status());
    }
    let bytes = resp.bytes().await?;
    if bytes.len() > MAX_DOWNLOAD_BYTES {
        anyhow::bail!("download exceeds size limit");
    }
    Ok(bytes.to_vec())
}

fn catalog_plugin_path(catalog: &serde_json::Value, id: &str) -> Option<String> {
    catalog
        .get("plugins")
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter().find(|p| p.get("id").and_then(|x| x.as_str()) == Some(id))
        })
        .and_then(|p| p.get("path").and_then(|x| x.as_str()).map(String::from))
}

fn catalog_wasm_sha256(catalog: &serde_json::Value, id: &str) -> Option<String> {
    catalog
        .get("plugins")
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter().find(|p| p.get("id").and_then(|x| x.as_str()) == Some(id))
        })
        .and_then(|p| p.get("wasm_sha256").and_then(|x| x.as_str()).map(String::from))
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{:02x}", b)).collect()
}

fn write_plugin_files(
    dest: &Path,
    manifest_bytes: &[u8],
    wasm_bytes: &[u8],
    wasm_filename: &str,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(dest)?;
    std::fs::write(dest.join("manifest.toml"), manifest_bytes)?;
    std::fs::write(dest.join(wasm_filename), wasm_bytes)?;
    Ok(())
}

/// Install a plugin by catalog id. Returns installed plugin id.
pub async fn install_from_catalog_id(
    data_dir: &Path,
    catalog_id: &str,
    catalog_url: Option<&str>,
) -> anyhow::Result<String> {
    if !is_safe_plugin_id(catalog_id) {
        anyhow::bail!("invalid plugin id");
    }
    let catalog_url = catalog_url.unwrap_or(DEFAULT_CATALOG_URL);
    if !catalog_url.starts_with("https://") {
        anyhow::bail!("catalog URL must be HTTPS");
    }
    let catalog_bytes = fetch_bytes(catalog_url).await?;
    let catalog: serde_json::Value = serde_json::from_slice(&catalog_bytes)?;
    let rel_path = catalog_plugin_path(&catalog, catalog_id)
        .ok_or_else(|| anyhow::anyhow!("plugin '{}' not found in catalog", catalog_id))?;
    let manifest_url = format!("{CDN_BASE}/{rel_path}/manifest.toml");
    let manifest_bytes = fetch_bytes(&manifest_url).await?;
    let manifest_str = String::from_utf8(manifest_bytes.clone())
        .map_err(|_| anyhow::anyhow!("manifest is not UTF-8"))?;
    let manifest: PluginManifest = toml::from_str(&manifest_str)?;
    if manifest.id != catalog_id {
        anyhow::bail!(
            "manifest id '{}' does not match catalog id '{}'",
            manifest.id,
            catalog_id
        );
    }
    let wasm_name = manifest
        .wasm_path
        .as_deref()
        .unwrap_or("plugin.wasm");
    let wasm_url = format!("{CDN_BASE}/{rel_path}/{wasm_name}");
    let wasm_bytes = fetch_bytes(&wasm_url).await?;
    if let Some(expected) = catalog_wasm_sha256(&catalog, catalog_id) {
        let got = sha256_hex(&wasm_bytes);
        if !expected.eq_ignore_ascii_case(&got) {
            anyhow::bail!("wasm sha256 mismatch (expected {}, got {})", expected, got);
        }
    }
    let plugins_dir = data_dir.join("plugins");
    let dest = plugins_dir.join(catalog_id);
    if dest.exists() {
        std::fs::remove_dir_all(&dest)?;
    }
    write_plugin_files(&dest, &manifest_bytes, &wasm_bytes, wasm_name)?;
    Ok(manifest.id)
}

/// Install from an explicit HTTPS base URL pointing at a plugin directory (manifest.toml + wasm).
pub async fn install_from_plugin_base_url(data_dir: &Path, base_url: &str) -> anyhow::Result<String> {
    let base = base_url.trim().trim_end_matches('/');
    if !base.starts_with("https://cdn.jsdelivr.net/gh/azerothl/Akasha_plugins@")
        && !base.starts_with("https://raw.githubusercontent.com/azerothl/Akasha_plugins/")
    {
        anyhow::bail!("plugin base URL not in allowlist (Akasha_plugins CDN only)");
    }
    let manifest_url = format!("{base}/manifest.toml");
    let manifest_bytes = fetch_bytes(&manifest_url).await?;
    let manifest_str = String::from_utf8(manifest_bytes.clone())
        .map_err(|_| anyhow::anyhow!("manifest is not UTF-8"))?;
    let manifest: PluginManifest = toml::from_str(&manifest_str)?;
    if !is_safe_plugin_id(&manifest.id) {
        anyhow::bail!("invalid plugin id in manifest");
    }
    let wasm_name = manifest
        .wasm_path
        .as_deref()
        .unwrap_or("plugin.wasm");
    let wasm_url = format!("{base}/{wasm_name}");
    let wasm_bytes = fetch_bytes(&wasm_url).await?;
    let plugins_dir = data_dir.join("plugins");
    let dest = plugins_dir.join(&manifest.id);
    if dest.exists() {
        std::fs::remove_dir_all(&dest)?;
    }
    write_plugin_files(&dest, &manifest_bytes, &wasm_bytes, wasm_name)?;
    Ok(manifest.id)
}

pub fn plugins_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("plugins")
}
