//! Telegram adapter: long polling, receives messages, creates tasks via daemon API, polls until done, replies in chat.
//! Same task_id visible from UI/Slack/Discord/Telegram (GET /api/tasks/:id).

use std::time::Duration;
use tracing::{info, warn};

const TELEGRAM_API_BASE: &str = "https://api.telegram.org";
const TELEGRAM_POLL_INTERVAL_MS: u64 = 1500;
const TELEGRAM_MAX_POLL_SECS: u64 = 600;
const TELEGRAM_GETUPDATES_TIMEOUT: u64 = 25;
const TELEGRAM_GETUPDATES_RETRIES: u32 = 4;
const TELEGRAM_RETRY_DELAYS_SECS: [u64; 4] = [2, 5, 10, 20];

/// Run Telegram bot loop: getUpdates (long poll) -> for each message with /akasha <text>, POST to daemon -> poll task -> sendMessage reply.
/// If `notify_chat_id` is Some, sends "Akasha Telegram bot is connected." to that chat at startup.
pub async fn run_telegram_bot(
    token: String,
    daemon_base_url: String,
    notify_chat_id: Option<i64>,
) -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(60))
        .user_agent("AkashaDaemon/1.0 TelegramBot")
        .build()?;
    let mut offset: i64 = 0;
    let get_updates_url = format!("{}/bot{}/getUpdates", TELEGRAM_API_BASE, token);
    let send_message_url = format!("{}/bot{}/sendMessage", TELEGRAM_API_BASE, token);
    let delete_webhook_url = format!("{}/bot{}/deleteWebhook", TELEGRAM_API_BASE, token);

    // If a webhook was ever set, getUpdates returns no updates. Remove it so we use long polling.
    if let Err(e) = client.get(&delete_webhook_url).send().await {
        warn!(error = %e, "Telegram deleteWebhook failed (continuing with getUpdates)");
    } else {
        info!("Telegram: webhook cleared, using long polling for updates");
    }

    if let Some(chat_id) = notify_chat_id {
        if let Err(e) = send_telegram(&client, &send_message_url, chat_id, "Akasha Telegram bot is connected.").await {
            warn!(error = %e, chat_id = %chat_id, "Failed to send Telegram startup notification");
        } else {
            info!(chat_id = %chat_id, "Sent Telegram startup notification (bot connected)");
        }
    }

    loop {
        let url = format!(
            "{}?offset={}&timeout={}",
            get_updates_url, offset, TELEGRAM_GETUPDATES_TIMEOUT
        );
        let mut resp = None;
        for attempt in 0..TELEGRAM_GETUPDATES_RETRIES {
            match client.get(&url).send().await {
                Ok(r) => {
                    resp = Some(r);
                    break;
                }
                Err(e) => {
                    let delay = TELEGRAM_RETRY_DELAYS_SECS.get(attempt as usize).copied().unwrap_or(20);
                    warn!(
                        error = %e,
                        attempt = attempt + 1,
                        next_retry_secs = delay,
                        "Telegram getUpdates failed (connection reset or network issue)"
                    );
                    tokio::time::sleep(Duration::from_secs(delay)).await;
                }
            }
        }
        let resp = match resp {
            Some(r) => r,
            None => {
                tokio::time::sleep(Duration::from_secs(20)).await;
                continue;
            }
        };
        if !resp.status().is_success() {
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }
        let json: serde_json::Value = match resp.json().await {
            Ok(j) => j,
            Err(_) => continue,
        };
        let results = match json.get("result").and_then(|r| r.as_array()) {
            Some(a) => a,
            None => continue,
        };
        if !results.is_empty() {
            info!(count = results.len(), "Telegram: received update(s)");
        }
        for update in results {
            let update_id = update.get("update_id").and_then(|v| v.as_i64()).unwrap_or(0);
            offset = update_id + 1;
            let message = match update.get("message") {
                Some(m) => m,
                None => {
                    tracing::debug!(update_id = %update_id, "Telegram: skipping update without message (e.g. callback_query)");
                    continue;
                }
            };
            let chat = match message.get("chat") {
                Some(c) => c,
                None => continue,
            };
            let chat_id = match chat.get("id").and_then(|v| v.as_i64()) {
                Some(id) => id,
                None => continue,
            };
            let text = match message.get("text").and_then(|v| v.as_str()) {
                Some(t) => t.trim(),
                None => {
                    tracing::debug!(update_id = %update_id, "Telegram: skipping message without text (e.g. photo)");
                    continue;
                }
            };
            // Support /akasha <message> or /start (reply with help); other messages get a hint
            let (command, rest) = text.split_once(' ').unwrap_or((text, ""));
            let text = if command == "/akasha" {
                rest.trim()
            } else if command == "/start" {
                "Usage: /akasha <your message> to talk to Akasha."
            } else {
                let _ = send_telegram(
                    &client,
                    &send_message_url,
                    chat_id,
                    "Use /akasha <your message> to talk to Akasha.",
                )
                .await;
                continue;
            };
            if text.is_empty() && command != "/start" {
                let _ = send_telegram(&client, &send_message_url, chat_id, "Usage: /akasha <your message>").await;
                continue;
            }
            if command == "/start" {
                let _ = send_telegram(&client, &send_message_url, chat_id, text).await;
                continue;
            }
            if let Err(e) = akasha_core::check_prompt_injection(text) {
                let _ = send_telegram(&client, &send_message_url, chat_id, &format!("Rejected: {}", e)).await;
                continue;
            }

            info!(chat_id = %chat_id, "Telegram: forwarding message to daemon");
            let url_post = format!("{}/api/message", daemon_base_url);
            let body = serde_json::json!({ "message": text });
            let resp = match client.post(&url_post).json(&body).send().await {
                Ok(r) => r,
                Err(e) => {
                    warn!(error = %e, "Telegram: failed to POST to daemon");
                    let _ = send_telegram(&client, &send_message_url, chat_id, "Failed to reach Akasha daemon.").await;
                    continue;
                }
            };
            if !resp.status().is_success() {
                let _ = send_telegram(&client, &send_message_url, chat_id, "Daemon returned an error.").await;
                continue;
            }
            let json: serde_json::Value = match resp.json().await {
                Ok(j) => j,
                Err(_) => {
                    let _ = send_telegram(&client, &send_message_url, chat_id, "Invalid daemon response.").await;
                    continue;
                }
            };
            let task_id = match json.get("task_id").and_then(|v| v.as_str()) {
                Some(id) => id.to_string(),
                None => {
                    let _ = send_telegram(&client, &send_message_url, chat_id, "No task_id in response.").await;
                    continue;
                }
            };
            info!(task_id = %task_id, "Telegram task created, polling until done");

            let task_url = format!("{}/api/tasks/{}", daemon_base_url, task_id);
            let deadline = std::time::Instant::now() + Duration::from_secs(TELEGRAM_MAX_POLL_SECS);
            let reply = loop {
                if std::time::Instant::now() > deadline {
                    break "Task timed out.".to_string();
                }
                tokio::time::sleep(Duration::from_millis(TELEGRAM_POLL_INTERVAL_MS)).await;
                let poll = match client.get(&task_url).send().await {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                if !poll.status().is_success() {
                    continue;
                }
                let json: serde_json::Value = match poll.json().await {
                    Ok(j) => j,
                    Err(_) => continue,
                };
                let task_status = json.get("status").and_then(|v| v.as_str()).unwrap_or("");
                if task_status == "completed" || task_status == "failed" {
                    let progress_msg = json
                        .get("progress")
                        .and_then(|p| p.as_array())
                        .and_then(|a| a.last())
                        .and_then(|e| e.get("message").and_then(|m| m.as_str()))
                        .unwrap_or("");
                    let reply_text = if progress_msg.is_empty() {
                        if task_status == "completed" {
                            "Task completed.".to_string()
                        } else {
                            "Task failed.".to_string()
                        }
                    } else {
                        progress_msg.to_string()
                    };
                    info!(task_id = %task_id, status = %task_status, "Telegram: task finished, sending reply to chat");
                    break reply_text;
                }
            };
            if let Err(e) = send_telegram(&client, &send_message_url, chat_id, &reply).await {
                warn!(error = %e, chat_id = %chat_id, "Telegram: failed to send reply");
            } else {
                info!(chat_id = %chat_id, "Telegram: reply sent");
            }
        }
    }
}

async fn send_telegram(
    client: &reqwest::Client,
    url: &str,
    chat_id: i64,
    text: &str,
) -> Result<(), reqwest::Error> {
    let body = serde_json::json!({ "chat_id": chat_id, "text": text });
    client.post(url).json(&body).send().await?;
    Ok(())
}
