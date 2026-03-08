#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

const DAEMON_PORT: u16 = 3876;
const TASK_POLL_INTERVAL_MS: u64 = 1500;
const TASK_POLL_TIMEOUT_SECS: u64 = 600;

fn daemon_base_url(port: u16) -> String {
    format!("http://127.0.0.1:{}", port)
}

/// Health check: GET / returns {"status":"ok"} when daemon is up.
#[tauri::command]
async fn check_health(port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/", daemon_base_url(port));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
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
    let message = json.get("message").and_then(|v| v.as_str()).unwrap_or("Je prends en compte votre demande.").to_string();
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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;
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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;
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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
    let body = serde_json::json!({ "category": category, "provider": provider, "model": model });
    let resp = client.post(&url).json(&body).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let err = resp.text().await.unwrap_or_default();
        return Err(err);
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// GET /api/router/embedded-status — embedded LLM status (for /embedded).
#[tauri::command]
async fn get_embedded_status(port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/router/embedded-status", daemon_base_url(port));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// POST /api/diagnostic/advice — get advice from health (for slash /advice).
#[tauri::command]
async fn get_advice(health: serde_json::Value, port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/diagnostic/advice", daemon_base_url(port));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .map_err(|e| e.to_string())?;
    let body = serde_json::json!({ "health": health });
    let resp = client.post(&url).json(&body).send().await.map_err(|e| e.to_string())?;
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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client.post(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    Ok(())
}

/// POST /api/skills/reload — reload skills from disk (Agent Skills + YAML). Returns { count }.
#[tauri::command]
async fn reload_skills(port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/skills/reload", daemon_base_url(port));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client.post(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// POST /api/restart — request daemon restart (for slash /restart).
#[tauri::command]
async fn restart_daemon(port: Option<u16>) -> Result<(), String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/restart", daemon_base_url(port));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;
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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client.post(&url).send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    let json: serde_json::Value = resp.json().await.unwrap_or(serde_json::json!({ "error": "invalid_response" }));
    if !status.is_success() {
        let detail = json.get("detail").and_then(|v| v.as_str()).unwrap_or(json.get("error").and_then(|v| v.as_str()).unwrap_or("Erreur inconnue"));
        return Err(detail.to_string());
    }
    Ok(json)
}

/// Human in the loop: GET /api/tasks/:id/human-input — pending question/context/choices for the task (404 if none).
#[tauri::command]
async fn get_task_human_input(task_id: String, port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/tasks/{}/human-input", daemon_base_url(port), task_id);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;
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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
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

/// Delete schedule: DELETE /api/schedules/:id.
#[tauri::command]
async fn delete_schedule(schedule_id: String, port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/schedules/{}", daemon_base_url(port), schedule_id);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

/// Memory long-term: GET /api/memory/long-term?limit=50
#[tauri::command]
async fn get_memory_long_term(limit: Option<u32>, port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let limit = limit.unwrap_or(50).min(200);
    let url = format!("{}/api/memory/long-term?limit={}", daemon_base_url(port), limit);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client.delete(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("{} {}", status, body));
    }
    Ok(())
}

/// Schedule run reports: GET /api/schedule_run_reports — completed schedule runs with message (for chat).
#[tauri::command]
async fn get_schedule_run_reports(port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/schedule_run_reports", daemon_base_url(port));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client.delete(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}", resp.status()));
    }
    Ok(())
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
            get_task_human_input,
            post_task_human_reply,
            get_schedules,
            get_schedule_by_id,
            create_schedule,
            delete_schedule,
            get_calendar_events,
            get_task_runs,
            get_memory_short_term,
            get_memory_long_term,
            delete_memory_long_term,
            get_schedule_run_reports,
            get_user_rag_documents,
            add_user_rag_document,
            delete_user_rag_document,
            get_docs,
            get_config,
            set_config,
            get_vault_keys,
            get_ollama_models,
            get_router_models,
            restart_daemon,
            get_doctor,
            get_advice,
            get_plugins,
            reload_plugins,
            reload_skills,
            get_router_routes,
            set_router_route,
            get_embedded_status,
            embedded_reload
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
