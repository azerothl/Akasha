//! Microsoft Teams adapter: Bot Framework webhook.
//! Receives POST with Activity JSON, creates task via main agent, polls until done, replies via Bot Framework API.

use crate::agents::MainAgent;
use std::path::Path;
use tracing::{info, warn};

const TEAMS_POLL_INTERVAL_MS: u64 = 1500;
const TEAMS_MAX_POLL_SECS: u64 = 600;
const MICROSOFT_LOGIN_URL: &str = "https://login.microsoftonline.com/botframework.com/oauth2/v2.0/token";

/// Minimal Bot Framework Activity (incoming).
#[derive(serde::Deserialize)]
#[allow(dead_code)]
struct TeamsActivity {
    #[serde(rename = "type")]
    type_: Option<String>,
    id: Option<String>,
    #[serde(rename = "serviceUrl")]
    service_url: Option<String>,
    #[serde(rename = "conversation")]
    conversation: Option<TeamsConversation>,
    from: Option<serde_json::Value>,
    text: Option<String>,
}

#[derive(serde::Deserialize)]
struct TeamsConversation {
    id: Option<String>,
}

/// Handle Teams Bot Framework message: parse activity, create task, poll, reply.
pub fn handle_teams_message(
    body: Option<Vec<u8>>,
    app_id: &str,
    app_password: &str,
    port: u16,
    main_agent: &MainAgent,
    store_path: &Path,
) -> String {
    let body = match body {
        Some(b) if !b.is_empty() => b,
        _ => {
            return crate::api::json_response("400 Bad Request", r#"{"error":"missing_body"}"#);
        }
    };
    let activity: TeamsActivity = match serde_json::from_slice(&body) {
        Ok(a) => a,
        Err(e) => {
            warn!(error = %e, "Teams: invalid JSON");
            return crate::api::json_response("400 Bad Request", r#"{"error":"invalid_json"}"#);
        }
    };
    if activity.type_.as_deref() != Some("message") {
        return crate::api::json_response("200 OK", "{}");
    }
    let text = activity.text.as_deref().unwrap_or("").trim().to_string();
    if text.is_empty() {
        return crate::api::json_response("200 OK", "{}");
    }
    let service_url = match activity.service_url.as_deref() {
        Some(u) if !u.is_empty() => u.to_string(),
        _ => {
            warn!("Teams: missing serviceUrl");
            return crate::api::json_response("400 Bad Request", r#"{"error":"missing_service_url"}"#);
        }
    };
    let conversation_id = match activity.conversation.as_ref().and_then(|c| c.id.as_deref()) {
        Some(id) => id.to_string(),
        None => {
            warn!("Teams: missing conversation.id");
            return crate::api::json_response("400 Bad Request", r#"{"error":"missing_conversation"}"#);
        }
    };
    if let Err(e) = akasha_core::check_prompt_injection(&text) {
        let body = serde_json::json!({ "error": "prompt_injection_rejected", "detail": e.to_string() });
        return crate::api::json_response("400 Bad Request", &body.to_string());
    }

    let main_agent = main_agent.clone();
    let store_path = store_path.to_path_buf();
    let app_id = app_id.to_string();
    let app_password = app_password.to_string();
    tokio::spawn(async move {
        let task_id = match main_agent.handle_message(
            &store_path,
            &text,
            uuid::Uuid::new_v4(),
            true,
            "teams",
            None,
            crate::agents::TaskPriority::UserNormal,
        ) {
            Ok(id) => id,
            Err(_) => {
                let _ = post_teams_reply(
                    &app_id,
                    &app_password,
                    &service_url,
                    &conversation_id,
                    "Failed to create task.",
                )
                .await;
                return;
            }
        };
        info!(task_id = %task_id, "Teams task created, polling until done");
        let base = format!("http://127.0.0.1:{}/api/tasks/{}", port, task_id);
        let client = reqwest::Client::new();
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_secs(TEAMS_MAX_POLL_SECS);
        loop {
            if std::time::Instant::now() > deadline {
                let _ = post_teams_reply(
                    &app_id,
                    &app_password,
                    &service_url,
                    &conversation_id,
                    "Task timed out.",
                )
                .await;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(TEAMS_POLL_INTERVAL_MS)).await;
            let resp = match client.get(&base).send().await {
                Ok(r) => r,
                Err(e) => {
                    warn!(error = %e, "Teams poll request failed");
                    continue;
                }
            };
            if !resp.status().is_success() {
                continue;
            }
            let json: serde_json::Value = match resp.json().await {
                Ok(j) => j,
                Err(_) => continue,
            };
            let task_status = json.get("status").and_then(|v| v.as_str()).unwrap_or("");
            if task_status == "completed" || task_status == "failed" {
                let progress = json
                    .get("progress")
                    .and_then(|p| p.as_array())
                    .and_then(|a| a.last())
                    .and_then(|e| e.get("message").and_then(|m| m.as_str()))
                    .unwrap_or("");
                let summary = if task_status == "completed" {
                    if progress.is_empty() {
                        "Task completed."
                    } else {
                        progress
                    }
                } else {
                    "Task failed."
                };
                let _ = post_teams_reply(
                    &app_id,
                    &app_password,
                    &service_url,
                    &conversation_id,
                    summary,
                )
                .await;
                break;
            }
        }
    });

    crate::api::json_response("200 OK", "{}")
}

async fn post_teams_reply(
    app_id: &str,
    app_password: &str,
    service_url: &str,
    conversation_id: &str,
    text: &str,
) -> Result<(), reqwest::Error> {
    let token = get_teams_token(app_id, app_password).await?;
    let url = format!(
        "{}/v3/conversations/{}/activities",
        service_url.trim_end_matches('/'),
        conversation_id
    );
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "type": "message",
        "text": text
    });
    client
        .post(&url)
        .header("Authorization", format!("Bearer {}", token))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?;
    Ok(())
}

async fn get_teams_token(app_id: &str, app_password: &str) -> Result<String, reqwest::Error> {
    let client = reqwest::Client::new();
    let params = [
        ("grant_type", "client_credentials"),
        ("client_id", app_id),
        ("client_secret", app_password),
        ("scope", "https://api.botframework.com/.default"),
    ];
    let resp = client
        .post(MICROSOFT_LOGIN_URL)
        .form(&params)
        .send()
        .await?;
    let json: serde_json::Value = resp.json().await?;
    let token = json
        .get("access_token")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    Ok(token.to_string())
}
