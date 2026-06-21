//! Home Assistant connector helpers: credential injection for plugin calls.

use akasha_vault::Vault;
use std::path::Path;

/// Merge `ha_base_url` and `ha_access_token` into plugin JSON when connector is enabled.
pub fn enrich_homeassistant_plugin_payload(data_dir: &Path, payload: &str) -> String {
    if !crate::connectors_config::homeassistant_enabled_in_file(data_dir) {
        return payload.to_string();
    }
    let mut value = match serde_json::from_str::<serde_json::Value>(payload) {
        Ok(v) => v,
        Err(_) => return payload.to_string(),
    };
    let obj = match value.as_object_mut() {
        Some(o) => o,
        None => return payload.to_string(),
    };
    if let Some(url) = crate::connectors_config::ha_base_url(data_dir) {
        if !url.is_empty() {
            obj.insert(
                "ha_base_url".into(),
                serde_json::Value::String(url),
            );
        }
    }
    if let Ok(vault) = akasha_vault::open_vault(data_dir) {
        if let Ok(token) = vault.get("ha_access_token") {
            if !token.trim().is_empty() {
                obj.insert(
                    "ha_access_token".into(),
                    serde_json::Value::String(token),
                );
            }
        }
    }
    value.to_string()
}

/// If HA connector enabled and URL empty, discover and persist first local/network URL.
pub async fn autofill_ha_base_url_if_needed(data_dir: &Path) {
    if !crate::connectors_config::homeassistant_enabled_in_file(data_dir) {
        return;
    }
    if crate::connectors_config::ha_base_url(data_dir)
        .map(|u| !u.trim().is_empty())
        .unwrap_or(false)
    {
        return;
    }
    let profile = match akasha_core::service_discovery::profile("homeassistant") {
        Some(p) => p,
        None => return,
    };
    let opts = akasha_core::service_discovery::DiscoveryOptions::from_env();
    let entries = akasha_core::service_discovery::discover(profile, &opts).await;
    if let Some(first) = entries.first() {
        if let Err(e) = crate::connectors_config::set_ha_base_url(data_dir, &first.base_url) {
            tracing::warn!(error = %e, "failed to persist discovered HA_BASE_URL");
        } else {
            tracing::info!(url = %first.base_url, "Home Assistant URL set from discovery");
        }
    }
}
