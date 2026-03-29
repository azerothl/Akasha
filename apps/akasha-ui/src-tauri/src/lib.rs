#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use enigo::{Axis, Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};

const DAEMON_PORT: u16 = 3876;
const TASK_POLL_INTERVAL_MS: u64 = 1500;
const TASK_POLL_TIMEOUT_SECS: u64 = 600;

fn daemon_base_url(port: u16) -> String {
    format!("http://127.0.0.1:{}", port)
}

/// Shared HTTP client for all daemon requests (avoids creating a new client per command).
fn http_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("HTTP client init")
    })
}

/// Health check: GET / returns {"status":"ok"} when daemon is up.
#[tauri::command]
async fn check_health(port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/", daemon_base_url(port));
    let client = http_client();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if resp.status().is_success() {
        let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
        Ok(serde_json::json!({ "ok": true, "port": port, "body": json }))
    } else {
        Ok(serde_json::json!({ "ok": false, "port": port, "status": resp.status().as_u16() }))
    }
}

/// Router metrics: GET /api/router/metrics returns { "provider::model": { total_requests, ... } }.
/// If period is Some("day"|"week"|"month"|"year"), appends ?period= for filtered aggregates from persisted store.
#[tauri::command]
async fn get_router_metrics(port: Option<u16>, period: Option<String>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = match period.as_deref().filter(|p| !p.is_empty()) {
        Some(p) => format!("{}/api/router/metrics?period={}", daemon_base_url(port), p),
        None => format!("{}/api/router/metrics", daemon_base_url(port)),
    };
    let client = http_client();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("Daemon returned {}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

#[derive(serde::Serialize)]
struct SendMessageResult {
    reply: String,
    session_id: String,
}

/// Non-blocking: POST /api/message and return immediately with task_id + session_id (FR-025).
#[derive(serde::Serialize)]
struct SendMessageAckResult {
    ack: bool,
    task_id: String,
    session_id: String,
    message: String,
}

#[derive(serde::Deserialize)]
struct AttachmentPayload {
    #[serde(rename = "type")]
    typ: Option<String>,
    name: Option<String>,
    content_base64: Option<String>,
    mime_type: Option<String>,
}

#[tauri::command]
async fn send_message_ack(
    message: String,
    session_id: Option<String>,
    attachments: Option<Vec<AttachmentPayload>>,
    port: Option<u16>,
) -> Result<SendMessageAckResult, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let base = daemon_base_url(port);
    let url = format!("{}/api/message", base);
    let client = http_client();
    // Si pièces jointes présentes et message vide, envoyer un libellé pour que la tâche reçoive un contenu (évite "message": "").
    let message_for_body = match attachments.as_deref() {
        Some(a) if !a.is_empty() && message.trim().is_empty() => "(Pièce(s) jointe(s))".to_string(),
        _ => message,
    };
    let mut body = match session_id.as_deref() {
        Some(s) if !s.is_empty() => serde_json::json!({ "message": message_for_body, "session_id": s }),
        _ => serde_json::json!({ "message": message_for_body }),
    };
    if let Some(ref atts) = attachments {
        if !atts.is_empty() {
            let arr: Vec<serde_json::Value> = atts
                .iter()
                .map(|a| {
                    let typ = a.typ.as_deref().unwrap_or("document");
                    let name = a.name.as_deref().unwrap_or("file");
                    serde_json::json!({
                        "type": typ,
                        "name": name,
                        "content_base64": a.content_base64.as_deref().unwrap_or(""),
                        "mime_type": a.mime_type.as_deref().unwrap_or("application/octet-stream")
                    })
                })
                .collect();
            body["attachments"] = serde_json::Value::Array(arr);
        }
    }
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Daemon unreachable: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("Daemon returned {}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let task_id = json.get("task_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let session_id = json.get("session_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let message = json.get("message").and_then(|v| v.as_str()).unwrap_or("Request received. You can follow progress in the Tasks tab.").to_string();
    Ok(SendMessageAckResult {
        ack: true,
        task_id,
        session_id,
        message,
    })
}

#[tauri::command]
async fn send_message(message: String, session_id: Option<String>, port: Option<u16>) -> Result<SendMessageResult, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let base = daemon_base_url(port);
    let url = format!("{}/api/message", base);
    let client = http_client();
    let body = match session_id.as_deref() {
        Some(s) if !s.is_empty() => serde_json::json!({ "message": message, "session_id": s }),
        _ => serde_json::json!({ "message": message }),
    };
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Daemon unreachable: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("Daemon returned {}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let task_id = json.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
    let session_id = json.get("session_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if task_id.is_empty() {
        return Ok(SendMessageResult { reply: "Message received.".to_string(), session_id });
    }

    // Poll task until completed/failed or timeout; return final reply text and session_id (for memory).
    let task_url = format!("{}/api/tasks/{}", base, task_id);
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(TASK_POLL_TIMEOUT_SECS);
    let mut last_message = String::new();
    loop {
        if std::time::Instant::now() > deadline {
            return Ok(SendMessageResult {
                reply: if last_message.is_empty() { "Request timed out.".to_string() } else { last_message },
                session_id,
            });
        }
        tokio::time::sleep(std::time::Duration::from_millis(TASK_POLL_INTERVAL_MS)).await;
        let poll = client
            .get(&task_url)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await;
        let resp = match poll {
            Ok(r) => r,
            Err(_) => continue,
        };
        if !resp.status().is_success() {
            continue;
        }
        let task_json: serde_json::Value = match resp.json().await {
            Ok(j) => j,
            Err(_) => continue,
        };
        let status = task_json.get("status").and_then(|v| v.as_str()).unwrap_or("");
        if let Some(progress) = task_json.get("progress").and_then(|p| p.as_array()) {
            if let Some(last) = progress.last() {
                if let Some(msg) = last.get("message").and_then(|m| m.as_str()) {
                    last_message = msg.to_string();
                }
            }
        }
        if status == "completed" {
            return Ok(SendMessageResult {
                reply: if last_message.is_empty() { "Done.".to_string() } else { last_message },
                session_id,
            });
        }
        if status == "failed" {
            return Ok(SendMessageResult {
                reply: if last_message.is_empty() { "Task failed.".to_string() } else { last_message },
                session_id,
            });
        }
    }
}

/// GET /api/config — vars from akasha.env (for slash /config list|get).
#[tauri::command]
async fn get_config(port: Option<u16>) -> Result<std::collections::HashMap<String, String>, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/config", daemon_base_url(port));
    let client = http_client();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let vars = json
        .get("vars")
        .and_then(|v| v.as_object())
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    Ok(vars)
}

/// POST /api/config — set one var (for slash /config set).
#[tauri::command]
async fn set_config(key: String, value: String, port: Option<u16>) -> Result<(), String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/config", daemon_base_url(port));
    let client = http_client();
    let body = serde_json::json!({ "key": key, "value": value });
    let resp = client.post(&url).json(&body).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    Ok(())
}

/// GET /api/vault/keys — list key names (for slash /vault list).
#[tauri::command]
async fn get_vault_keys(port: Option<u16>) -> Result<Vec<String>, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/vault/keys", daemon_base_url(port));
    let client = http_client();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let keys = json
        .get("keys")
        .and_then(|k| k.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    Ok(keys)
}

/// GET /api/router/ollama/models (for slash /models).
#[tauri::command]
async fn get_ollama_models(port: Option<u16>) -> Result<Vec<String>, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/router/ollama/models", daemon_base_url(port));
    let client = http_client();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let empty: Vec<serde_json::Value> = vec![];
    let list = json.get("models").and_then(|m| m.as_array()).unwrap_or(&empty);
    // API returns models as array of strings and/or objects with "name" or "model"
    let names: Vec<String> = list
        .iter()
        .filter_map(|m| {
            m.as_str()
                .map(String::from)
                .or_else(|| m.get("name").and_then(|n| n.as_str()).map(String::from))
                .or_else(|| m.get("model").and_then(|n| n.as_str()).map(String::from))
        })
        .collect();
    Ok(names)
}

/// GET /api/router/models — list models from all providers (Ollama live + config for others).
#[tauri::command]
async fn get_router_models(port: Option<u16>) -> Result<std::collections::HashMap<String, Vec<String>>, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/router/models", daemon_base_url(port));
    let client = http_client();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let providers = json
        .get("providers")
        .and_then(|p| p.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| {
                    v.as_array().map(|arr| {
                        let models: Vec<String> = arr
                            .iter()
                            .filter_map(|m| m.as_str().map(String::from))
                            .collect();
                        (k.clone(), models)
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(providers)
}

/// GET /api/router/routes — primary + fallback per category (for /routes, /models list).
#[tauri::command]
async fn get_router_routes(port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/router/routes", daemon_base_url(port));
    let client = http_client();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// POST /api/router/route — set primary provider/model for a category (for /models set).
#[tauri::command]
async fn set_router_route(category: String, provider: String, model: String, port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/router/route", daemon_base_url(port));
    let client = http_client();
    let body = serde_json::json!({ "category": category, "provider": provider, "model": model });
    let resp = client.post(&url).json(&body).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let err = resp.text().await.unwrap_or_default();
        return Err(err);
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// GET /api/voice/status — whether TTS/STT are configured (voice_router.yaml).
#[tauri::command]
async fn get_voice_status(port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/voice/status", daemon_base_url(port));
    let client = http_client();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

#[derive(serde::Deserialize)]
struct VoiceSttPayload {
    #[serde(default)]
    data_url: Option<String>,
    #[serde(default)]
    audio_base64: Option<String>,
}

/// POST /api/voice/stt — transcribe audio to text. Body: { "data_url" } or { "audio_base64" }.
#[tauri::command]
async fn voice_stt_transcribe(port: Option<u16>, payload: VoiceSttPayload) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/voice/stt", daemon_base_url(port));
    let body = if let Some(ref u) = payload.data_url.filter(|s| !s.is_empty()) {
        serde_json::json!({ "data_url": u })
    } else if let Some(ref b) = payload.audio_base64.filter(|s| !s.is_empty()) {
        serde_json::json!({ "audio_base64": b })
    } else {
        return Err("data_url or audio_base64 required".to_string());
    };
    let client = http_client();
    let resp = client.post(&url).json(&body).send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    if !status.is_success() {
        let err_body: serde_json::Value = resp.json().await.unwrap_or(serde_json::json!({ "error": status.to_string() }));
        return Err(err_body.get("error").and_then(|v| v.as_str()).unwrap_or("STT failed").to_string());
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// POST /api/voice/tts — synthesize text to audio. Returns { "data_url": "data:audio/wav;base64,...", "message": "..." }.
#[tauri::command]
async fn voice_tts(port: Option<u16>, text: String) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/voice/tts", daemon_base_url(port));
    let body = serde_json::json!({ "text": text.trim() });
    let client = http_client();
    let resp = client.post(&url).json(&body).send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    if !status.is_success() {
        let err_body: serde_json::Value = resp.json().await.unwrap_or(serde_json::json!({ "error": status.to_string() }));
        return Err(err_body.get("error").and_then(|v| v.as_str()).unwrap_or("TTS failed").to_string());
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// GET /api/router/embedded-status — embedded LLM status (for /embedded).
#[tauri::command]
async fn get_embedded_status(port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/router/embedded-status", daemon_base_url(port));
    let client = http_client();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// POST /api/router/embedded/reload — unload embedded model (for /embedded reload).
#[tauri::command]
async fn embedded_reload(port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/router/embedded/reload", daemon_base_url(port));
    let client = http_client();
    let resp = client.post(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// GET /api/doctor — health checks (for slash /doctor).
#[tauri::command]
async fn get_doctor(port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/doctor", daemon_base_url(port));
    let client = http_client();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// GET /api/update/status — cached latest version info from daemon (for update banner).
#[tauri::command]
async fn get_update_status(port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/update/status", daemon_base_url(port));
    let client = http_client();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// App version (from Cargo.toml) for update comparison.
#[tauri::command]
fn get_app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Open a local file or directory with the default application (or file manager for directory).
/// Only allows absolute local paths; path is canonicalized and must exist.
#[tauri::command]
fn open_path(path: String) -> Result<(), String> {
    let path = path.trim().trim_matches('"');
    if path.is_empty() {
        return Err("Path is empty".to_string());
    }
    let p = std::path::Path::new(path);
    let canonical = p.canonicalize().map_err(|e| format!("Invalid path: {}", e))?;
    if !canonical.is_absolute() {
        return Err("Only absolute paths are allowed".to_string());
    }
    let path_str = canonical.to_string_lossy();
    let status_result = match std::env::consts::OS {
        "windows" => std::process::Command::new("cmd").args(["/c", "start", "", path_str.as_ref()]).status(),
        "macos" => std::process::Command::new("open").arg(path_str.as_ref()).status(),
        _ => std::process::Command::new("xdg-open").arg(path_str.as_ref()).status(),
    };
    let exit_status = status_result.map_err(|e| format!("Failed to launch system opener: {}", e))?;
    if !exit_status.success() {
        return Err(format!("System opener exited with status: {}", exit_status));
    }
    Ok(())
}

/// Read a local image file and return a data URL (data:image/xxx;base64,...). Only allows image extensions.
#[tauri::command]
fn read_file_as_data_url(path: String) -> Result<String, String> {
    let path = path.trim().trim_matches('"');
    if path.is_empty() {
        return Err("Path is empty".to_string());
    }
    let p = std::path::Path::new(path);
    let canonical = p.canonicalize().map_err(|e| format!("Invalid path: {}", e))?;
    if !canonical.is_absolute() {
        return Err("Only absolute paths are allowed".to_string());
    }
    let ext = canonical.extension().and_then(|e| e.to_str()).unwrap_or("");
    let mime = match ext.to_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => return Err("Only image files (png, jpg, gif, webp) are allowed".to_string()),
    };
    const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024; // 20 MiB
    let metadata = std::fs::metadata(&canonical).map_err(|e| e.to_string())?;
    if metadata.len() > MAX_IMAGE_BYTES {
        return Err(format!("File too large: {} bytes (max {} bytes)", metadata.len(), MAX_IMAGE_BYTES));
    }
    let bytes = std::fs::read(&canonical).map_err(|e| e.to_string())?;
    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);
    Ok(format!("data:{};base64,{}", mime, b64))
}

/// Open the parent directory of the given path in the file manager (reveal in folder).
/// If the path is a directory, opens that directory. Only allows absolute local paths.
#[tauri::command]
fn open_path_in_explorer(path: String) -> Result<(), String> {
    let path = path.trim().trim_matches('"');
    if path.is_empty() {
        return Err("Path is empty".to_string());
    }
    let p = std::path::Path::new(path);
    let canonical = p.canonicalize().map_err(|e| format!("Invalid path: {}", e))?;
    if !canonical.is_absolute() {
        return Err("Only absolute paths are allowed".to_string());
    }
    let dir = if canonical.is_dir() {
        canonical.clone()
    } else {
        canonical.parent().map(|x| x.to_path_buf()).unwrap_or(canonical)
    };
    let path_str = dir.to_string_lossy();
    let _ = match std::env::consts::OS {
        "windows" => std::process::Command::new("explorer").arg(path_str.as_ref()).status(),
        "macos" => std::process::Command::new("open").arg(path_str.as_ref()).status(),
        _ => std::process::Command::new("xdg-open").arg(path_str.as_ref()).status(),
    };
    Ok(())
}

/// Open URL in default browser. Only allows https URLs for known update/release hosts.
#[tauri::command]
fn open_url(url: String) -> Result<(), String> {
    let url = url.trim();
    if !url.starts_with("https://") {
        return Err("Only https URLs are allowed".to_string());
    }
    let parsed = url.parse::<url::Url>().map_err(|e| format!("Invalid URL: {}", e))?;
    let host = parsed.host_str().unwrap_or("");
    if !host.ends_with("github.io") && !host.ends_with("github.com") && host != "ollama.com" {
        return Err("URL host not allowed for security".to_string());
    }
    let _ = match std::env::consts::OS {
        "windows" => std::process::Command::new("cmd").args(["/c", "start", "", url]).status(),
        "macos" => std::process::Command::new("open").arg(url).status(),
        _ => std::process::Command::new("xdg-open").arg(url).status(),
    };
    Ok(())
}

/// POST /api/diagnostic/advice — get advice from health (for slash /advice).
#[tauri::command]
async fn get_advice(health: serde_json::Value, port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/diagnostic/advice", daemon_base_url(port));
    let client = http_client();
    let body = serde_json::json!({ "health": health });
    let resp = client
        .post(&url)
        .timeout(std::time::Duration::from_secs(180))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// GET /api/plugins (for slash /plugins).
#[tauri::command]
async fn get_plugins(port: Option<u16>) -> Result<Vec<serde_json::Value>, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/plugins", daemon_base_url(port));
    let client = http_client();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: Vec<serde_json::Value> = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// POST /api/plugins/reload (for slash /reload).
#[tauri::command]
async fn reload_plugins(port: Option<u16>) -> Result<(), String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/plugins/reload", daemon_base_url(port));
    let client = http_client();
    let resp = client.post(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    Ok(())
}

/// GET /api/skills — list installed skills. Returns array of { name, description, ... }.
#[tauri::command]
async fn get_skills(port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/skills", daemon_base_url(port));
    let client = http_client();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// POST /api/skills/reload — reload skills from disk (Agent Skills + YAML). Returns { count }.
#[tauri::command]
async fn reload_skills(port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/skills/reload", daemon_base_url(port));
    let client = http_client();
    let resp = client.post(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// POST /api/skills/install — install a skill from a URL. Body: { "url": "<skill_url>" }.
#[tauri::command]
async fn install_skill(url: String, port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let api_url = format!("{}/api/skills/install", daemon_base_url(port));
    let client = http_client();
    let body = serde_json::json!({ "url": url.trim() });
    let resp = client
        .post(&api_url)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let status = resp.status();
        let err_body = resp.text().await.unwrap_or_default();
        return Err(format!("{} — {}", status, err_body));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// POST /api/skills/uninstall — uninstall a skill by name. Body: { "name": "<skill_name>" }.
#[tauri::command]
async fn uninstall_skill(name: String, port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/skills/uninstall", daemon_base_url(port));
    let client = http_client();
    let body = serde_json::json!({ "name": name.trim() });
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let status = resp.status();
        let err_body = resp.text().await.unwrap_or_default();
        return Err(format!("{} — {}", status, err_body));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// POST /api/restart — request daemon restart (for slash /restart).
#[tauri::command]
async fn restart_daemon(port: Option<u16>) -> Result<(), String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/restart", daemon_base_url(port));
    let client = http_client();
    let resp = client.post(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    Ok(())
}

/// User documentation: GET /api/docs returns { "content": "..." } (markdown).
#[tauri::command]
async fn get_docs(port: Option<u16>) -> Result<String, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/docs", daemon_base_url(port));
    let client = http_client();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("Daemon returned {}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let content = json
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or("Documentation non disponible.")
        .to_string();
    Ok(content)
}

#[tauri::command]
async fn get_task_status(task_id: String, port: Option<u16>) -> Result<String, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/tasks/{}", daemon_base_url(port), task_id);
    let client = http_client();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let text = resp.text().await.map_err(|e| e.to_string())?;
    Ok(text)
}

/// Task list: GET /api/tasks returns { tasks: [ { id, status, ... } ] }.
#[tauri::command]
async fn get_tasks(port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/tasks", daemon_base_url(port));
    let client = http_client();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// Task events: GET /api/tasks/:id/events returns { task_id, events: [ { event_type, payload, at } ] }.
#[tauri::command]
async fn get_task_events(task_id: String, port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/tasks/{}/events", daemon_base_url(port), task_id);
    let client = http_client();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// Cancel a running or pending task: POST /api/tasks/:id/cancel.
#[tauri::command]
async fn cancel_task(task_id: String, port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/tasks/{}/cancel", daemon_base_url(port), task_id);
    let client = http_client();
    let resp = client.post(&url).send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    let json: serde_json::Value = resp.json().await.unwrap_or(serde_json::json!({ "error": "invalid_response" }));
    if !status.is_success() {
        let detail = json.get("detail").and_then(|v| v.as_str()).unwrap_or(json.get("error").and_then(|v| v.as_str()).unwrap_or("Erreur inconnue"));
        return Err(detail.to_string());
    }
    Ok(json)
}

/// Human in the loop: GET /api/pending-human-input — list all tasks waiting for user input (for notifications on load or when user was away).
#[tauri::command]
async fn get_pending_human_input(port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/pending-human-input", daemon_base_url(port));
    let client = http_client();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// Human in the loop: GET /api/tasks/:id/human-input — pending question/context/choices for the task (404 if none).
#[tauri::command]
async fn get_task_human_input(task_id: String, port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/tasks/{}/human-input", daemon_base_url(port), task_id);
    let client = http_client();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// Human in the loop: POST /api/tasks/:id/human-reply — submit user response to unblock the agent.
#[tauri::command]
async fn post_task_human_reply(task_id: String, response: String, port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/tasks/{}/human-reply", daemon_base_url(port), task_id);
    let client = http_client();
    let body = serde_json::json!({ "response": response });
    let resp = client.post(&url).json(&body).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// Schedules: GET /api/schedules (FR-028, Calendrier).
#[tauri::command]
async fn get_schedules(port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/schedules", daemon_base_url(port));
    let client = http_client();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// Schedule by id: GET /api/schedules/:id (détail d'une récurrence).
#[tauri::command]
async fn get_schedule_by_id(schedule_id: String, port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/schedules/{}", daemon_base_url(port), schedule_id);
    let client = http_client();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// Calendar events in range: GET /api/calendar/events?from=...&to=... (for calendar grid view).
#[tauri::command]
async fn get_calendar_events(port: Option<u16>, from: String, to: String) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let from_enc = urlencoding::encode(&from);
    let to_enc = urlencoding::encode(&to);
    let url = format!("{}/api/calendar/events?from={}&to={}", daemon_base_url(port), from_enc, to_enc);
    let client = http_client();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// Task runs: GET /api/task_runs (optionally ?schedule_id=...) for Calendrier.
#[tauri::command]
async fn get_task_runs(port: Option<u16>, schedule_id: Option<String>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = match schedule_id.as_deref() {
        Some(s) if !s.is_empty() => format!("{}/api/task_runs?schedule_id={}", daemon_base_url(port), s),
        _ => format!("{}/api/task_runs", daemon_base_url(port)),
    };
    let client = http_client();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// Create schedule: POST /api/schedules.
#[tauri::command]
async fn create_schedule(
    name: String,
    description: String,
    interval_seconds: Option<u64>,
    port: Option<u16>,
) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/schedules", daemon_base_url(port));
    let body = serde_json::json!({
        "name": name,
        "description": description,
        "enabled": true,
        "timezone": "UTC",
        "rrule": "",
        "interval_seconds": interval_seconds.unwrap_or(3600),
        "channel_context": description
    });
    let client = http_client();
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let status = resp.status();
        return Err(format!("{}", status));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// Update schedule: PUT /api/schedules/:id (e.g. channel_context / prompt).
#[tauri::command]
async fn put_schedule(
    schedule_id: String,
    port: Option<u16>,
    body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/schedules/{}", daemon_base_url(port), schedule_id);
    let client = http_client();
    let resp = client
        .put(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// Delete schedule: DELETE /api/schedules/:id.
#[tauri::command]
async fn delete_schedule(schedule_id: String, port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/schedules/{}", daemon_base_url(port), schedule_id);
    let client = http_client();
    let resp = client.delete(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    Ok(serde_json::json!({ "deleted": schedule_id }))
}

/// Memory short-term: GET /api/memory/short-term?session_id=...
#[tauri::command]
async fn get_memory_short_term(session_id: Option<String>, port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = match session_id.as_deref() {
        Some(s) if !s.is_empty() => format!(
            "{}/api/memory/short-term?session_id={}",
            daemon_base_url(port),
            urlencoding::encode(s)
        ),
        _ => format!("{}/api/memory/short-term", daemon_base_url(port)),
    };
    let client = http_client();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// Memory long-term: GET /api/memory/long-term?limit=200&offset=0 (paginated)
#[tauri::command]
async fn get_memory_long_term(limit: Option<u32>, offset: Option<u32>, port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let limit = limit.unwrap_or(200).min(200);
    let offset = offset.unwrap_or(0);
    let url = format!("{}/api/memory/long-term?limit={}&offset={}", daemon_base_url(port), limit, offset);
    let client = http_client();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// Memory search: GET /api/memory/search?q=...&top_k=...
#[tauri::command]
async fn get_memory_search(q: String, top_k: Option<u32>, port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let q = q.trim();
    if q.is_empty() {
        return Err("missing or empty q".to_string());
    }
    let top_k = top_k.unwrap_or(10).min(20);
    let url = format!(
        "{}/api/memory/search?q={}&top_k={}",
        daemon_base_url(port),
        urlencoding::encode(q),
        top_k
    );
    let client = http_client();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// Memory long-term: DELETE /api/memory/long-term/:id
#[tauri::command]
async fn delete_memory_long_term(id: String, port: Option<u16>) -> Result<(), String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let id = id.trim();
    if id.is_empty() {
        return Err("missing id".to_string());
    }
    let url = format!("{}/api/memory/long-term/{}", daemon_base_url(port), id);
    let client = http_client();
    let resp = client.delete(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("{} {}", status, body));
    }
    Ok(())
}

/// Memory long-term: POST /api/memory/rebuild-relations — recompute "similar" relations for all entries
#[tauri::command]
async fn rebuild_memory_relations(port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/memory/rebuild-relations", daemon_base_url(port));
    let client = http_client();
    let resp = client.post(&url).send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        let err = json.get("error").and_then(|v| v.as_str()).unwrap_or("unknown");
        return Err(err.to_string());
    }
    Ok(json)
}

/// Schedule run reports: GET /api/schedule_run_reports — completed schedule runs with message (for chat).
#[tauri::command]
async fn get_schedule_run_reports(port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/schedule_run_reports", daemon_base_url(port));
    let client = http_client();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// User RAG: GET /api/user-rag/documents
#[tauri::command]
async fn get_user_rag_documents(port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/user-rag/documents", daemon_base_url(port));
    let client = http_client();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// User RAG: POST /api/user-rag/documents
#[tauri::command]
async fn add_user_rag_document(
    name: String,
    content_base64: String,
    mime_type: Option<String>,
    port: Option<u16>,
) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/user-rag/documents", daemon_base_url(port));
    let client = http_client();
    let body = serde_json::json!({
        "name": name,
        "content_base64": content_base64,
        "mime_type": mime_type.unwrap_or_else(|| "application/octet-stream".to_string())
    });
    let resp = client.post(&url).json(&body).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("{} {}", status, text));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// User RAG: DELETE /api/user-rag/documents/:id
#[tauri::command]
async fn delete_user_rag_document(id: String, port: Option<u16>) -> Result<(), String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/user-rag/documents/{}", daemon_base_url(port), id.trim());
    let client = http_client();
    let resp = client.delete(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    Ok(())
}

/// Device bridge: get oldest pending device request (for UI to fulfill camera, mic, etc.).
#[tauri::command]
async fn get_device_pending(port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/device/pending", daemon_base_url(port));
    let client = http_client();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        eprintln!("[device_bridge UI] GET {} -> {}", url, resp.status());
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// Device bridge: send result of device action (e.g. image or audio base64 from UI).
#[tauri::command]
async fn post_device_result(
    request_id: String,
    success: bool,
    data: Option<String>,
    port: Option<u16>,
) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/device/result", daemon_base_url(port));
    let body = serde_json::json!({
        "request_id": request_id,
        "success": success,
        "data": data,
    });
    let client = http_client();
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("{} {}", status, text));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// Agent profile: GET /api/agent-profile (name, personality, rules, can_do, cannot_do).
#[tauri::command]
async fn get_agent_profile(port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/agent-profile", daemon_base_url(port));
    let client = http_client();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// User profile: GET /api/user-profile (first_name, last_name, how_to_call, onboarding_completed).
#[tauri::command]
async fn get_user_profile(port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/user-profile", daemon_base_url(port));
    let client = http_client();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// User profile: POST /api/user-profile (first_name?, last_name?, how_to_call?, onboarding_completed?, proactive_check_in_enabled?, proactive_check_in_interval_days?).
#[tauri::command]
async fn post_user_profile(body: serde_json::Value, port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/user-profile", daemon_base_url(port));
    let client = http_client();
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// First message (onboarding, daily greeting, proactive): GET /api/first-message?context=...
#[tauri::command]
async fn get_first_message(context: String, port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let context = context.trim();
    let url = if context.is_empty() {
        format!("{}/api/first-message", daemon_base_url(port))
    } else {
        format!(
            "{}/api/first-message?context={}",
            daemon_base_url(port),
            urlencoding::encode(context)
        )
    };
    let client = http_client();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// Parse a key name string to enigo Key (e.g. "Control" -> Key::Control, "a" -> Key::Unicode('a')).
fn parse_key(s: &str) -> Option<Key> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let lower = s.to_lowercase();
    match lower.as_str() {
        "control" | "ctrl" => Some(Key::Control),
        "shift" => Some(Key::Shift),
        "alt" => Some(Key::Alt),
        "meta" | "command" | "cmd" => Some(Key::Meta),
        "super" | "windows" | "win" => Some(Key::Meta),
        "space" => Some(Key::Space),
        "return" | "enter" => Some(Key::Return),
        "tab" => Some(Key::Tab),
        "escape" | "esc" => Some(Key::Escape),
        "backspace" => Some(Key::Backspace),
        "delete" => Some(Key::Delete),
        "home" => Some(Key::Home),
        "end" => Some(Key::End),
        "pageup" | "page_up" => Some(Key::PageUp),
        "pagedown" | "page_down" => Some(Key::PageDown),
        "left" | "leftarrow" => Some(Key::LeftArrow),
        "right" | "rightarrow" => Some(Key::RightArrow),
        "up" | "uparrow" => Some(Key::UpArrow),
        "down" | "downarrow" => Some(Key::DownArrow),
        // enigo doesn't expose PrintScr on macOS; don't reference it there.
        #[cfg(not(target_os = "macos"))]
        "printscreen" | "print_scr" => Some(Key::PrintScr),
        #[cfg(target_os = "macos")]
        "printscreen" | "print_scr" => None,
        "f1" => Some(Key::F1),
        "f2" => Some(Key::F2),
        "f3" => Some(Key::F3),
        "f4" => Some(Key::F4),
        "f5" => Some(Key::F5),
        "f6" => Some(Key::F6),
        "f7" => Some(Key::F7),
        "f8" => Some(Key::F8),
        "f9" => Some(Key::F9),
        "f10" => Some(Key::F10),
        "f11" => Some(Key::F11),
        "f12" => Some(Key::F12),
        // Only map to Unicode when the input is exactly one Unicode scalar.
        _ if s.chars().count() == 1 => Some(Key::Unicode(s.chars().next().unwrap())),
        _ => None,
    }
}

/// Execute a synthetic input action (keyboard/mouse) via enigo. Used when UI fulfills device_invoke synthetic_input.
#[tauri::command]
fn execute_synthetic_input(action: String, params: serde_json::Value) -> Result<bool, String> {
    let mut enigo = Enigo::new(&Settings::default()).map_err(|e| e.to_string())?;
    let action = action.to_lowercase();
    let params = params.as_object().ok_or("params must be an object")?;

    match action.as_str() {
        "shortcut" => {
            let keys = params
                .get("keys")
                .and_then(|v| v.as_array())
                .ok_or("shortcut requires params.keys array")?;
            let keys: Vec<Key> = keys
                .iter()
                .filter_map(|v| v.as_str().and_then(parse_key))
                .collect();
            if keys.is_empty() {
                return Err("shortcut: no valid keys".to_string());
            }
            // Modifiers first (press), then main key (click), then modifiers (release)
            let (modifiers, main): (Vec<&Key>, Vec<&Key>) = keys.iter().partition(|k| {
                matches!(k, Key::Control | Key::Shift | Key::Alt | Key::Meta)
            });
            for k in &modifiers {
                enigo.key(**k, Direction::Press).map_err(|e| e.to_string())?;
            }
            for k in &main {
                enigo.key(**k, Direction::Click).map_err(|e| e.to_string())?;
            }
            for k in modifiers.iter().rev() {
                enigo.key(**k, Direction::Release).map_err(|e| e.to_string())?;
            }
            Ok(true)
        }
        "key" => {
            let key_str = params.get("key").and_then(|v| v.as_str()).ok_or("key requires params.key")?;
            let key = parse_key(key_str).ok_or_else(|| format!("unknown key: {}", key_str))?;
            enigo.key(key, Direction::Click).map_err(|e| e.to_string())?;
            Ok(true)
        }
        "type" => {
            let text = params.get("text").and_then(|v| v.as_str()).ok_or("type requires params.text")?;
            enigo.text(text).map_err(|e| e.to_string())?;
            Ok(true)
        }
        "mouse_move" => {
            let x = params.get("x").and_then(|v| v.as_i64()).ok_or("mouse_move requires params.x")? as i32;
            let y = params.get("y").and_then(|v| v.as_i64()).ok_or("mouse_move requires params.y")? as i32;
            enigo.move_mouse(x, y, Coordinate::Abs).map_err(|e| e.to_string())?;
            Ok(true)
        }
        "mouse_click" | "mouse_double_click" => {
            let button = params
                .get("button")
                .and_then(|v| v.as_str())
                .map(|s| match s.to_lowercase().as_str() {
                    "right" => Button::Right,
                    "middle" => Button::Middle,
                    _ => Button::Left,
                })
                .unwrap_or(Button::Left);
            if let (Some(x), Some(y)) = (
                params.get("x").and_then(|v| v.as_i64()),
                params.get("y").and_then(|v| v.as_i64()),
            ) {
                enigo.move_mouse(x as i32, y as i32, Coordinate::Abs).map_err(|e| e.to_string())?;
            }
            enigo.button(button, Direction::Click).map_err(|e| e.to_string())?;
            if action == "mouse_double_click" {
                enigo.button(button, Direction::Click).map_err(|e| e.to_string())?;
            }
            Ok(true)
        }
        "mouse_scroll" => {
            let delta_x = params.get("delta_x").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let delta_y = params.get("delta_y").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let clicks = params.get("clicks").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            if delta_x != 0 {
                enigo.scroll(delta_x, Axis::Horizontal).map_err(|e| e.to_string())?;
            }
            if delta_y != 0 {
                enigo.scroll(delta_y, Axis::Vertical).map_err(|e| e.to_string())?;
            }
            if clicks != 0 && delta_x == 0 && delta_y == 0 {
                enigo.scroll(clicks, Axis::Vertical).map_err(|e| e.to_string())?;
            }
            Ok(true)
        }
        "mouse_drag" => {
            let from_x = params.get("from_x").and_then(|v| v.as_i64()).ok_or("mouse_drag requires from_x")? as i32;
            let from_y = params.get("from_y").and_then(|v| v.as_i64()).ok_or("mouse_drag requires from_y")? as i32;
            let to_x = params.get("to_x").and_then(|v| v.as_i64()).ok_or("mouse_drag requires to_x")? as i32;
            let to_y = params.get("to_y").and_then(|v| v.as_i64()).ok_or("mouse_drag requires to_y")? as i32;
            let button = params
                .get("button")
                .and_then(|v| v.as_str())
                .map(|s| match s.to_lowercase().as_str() {
                    "right" => Button::Right,
                    "middle" => Button::Middle,
                    _ => Button::Left,
                })
                .unwrap_or(Button::Left);
            enigo.move_mouse(from_x, from_y, Coordinate::Abs).map_err(|e| e.to_string())?;
            enigo.button(button, Direction::Press).map_err(|e| e.to_string())?;
            enigo.move_mouse(to_x, to_y, Coordinate::Abs).map_err(|e| e.to_string())?;
            enigo.button(button, Direction::Release).map_err(|e| e.to_string())?;
            Ok(true)
        }
        _ => Err(format!("unknown action: {}", action)),
    }
}

/// Agent profile: POST /api/agent-profile (merge body: name?, personality?, rules?, can_do?, cannot_do?).
#[tauri::command]
async fn post_agent_profile(body: serde_json::Value, port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/agent-profile", daemon_base_url(port));
    let client = http_client();
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .invoke_handler(tauri::generate_handler![
            check_health,
            get_router_metrics,
            send_message,
            send_message_ack,
            get_task_status,
            get_tasks,
            get_task_events,
            cancel_task,
            get_pending_human_input,
            get_task_human_input,
            post_task_human_reply,
            get_schedules,
            get_schedule_by_id,
            create_schedule,
            put_schedule,
            delete_schedule,
            get_calendar_events,
            get_task_runs,
            get_memory_short_term,
            get_memory_long_term,
            get_memory_search,
            delete_memory_long_term,
            rebuild_memory_relations,
            get_schedule_run_reports,
            get_user_rag_documents,
            add_user_rag_document,
            delete_user_rag_document,
            get_device_pending,
            post_device_result,
            get_agent_profile,
            post_agent_profile,
            get_user_profile,
            post_user_profile,
            get_first_message,
            execute_synthetic_input,
            get_docs,
            get_config,
            set_config,
            get_vault_keys,
            get_ollama_models,
            get_router_models,
            restart_daemon,
            get_doctor,
            get_update_status,
            get_app_version,
            open_url,
            open_path,
            open_path_in_explorer,
            read_file_as_data_url,
            get_advice,
            get_plugins,
            reload_plugins,
            get_skills,
            reload_skills,
            install_skill,
            uninstall_skill,
            get_router_routes,
            set_router_route,
            get_voice_status,
            voice_stt_transcribe,
            voice_tts,
            get_embedded_status,
            embedded_reload,
            get_device_pending,
            post_device_result,
            execute_synthetic_input,
            get_agent_profile,
            post_agent_profile
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
