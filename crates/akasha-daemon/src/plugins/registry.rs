//! Phase 5 — Plugin registry: load WASM from dir, list, call tool, reputation.

use akasha_core::TrustStore;
use akasha_plugin_api::{PluginManifest, PluginKind, PluginRoutingRule};
use akasha_plugin_host::WasmPlugin;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, info, warn};

use super::reputation::ReputationStore;

#[derive(Clone, serde::Serialize)]
pub struct PluginEntry {
    pub id: String,
    pub name: String,
    pub version: String,
    pub kind: String,
    pub enabled: bool,
    pub score: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
}

pub struct PluginRegistry {
    plugins_dir: PathBuf,
    plugins: std::sync::RwLock<HashMap<String, LoadedPlugin>>,
    reputation: Arc<ReputationStore>,
    trust_store: Option<Arc<TrustStore>>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MatchedRoutingRule {
    pub plugin_id: String,
    pub intent: Option<String>,
    pub preferred_tools: Vec<String>,
    pub forbidden_tools: Vec<String>,
    pub instruction: String,
    pub priority: u32,
}

struct LoadedPlugin {
    manifest: PluginManifest,
    wasm: WasmPlugin,
}

fn debug_log(hypothesis_id: &str, location: &str, message: &str, data: serde_json::Value) {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let enabled = *ENABLED.get_or_init(|| {
        std::env::var_os("AKASHA_DEBUG_LOG")
            .map(|v| {
                let v = v.to_string_lossy().to_ascii_lowercase();
                matches!(v.as_str(), "1" | "true" | "yes" | "on")
            })
            .unwrap_or(false)
    });
    if !enabled {
        return;
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let run_id =
        std::env::var("AKASHA_DEBUG_RUN_ID").unwrap_or_else(|_| "pre-fix".to_string());
    debug!(
        run_id = %run_id,
        hypothesis_id = hypothesis_id,
        location = location,
        message = message,
        timestamp = timestamp,
        data = %data,
        "plugin registry debug log"
    );
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
        let mut out: Vec<PluginEntry> = guard
            .values()
            .map(|p| {
                let disabled = self.reputation.is_disabled(&p.manifest.id);
                PluginEntry {
                    id: p.manifest.id.clone(),
                    name: p.manifest.name.clone(),
                    version: p.manifest.version.clone(),
                    kind: p.manifest.kind.to_string(),
                    enabled: !disabled,
                    score: self.reputation.score(&p.manifest.id),
                    disabled_reason: if disabled {
                        Some("reputation".to_string())
                    } else {
                        None
                    },
                }
            })
            .collect();

        // Add plugins that are currently disabled by reputation and therefore not loaded in memory.
        let mut known_ids: std::collections::HashSet<String> =
            out.iter().map(|p| p.id.clone()).collect();
        if let Ok(read_dir) = std::fs::read_dir(&self.plugins_dir) {
            for entry in read_dir.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                for name in &["manifest.toml", "manifest.json"] {
                    let manifest_path = path.join(name);
                    if !manifest_path.exists() {
                        continue;
                    }
                    if let Ok(manifest) = PluginManifest::load_from_path(&manifest_path) {
                        if known_ids.contains(&manifest.id) {
                            break;
                        }
                        let disabled = self.reputation.is_disabled(&manifest.id);
                        if disabled {
                            known_ids.insert(manifest.id.clone());
                            out.push(PluginEntry {
                                id: manifest.id.clone(),
                                name: manifest.name.clone(),
                                version: manifest.version.clone(),
                                kind: manifest.kind.to_string(),
                                enabled: false,
                                score: self.reputation.score(&manifest.id),
                                disabled_reason: Some("reputation".to_string()),
                            });
                        }
                    }
                    break;
                }
            }
        }

        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    pub fn manifests(&self) -> Vec<PluginManifest> {
        let guard = self.plugins.read().unwrap();
        guard.values().map(|p| p.manifest.clone()).collect()
    }

    /// Match routing rules declared by installed plugins against user message + semantic intents.
    pub fn match_routing_rules(
        &self,
        message: &str,
        active_intents: &[&str],
        can_use_tool: impl Fn(&str) -> bool,
    ) -> Vec<MatchedRoutingRule> {
        let lower = message.to_lowercase();
        let guard = self.plugins.read().unwrap();
        let mut out: Vec<MatchedRoutingRule> = Vec::new();

        for (plugin_id, loaded) in guard.iter() {
            if loaded.manifest.kind != PluginKind::Tool {
                continue;
            }
            if loaded.manifest.routing_rules.is_empty() {
                continue;
            }

            for rule in &loaded.manifest.routing_rules {
                if !rule_matches(rule, &lower, active_intents) {
                    continue;
                }

                if !rule.preferred_tools.is_empty()
                    && !rule.preferred_tools.iter().any(|t| can_use_tool(t) || can_use_tool("plugin.call"))
                {
                    continue;
                }

                let instruction = if rule.instruction.trim().is_empty() {
                    let preferred = if rule.preferred_tools.is_empty() {
                        "".to_string()
                    } else {
                        format!(
                            " Prefer TOOL: {}.",
                            rule.preferred_tools.join(" or ")
                        )
                    };
                    let forbidden = if rule.forbidden_tools.is_empty() {
                        "".to_string()
                    } else {
                        format!(" Avoid tools: {}.", rule.forbidden_tools.join(", "))
                    };
                    format!(
                        "[Plugin routing reminder from {}.{}{}]",
                        plugin_id, preferred, forbidden
                    )
                } else {
                    rule.instruction.clone()
                };

                out.push(MatchedRoutingRule {
                    plugin_id: plugin_id.clone(),
                    intent: rule.intent.clone(),
                    preferred_tools: rule.preferred_tools.clone(),
                    forbidden_tools: rule.forbidden_tools.clone(),
                    instruction,
                    priority: rule.priority,
                });
            }
        }

        out.sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.plugin_id.cmp(&b.plugin_id)));
        out
    }

    /// Call a tool plugin by id. Updates reputation on success/failure/crash.
    pub fn call_tool(&self, plugin_id: &str, input: &str) -> Result<String, akasha_plugin_api::PluginError> {
        // #region agent log
        debug_log(
            "H4",
            "crates/akasha-daemon/src/plugins/registry.rs:301",
            "Plugin call requested",
            serde_json::json!({
                "plugin_id": plugin_id,
                "input_len": input.len()
            }),
        );
        // #endregion
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
        // #region agent log
        debug_log(
            "H3",
            "crates/akasha-daemon/src/plugins/registry.rs:325",
            "Plugin call completed",
            serde_json::json!({
                "plugin_id": plugin_id,
                "result": match &result {
                    Ok(_) => "ok",
                    Err(e) => {
                        if matches!(e, akasha_plugin_api::PluginError::Crashed) {
                            "crashed"
                        } else {
                            "error"
                        }
                    }
                }
            }),
        );
        // #endregion
        result
    }

    pub fn reload(&self) {
        self.load_all();
    }

    pub fn reset_reputation(&self, plugin_id: &str) -> std::io::Result<()> {
        self.reputation.reset(plugin_id)
    }

    pub fn reset_all_reputation(&self) -> std::io::Result<()> {
        self.reputation.reset_all()
    }
}

fn rule_matches(rule: &PluginRoutingRule, message_lower: &str, active_intents: &[&str]) -> bool {
    let intent_match = rule
        .intent
        .as_deref()
        .map(|intent| active_intents.iter().any(|i| i.eq_ignore_ascii_case(intent)))
        .unwrap_or(false);
    let keyword_match = !rule.keywords.is_empty()
        && rule
            .keywords
            .iter()
            .filter(|k| !k.trim().is_empty())
            .any(|k| message_lower.contains(&k.to_lowercase()));

    if rule.intent.is_some() || !rule.keywords.is_empty() {
        intent_match || keyword_match
    } else {
        false
    }
}
