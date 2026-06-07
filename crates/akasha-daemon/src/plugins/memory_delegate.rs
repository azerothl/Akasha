//! Memory plugin HTTP delegation: fulfill [`MemoryDelegateRequest`] via loopback `/api/memory/*`.

use akasha_plugin_api::{MemoryDelegateRequest, MemoryDelegateResponse, PluginError};
use std::time::Duration;

fn daemon_base_url() -> String {
    let port = std::env::var("AKASHA_PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(3876);
    format!("http://127.0.0.1:{port}")
}

fn http_client() -> Result<reqwest::blocking::Client, PluginError> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| PluginError::Message(format!("http client: {e}")))
}

/// Fulfill a memory delegate envelope against the local daemon memory API.
pub fn fulfill_memory_delegate(req: &MemoryDelegateRequest) -> Result<MemoryDelegateResponse, PluginError> {
    let op = req.operation.trim().to_ascii_lowercase();
    if op.is_empty() {
        return Err(PluginError::InvalidInput);
    }
    let client = http_client()?;
    let base = daemon_base_url();
    match op.as_str() {
        "search" | "retrieve" => {
            let q = req
                .key
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or(PluginError::InvalidInput)?;
            let url = format!("{base}/api/memory/search?q={}&top_k=5", urlencoding::encode(q));
            let resp = client
                .get(&url)
                .send()
                .map_err(|e| PluginError::Message(format!("memory search: {e}")))?;
            let body = resp
                .text()
                .map_err(|e| PluginError::Message(format!("memory search body: {e}")))?;
            Ok(MemoryDelegateResponse {
                ok: true,
                value: Some(body),
                detail: None,
            })
        }
        "delete" => {
            let id = req
                .key
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or(PluginError::InvalidInput)?;
            let url = format!("{base}/api/memory/long-term/{id}");
            let resp = client
                .delete(&url)
                .send()
                .map_err(|e| PluginError::Message(format!("memory delete: {e}")))?;
            let body = resp
                .text()
                .map_err(|e| PluginError::Message(format!("memory delete body: {e}")))?;
            Ok(MemoryDelegateResponse {
                ok: true,
                value: Some(body),
                detail: None,
            })
        }
        "store" => {
            let content = req
                .value
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or(PluginError::InvalidInput)?;
            let source = req
                .key
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("plugin:memory");
            let bundle = serde_json::json!({
                "schema_version": crate::memory_export::EXPORT_SCHEMA_VERSION,
                "exported_at": chrono::Utc::now().to_rfc3339(),
                "entries": [{
                    "id": uuid::Uuid::new_v4().to_string(),
                    "content": content,
                    "source": source,
                    "created_at": chrono::Utc::now().to_rfc3339(),
                    "importance": null,
                    "scope": null
                }],
                "facts": [],
                "episodic": []
            });
            let url = format!("{base}/api/memory/import");
            let resp = client
                .post(&url)
                .json(&bundle)
                .send()
                .map_err(|e| PluginError::Message(format!("memory store: {e}")))?;
            let body = resp
                .text()
                .map_err(|e| PluginError::Message(format!("memory store body: {e}")))?;
            Ok(MemoryDelegateResponse {
                ok: true,
                value: Some(body),
                detail: None,
            })
        }
        _ => Err(PluginError::Message(format!(
            "unsupported memory delegate operation: {op}"
        ))),
    }
}
