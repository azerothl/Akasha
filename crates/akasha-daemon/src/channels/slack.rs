//! Slack adapter: slash command endpoint.
//! Receives POST from Slack, builds MessageEnvelope, delegates to gateway, polls until done, posts result to response_url.

use crate::agents::MainAgent;
use crate::gateway;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::path::Path;
use tracing::{info, warn};

const SLACK_POLL_INTERVAL_MS: u64 = 1500;
const SLACK_MAX_POLL_SECS: u64 = 600;

/// Verify Slack signing signature (X-Slack-Signature: v0=hex).
pub fn verify_signature(body: &[u8], signature_header: Option<&str>, signing_secret: &str) -> bool {
    let Some(header) = signature_header else { return false };
    let sig = header.trim().strip_prefix("v0=").unwrap_or(header.trim());
    if sig.len() != 64 {
        return false;
    }
    let mut mac =
        Hmac::<Sha256>::new_from_slice(signing_secret.as_bytes()).expect("HMAC key length");
    mac.update(body);
    let result = mac.finalize();
    let hex = hex::encode(result.into_bytes());
    constant_time_eq(hex.as_bytes(), sig.as_bytes())
}

#[inline]
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Parse application/x-www-form-urlencoded body for Slack slash command.
/// Returns (response_url, text).
pub fn parse_slash_form(body: &[u8]) -> Option<(String, String)> {
    let s = std::str::from_utf8(body).ok()?;
    let mut response_url = String::new();
    let mut text = String::new();
    for part in s.split('&') {
        let (k, v) = part.split_once('=')?;
        let v = urlencoding::decode(v).ok()?.into_owned();
        match k {
            "response_url" => response_url = v,
            "text" => text = v,
            _ => {}
        }
    }
    if response_url.is_empty() {
        return None;
    }
    Some((response_url, text))
}

/// Handle Slack slash command: verify, parse, return 200 immediately, then process in background.
pub fn handle_slack_command(
    body: Option<Vec<u8>>,
    signature_header: Option<&str>,
    signing_secret: &str,
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
    if !verify_signature(&body, signature_header, signing_secret) {
        return crate::api::json_response("401 Unauthorized", r#"{"error":"invalid_signature"}"#);
    }
    let (response_url, text) = match parse_slash_form(&body) {
        Some(p) => p,
        None => {
            return crate::api::json_response("400 Bad Request", r#"{"error":"invalid_form"}"#);
        }
    };
    if let Err(e) = akasha_core::check_prompt_injection(&text) {
        let body = serde_json::json!({ "error": "prompt_injection_rejected", "detail": e.to_string() });
        return crate::api::json_response("400 Bad Request", &body.to_string());
    }

    // Respond immediately so Slack gets 200 within 3s
    let immediate = crate::api::json_response(
        "200 OK",
        r#"{"response_type":"ephemeral","text":"Processing your request..."}"#,
    );

    let main_agent = main_agent.clone();
    let store_path = store_path.to_path_buf();
    let envelope = gateway::MessageEnvelope::slack("slack".to_string(), text.clone());
    tokio::spawn(async move {
        let task_id = match gateway::handle_envelope(&main_agent, &store_path, envelope).await {
            Ok(id) => id,
            Err(_) => {
                let _ = post_slack_response(&response_url, "Failed to create task.").await;
                return;
            }
        };
        info!(task_id = %task_id, "Slack task created, polling until done");
        let base = format!("http://127.0.0.1:{}/api/tasks/{}", port, task_id);
        let client = reqwest::Client::new();
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_secs(SLACK_MAX_POLL_SECS);
        loop {
            if std::time::Instant::now() > deadline {
                let _ = post_slack_response(&response_url, "Task timed out.").await;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(SLACK_POLL_INTERVAL_MS)).await;
            let resp = match client.get(&base).send().await {
                Ok(r) => r,
                Err(e) => {
                    warn!(error = %e, "Slack poll request failed");
                    continue;
                }
            };
            let status = resp.status();
            if !status.is_success() {
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
                let _ = post_slack_response(&response_url, summary).await;
                break;
            }
        }
    });

    immediate
}

async fn post_slack_response(response_url: &str, text: &str) -> Result<(), reqwest::Error> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({ "text": text });
    client
        .post(response_url)
        .json(&body)
        .send()
        .await?;
    Ok(())
}
