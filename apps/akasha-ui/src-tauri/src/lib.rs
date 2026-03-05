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
#[tauri::command]
async fn get_router_metrics(port: Option<u16>) -> Result<serde_json::Value, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let url = format!("{}/api/router/metrics", daemon_base_url(port));
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

#[tauri::command]
async fn send_message_ack(message: String, session_id: Option<String>, port: Option<u16>) -> Result<SendMessageAckResult, String> {
    let port = port.unwrap_or(DAEMON_PORT);
    let base = daemon_base_url(port);
    let url = format!("{}/api/message", base);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
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
    let mut session_id = session_id;
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
            get_schedules,
            get_task_runs,
            get_schedule_run_reports,
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
            reload_plugins
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
