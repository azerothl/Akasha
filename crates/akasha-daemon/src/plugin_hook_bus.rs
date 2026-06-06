//! Plugin hook bus: dispatch daemon lifecycle events to opted-in WASM plugins.

use akasha_plugin_api::PluginManifest;
use akasha_plugin_host::WasmPlugin;
use std::path::Path;

fn hook_payload(event_name: &str, payload_json: &str) -> String {
    let payload_value = serde_json::from_str::<serde_json::Value>(payload_json)
        .unwrap_or_else(|_| serde_json::json!({ "raw": payload_json }));
    serde_json::json!({
        "event_name": event_name,
        "payload": payload_value,
    })
    .to_string()
}

/// Dispatch one hook event to all enabled plugins that subscribe via `manifest.hook_events`.
pub fn dispatch_hook_event(data_dir: &Path, event_name: &str, payload_json: &str) {
    let plugins_dir = data_dir.join("plugins");
    if !plugins_dir.is_dir() {
        return;
    }
    let state = match crate::plugins::state::PluginStateStore::open(data_dir) {
        Ok(v) => v,
        Err(_) => return,
    };
    let rep = match crate::plugins::reputation::ReputationStore::open(data_dir) {
        Ok(v) => v,
        Err(_) => return,
    };
    let input = hook_payload(event_name, payload_json);
    let read_dir = match std::fs::read_dir(&plugins_dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for entry in read_dir.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let mut manifest: Option<PluginManifest> = None;
        for name in ["manifest.toml", "manifest.json"] {
            let p = dir.join(name);
            if !p.is_file() {
                continue;
            }
            manifest = PluginManifest::load_from_path(&p).ok();
            if manifest.is_some() {
                break;
            }
        }
        let Some(manifest) = manifest else {
            continue;
        };
        if state.is_disabled(&manifest.id) || rep.is_disabled(&manifest.id) {
            continue;
        }
        if !manifest
            .hook_events
            .iter()
            .any(|e| e.eq_ignore_ascii_case(event_name))
        {
            continue;
        }
        let wasm_path = manifest
            .wasm_path
            .as_ref()
            .map(|p| dir.join(p))
            .unwrap_or_else(|| dir.join("plugin.wasm"));
        let Ok(wasm) = WasmPlugin::load(&wasm_path) else {
            continue;
        };
        let wasm = wasm.with_manifest(manifest.clone());
        match wasm.run(&input) {
            Ok(out) => tracing::debug!(
                plugin_id = %manifest.id,
                event_name,
                output_preview = %out.chars().take(400).collect::<String>(),
                "plugin hook dispatched"
            ),
            Err(err) => tracing::warn!(
                plugin_id = %manifest.id,
                event_name,
                error = %err,
                "plugin hook dispatch failed"
            ),
        }
    }
}
