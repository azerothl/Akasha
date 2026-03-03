//! Phase 5 — Plugin registry: load WASM from dir, list, call tool, reputation.

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
}

struct LoadedPlugin {
    manifest: PluginManifest,
    wasm: WasmPlugin,
}

impl PluginRegistry {
    pub fn new(plugins_dir: PathBuf, reputation: Arc<ReputationStore>) -> Self {
        Self {
            plugins_dir,
            plugins: std::sync::RwLock::new(HashMap::new()),
            reputation,
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
