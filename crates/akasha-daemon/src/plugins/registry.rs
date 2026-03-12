//! Phase 5 — Plugin registry: load WASM from dir, list, call tool, reputation.

use akasha_core::TrustStore;
use akasha_plugin_api::{PluginManifest, PluginKind};
use akasha_plugin_host::WasmPlugin;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{info, warn};

use super::reputation::ReputationStore;

#[derive(Clone, serde::Serialize)]
pub struct PluginEntry {
    pub id: String,
    pub name: String,
    pub version: String,
    pub kind: String,
    pub enabled: bool,
    pub score: u32,
}

pub struct PluginRegistry {
    plugins_dir: PathBuf,
    plugins: std::sync::RwLock<HashMap<String, LoadedPlugin>>,
    reputation: Arc<ReputationStore>,
    trust_store: Option<Arc<TrustStore>>,
}

struct LoadedPlugin {
    manifest: PluginManifest,
    wasm: WasmPlugin,
}

impl PluginRegistry {
    pub fn new(
        plugins_dir: PathBuf,
        reputation: Arc<ReputationStore>,
        trust_store: Option<Arc<TrustStore>>,
    ) -> Self {
        Self {
            plugins_dir,
            plugins: std::sync::RwLock::new(HashMap::new()),
            reputation,
            trust_store,
        }
    }

    /// Load all plugins from plugins_dir (scan for manifest.toml / manifest.json per subdir or root).
    pub fn load_all(&self) {
        let mut plugins = self.plugins.write().unwrap();
        plugins.clear();
        if !self.plugins_dir.exists() {
            if let Err(e) = std::fs::create_dir_all(&self.plugins_dir) {
                warn!(error = %e, "Could not create plugins dir");
            }
            return;
        }
        let read_dir = match std::fs::read_dir(&self.plugins_dir) {
            Ok(d) => d,
            Err(e) => {
                warn!(error = %e, "Could not read plugins dir");
                return;
            }
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.is_dir() {
                for name in &["manifest.toml", "manifest.json"] {
                    let manifest_path = path.join(name);
                    if manifest_path.exists() {
                        if let Ok(manifest) = PluginManifest::load_from_path(&manifest_path) {
                            if self.reputation.is_disabled(&manifest.id) {
                                info!(id = %manifest.id, "Plugin disabled (reputation), skipping");
                                continue;
                            }
                            let wasm_path = manifest.wasm_path.as_ref().map(|p| path.join(p)).unwrap_or_else(|| path.join("plugin.wasm"));
                            if let Some(ref store) = self.trust_store {
                                if store.requires_signing() {
                                    let wasm_bytes = match std::fs::read(&wasm_path) {
                                        Ok(b) => b,
                                        Err(_) => {
                                            warn!(id = %manifest.id, path = ?wasm_path, "Failed to read WASM for signature check");
                                            continue;
                                        }
                                    };
                                    let sig_path = wasm_path.with_extension("wasm.sig");
                                    let sig_path = if sig_path.exists() { sig_path } else { wasm_path.with_extension("sig") };
                                    let sig = match std::fs::read(&sig_path) {
                                        Ok(s) if s.len() == 64 => s,
                                        Ok(_) => {
                                            warn!(id = %manifest.id, "Plugin signature file invalid length (expected 64 bytes), skipping");
                                            continue;
                                        }
                                        Err(_) => {
                                            warn!(id = %manifest.id, "Plugin unsigned (no .sig file) and trust store requires signing, skipping");
                                            continue;
                                        }
                                    };
                                    if store.verify_plugin(&wasm_bytes, &sig).is_err() {
                                        warn!(id = %manifest.id, "Plugin signature verification failed, skipping");
                                        continue;
                                    }
                                }
                            }
                            if let Ok(wasm) = WasmPlugin::load(&wasm_path) {
                                let loaded = LoadedPlugin {
                                    manifest: manifest.clone(),
                                    wasm: wasm.with_manifest(manifest.clone()),
                                };
                                plugins.insert(manifest.id.clone(), loaded);
                                info!(id = %manifest.id, kind = ?manifest.kind, "Plugin loaded");
                            } else {
                                warn!(id = %manifest.id, path = ?wasm_path, "Failed to load WASM");
                            }
                        }
                        break;
                    }
                }
            }
        }
    }

    pub fn list(&self) -> Vec<PluginEntry> {
        let guard = self.plugins.read().unwrap();
        guard
            .values()
            .map(|p| PluginEntry {
                id: p.manifest.id.clone(),
                name: p.manifest.name.clone(),
                version: p.manifest.version.clone(),
                kind: p.manifest.kind.to_string(),
                enabled: !self.reputation.is_disabled(&p.manifest.id),
                score: self.reputation.score(&p.manifest.id),
            })
            .collect()
    }

    /// Call a tool plugin by id. Updates reputation on success/failure/crash.
    pub fn call_tool(&self, plugin_id: &str, input: &str) -> Result<String, akasha_plugin_api::PluginError> {
        if self.reputation.is_disabled(plugin_id) {
            return Err(akasha_plugin_api::PluginError::Disabled);
        }
        let guard = self.plugins.read().unwrap();
        let loaded = guard.get(plugin_id).ok_or_else(|| akasha_plugin_api::PluginError::Message("plugin not found".into()))?;
        if loaded.manifest.kind != PluginKind::Tool {
            return Err(akasha_plugin_api::PluginError::Message("not a tool plugin".into()));
        }
        let result = loaded.wasm.run(input);
        drop(guard);
        match &result {
            Ok(_) => self.reputation.record_success(plugin_id),
            Err(akasha_plugin_api::PluginError::Crashed) => self.reputation.record_crash(plugin_id),
            Err(_) => self.reputation.record_failure(plugin_id),
        }
        result
    }

    pub fn reload(&self) {
        self.load_all();
    }
}
