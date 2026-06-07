//! Telegram adapter: long polling, receives messages, creates tasks via daemon API, polls until done, replies in chat.
//! Commands: `/akasha` (and `/akasha@BotName` in groups), plain text in private DMs, `/start` for help.
//! Same task_id visible from UI/Slack/Discord/Telegram (GET /api/tasks/:id).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tracing::{info, warn};

fn telegram_hitl_pending() -> &'static Mutex<HashMap<i64, String>> {
    static P: OnceLock<Mutex<HashMap<i64, String>>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(HashMap::new()))
}

const TELEGRAM_API_BASE: &str = "https://api.telegram.org";
/// Shown when Telegram returns 401 (invalid/revoked token, typo, or trailing space in vault).
const TELEGRAM_401_HELP: &str = "Telegram HTTP 401 = jeton de bot invalide ou révoqué. Va sur @BotFather → /mybots → ton bot → API Token → /revoke puis copie le NOUVEAU token (format 123456789:ABC-DEF... sans espace). Puis: akasha vault set telegram_bot_token <token> et redémarre le démon. Si le token a fuité dans des logs, révoque-le tout de suite.";
const TELEGRAM_POLL_INTERVAL_MS: u64 = 1500;
const TELEGRAM_MAX_POLL_SECS: u64 = 600;
const TELEGRAM_GETUPDATES_TIMEOUT: u64 = 25;
const TELEGRAM_GETUPDATES_RETRIES: u32 = 4;
const TELEGRAM_RETRY_DELAYS_SECS: [u64; 4] = [2, 5, 10, 20];


fn is_telegram_unauthorized(err: &str, status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::UNAUTHORIZED || err.contains("401") || err.to_lowercase().contains("unauthorized")
}

fn telegram_error_description(json: &serde_json::Value) -> String {
    json.get("description")
        .and_then(|d| d.as_str())
        .unwrap_or("unknown Telegram error")
        .to_string()
}

/// Telegram returns HTTP 200 with `ok: false` on failure — must check JSON, not only status.
async fn telegram_response_result(resp: reqwest::Response) -> Result<serde_json::Value, String> {
    let status = resp.status();
    let body = resp.text().await.map_err(|e| e.to_string())?;
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("Telegram JSON: {e}"))?;
    if json.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        let desc = telegram_error_description(&json);
        return Err(format!("{desc} (HTTP {status})"));
    }
    Ok(json.get("result").cloned().unwrap_or(serde_json::Value::Null))
}

async fn send_telegram(
    client: &reqwest::Client,
    url: &str,
    chat_id: i64,
    text: &str,
) -> Result<(), String> {
    let body = serde_json::json!({ "chat_id": chat_id, "text": text });
    let resp = client
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    telegram_response_result(resp).await?;
    Ok(())
}

/// Run Telegram bot loop: getUpdates (long poll) -> messages (private text, /akasha, /akasha@Bot in groups) POST to daemon -> poll task -> sendMessage reply.
/// If `notify_chat_id` is Some, sends "Akasha Telegram bot is connected." to that chat at startup.
pub async fn run_telegram_bot(
    token: String,
    daemon_base_url: String,
    notify_chat_id: Option<i64>,
    data_dir: std::path::PathBuf,
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
    let get_me_url = format!("{}/bot{}/getMe", TELEGRAM_API_BASE, token);

    match client.get(&get_me_url).send().await {
        Ok(r) => {
            let status = r.status();
            match telegram_response_result(r).await {
                Ok(me) => {
                    let username = me.get("username").and_then(|u| u.as_str()).unwrap_or("?");
                    let id = me.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
                    info!(bot_username = %username, bot_id = id, "Telegram: token OK — open Telegram and message this bot (see bot_username)");
                }
                Err(e) => {
                    if is_telegram_unauthorized(&e, status) {
                        warn!(error = %e, "{}", TELEGRAM_401_HELP);
                        return Err(anyhow::anyhow!("Telegram bot token rejected (HTTP 401)"));
                    }
                    warn!(error = %e, "Telegram getMe failed");
                }
            }
        }
        Err(_) => {
            warn!("Telegram getMe: erreur réseau/TLS vers api.telegram.org (URL non journalisée — pare-feu/VPN). Si deleteWebhook affiche 401, le token est invalide.");
        }
    }

    match client.get(&delete_webhook_url).send().await {
        Ok(r) => {
            let status = r.status();
            match telegram_response_result(r).await {
                Ok(_) => info!("Telegram: webhook cleared, long polling enabled"),
                Err(e) => {
                    if is_telegram_unauthorized(&e, status) {
                        warn!(error = %e, "{}", TELEGRAM_401_HELP);
                        return Err(anyhow::anyhow!("Telegram bot token rejected (HTTP 401)"));
                    }
                    warn!(error = %e, "Telegram deleteWebhook failed — getUpdates may stay empty if a webhook is still set elsewhere");
                }
            }
        }
        Err(e) => warn!(error = %e, "Telegram deleteWebhook network error"),
    }

    if let Some(chat_id) = notify_chat_id {
        match send_telegram(
            &client,
            &send_message_url,
            chat_id,
            "Akasha Telegram bot is connected.",
        )
        .await
        {
            Ok(()) => info!(chat_id = %chat_id, "Telegram startup notification delivered"),
            Err(e) => {
                if e.contains("401") || e.to_lowercase().contains("unauthorized") {
                    warn!(error = %e, "{}", TELEGRAM_401_HELP);
                    return Err(anyhow::anyhow!("Telegram bot token rejected (HTTP 401)"));
                }
                warn!(
                    error = %e,
                    chat_id = %chat_id,
                    "Telegram startup notification failed — vérifie AKASHA_TELEGRAM_NOTIFY_CHAT_ID (ex. @userinfobot) après avoir écrit au bot"
                );
            }
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
                    let delay = TELEGRAM_RETRY_DELAYS_SECS
                        .get(attempt as usize)
                        .copied()
                        .unwrap_or(20);
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
        let status = resp.status();
        if !status.is_success() {
            if status == reqwest::StatusCode::UNAUTHORIZED {
                warn!(status = %status, "{}", TELEGRAM_401_HELP);
                return Err(anyhow::anyhow!("Telegram bot token rejected (HTTP 401)"));
            }
            warn!(status = %status, "Telegram getUpdates HTTP error");
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }
        let body = match resp.text().await {
            Ok(b) => b,
            Err(e) => {
                warn!(error = %e, "Telegram getUpdates body read failed");
                continue;
            }
        };
        let json: serde_json::Value = match serde_json::from_str(&body) {
            Ok(j) => j,
            Err(e) => {
                warn!(error = %e, "Telegram getUpdates invalid JSON");
                continue;
            }
        };
        if json.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            warn!(
                error = %telegram_error_description(&json),
                "Telegram getUpdates rejected — if 'terminated by other getUpdates', stop duplicate daemons or other bots using this token"
            );
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }
        let results = match json.get("result").and_then(|r| r.as_array()) {
            Some(a) => a,
            None => {
                warn!("Telegram getUpdates: missing result array");
                continue;
            }
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
            let from = message.get("from");
            let from_user_id = from
                .and_then(|f| f.get("id"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let from_username = from
                .and_then(|f| f.get("username"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let text = match message.get("text").and_then(|v| v.as_str()) {
                Some(t) => t.trim(),
                None => {
                    tracing::debug!(update_id = %update_id, "Telegram: skipping message without text (e.g. photo)");
                    continue;
                }
            };
            let chat_type = chat.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let (command_raw, rest) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
            let command = command_raw.split('@').next().unwrap_or(command_raw);
            let rest = rest.trim();
            let access = crate::channel_access::load(data_dir.as_path());

            let payload: String = if command == "/akasha" {
                rest.to_string()
            } else if command == "/start" {
                if from_user_id == 0 {
                    "Unable to identify Telegram user.".to_string()
                } else if crate::channel_access::is_approved_user(&access, from_user_id) {
                    "You are already approved. Use /akasha <message>.".to_string()
                } else {
                    let req_url = format!("{}/api/channel-access/telegram/request", daemon_base_url);
                    let req_body = serde_json::json!({
                        "user_id": from_user_id,
                        "username": from_username
                    });
                    match client.post(&req_url).json(&req_body).send().await {
                        Ok(resp) if resp.status().is_success() => {
                            let v = resp.json::<serde_json::Value>().await.unwrap_or_default();
                            if v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
                                if v.get("already_approved").and_then(|b| b.as_bool()).unwrap_or(false) {
                                    "You are already approved. Use /akasha <message>.".to_string()
                                } else {
                                    let code = v
                                        .get("pairing_code")
                                        .and_then(|s| s.as_str())
                                        .unwrap_or("pending");
                                    format!("Pairing request created. Share this code with an Akasha admin: {}", code)
                                }
                            } else {
                                "Pairing request failed: unexpected response from daemon. Try again later.".to_string()
                            }
                        }
                        Ok(resp) => {
                            let status = resp.status();
                            format!("Pairing request failed (daemon returned {}). Try again later.", status.as_u16())
                        }
                        Err(_) => "Pairing request failed. Try again later.".to_string(),
                    }
                }
            } else if command == "/status" {
                let url = format!("{}/api/status", daemon_base_url);
                match client.get(&url).send().await {
                    Ok(resp) => resp.text().await.unwrap_or_else(|_| "status unavailable".to_string()),
                    Err(_) => "status unavailable".to_string(),
                }
            } else if command == "/budget" {
                let url = format!("{}/api/budget", daemon_base_url);
                match client.get(&url).send().await {
                    Ok(resp) => resp.text().await.unwrap_or_else(|_| "budget unavailable".to_string()),
                    Err(_) => "budget unavailable".to_string(),
                }
            } else if command == "/permissions" {
                if from_user_id == 0 || !crate::channel_access::is_admin(&access, from_user_id) {
                    let msg = "Permission denied: /permissions requires an approved admin.";
                    let _ = send_telegram(&client, &send_message_url, chat_id, msg).await;
                    continue;
                }
                let mode = if rest.eq_ignore_ascii_case("allow_all") {
                    "allow_all"
                } else if rest.eq_ignore_ascii_case("ask_me") {
                    "ask_me"
                } else {
                    ""
                };
                if mode.is_empty() {
                    let url = format!("{}/api/permissions/mode", daemon_base_url);
                    match client.get(&url).send().await {
                        Ok(resp) => resp.text().await.unwrap_or_else(|_| "permissions mode unavailable".to_string()),
                        Err(_) => "permissions mode unavailable".to_string(),
                    }
                } else {
                    let url = format!("{}/api/permissions/mode", daemon_base_url);
                    let body = serde_json::json!({ "mode": mode });
                    match client.post(&url).json(&body).send().await {
                        Ok(resp) if resp.status().is_success() => format!("permissions mode set to {}", mode),
                        Ok(resp) => format!("failed to set permissions mode (daemon returned {})", resp.status().as_u16()),
                        Err(_) => "failed to set permissions mode".to_string(),
                    }
                }
            } else if command == "/memory" {
                let url = format!("{}/api/memory/second-brain/overview", daemon_base_url);
                match client.get(&url).send().await {
                    Ok(resp) => resp.text().await.unwrap_or_else(|_| "memory unavailable".to_string()),
                    Err(_) => "memory unavailable".to_string(),
                }
            } else if command == "/tasks" {
                let url = format!("{}/api/schedules", daemon_base_url);
                match client.get(&url).send().await {
                    Ok(resp) => resp.text().await.unwrap_or_else(|_| "tasks unavailable".to_string()),
                    Err(_) => "tasks unavailable".to_string(),
                }
            } else if chat_type == "private" && !text.starts_with('/') {
                text.to_string()
            } else {
                let hint = "Use /akasha <your message>. In groups: /akasha@BotUsername <message>.";
                if let Err(e) = send_telegram(&client, &send_message_url, chat_id, hint).await {
                    warn!(error = %e, chat_id = %chat_id, "Telegram: failed to send hint");
                }
                continue;
            };
            if payload.is_empty() && command != "/start" {
                if let Err(e) =
                    send_telegram(&client, &send_message_url, chat_id, "Usage: /akasha <your message>").await
                {
                    warn!(error = %e, "Telegram sendMessage failed");
                }
                continue;
            }
            if command == "/start" {
                if let Err(e) = send_telegram(&client, &send_message_url, chat_id, &payload).await {
                    warn!(error = %e, "Telegram /start reply failed");
                }
                continue;
            }
            if command == "/status"
                || command == "/budget"
                || command == "/permissions"
                || command == "/memory"
                || command == "/tasks"
            {
                if let Err(e) = send_telegram(&client, &send_message_url, chat_id, &payload).await {
                    warn!(error = %e, "Telegram command reply failed");
                }
                continue;
            }
            if from_user_id == 0 || !crate::channel_access::is_approved_user(&access, from_user_id) {
                let _ = send_telegram(
                    &client,
                    &send_message_url,
                    chat_id,
                    "Access pending approval. Send /start to get your pairing code and ask an admin to approve it.",
                )
                .await;
                continue;
            }
            if let Err(e) = akasha_core::check_prompt_injection(&payload) {
                let _ = send_telegram(
                    &client,
                    &send_message_url,
                    chat_id,
                    &format!("Rejected: {}", e),
                )
                .await;
                continue;
            }

            info!(chat_id = %chat_id, "Telegram: forwarding message to daemon");
            let hitl_reply = if let Ok(mut pending) = telegram_hitl_pending().lock() {
                pending.remove(&chat_id)
            } else {
                None
            };
            if let Some(task_id) = hitl_reply {
                let reply_url = format!("{}/api/tasks/{}/human-reply", daemon_base_url, task_id);
                let body = serde_json::json!({ "response": payload });
                match client.post(&reply_url).json(&body).send().await {
                        Ok(r) if r.status().is_success() => {
                            let _ = send_telegram(
                                &client,
                                &send_message_url,
                                chat_id,
                                "Réponse enregistrée — l'agent reprend la tâche.",
                            )
                            .await;
                        }
                        _ => {
                            let _ = send_telegram(
                                &client,
                                &send_message_url,
                                chat_id,
                                "Impossible d'enregistrer la réponse human-in-the-loop.",
                            )
                            .await;
                        }
                    }
                continue;
            }
            let url_post = format!("{}/api/message", daemon_base_url);
            let body = serde_json::json!({ "message": payload });
            let resp = match client.post(&url_post).json(&body).send().await {
                Ok(r) => r,
                Err(e) => {
                    warn!(error = %e, "Telegram: failed to POST to daemon");
                    let _ = send_telegram(&client, &send_message_url, chat_id, "Failed to reach Akasha daemon.")
                        .await;
                    continue;
                }
            };
            if !resp.status().is_success() {
                let _ = send_telegram(&client, &send_message_url, chat_id, "Daemon returned an error.")
                    .await;
                continue;
            }
            let json: serde_json::Value = match resp.json().await {
                Ok(j) => j,
                Err(_) => {
                    let _ = send_telegram(&client, &send_message_url, chat_id, "Invalid daemon response.")
                        .await;
                    continue;
                }
            };
            let task_id = match json.get("task_id").and_then(|v| v.as_str()) {
                Some(id) => id.to_string(),
                None => {
                    let _ = send_telegram(&client, &send_message_url, chat_id, "No task_id in response.")
                        .await;
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
                if task_status == "waiting_user_input" {
                    let hi_url = format!("{}/api/tasks/{}/human-input", daemon_base_url, task_id);
                    if let Ok(hi_resp) = client.get(&hi_url).send().await {
                        if hi_resp.status().is_success() {
                            if let Ok(hi_json) = hi_resp.json::<serde_json::Value>().await {
                                let question = hi_json
                                    .get("question")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("L'agent attend votre réponse.");
                                let _ = send_telegram(
                                    &client,
                                    &send_message_url,
                                    chat_id,
                                    &format!("❓ {question}\n\nRépondez ici pour continuer."),
                                )
                                .await;
                                if let Ok(mut pending) = telegram_hitl_pending().lock() {
                                    pending.insert(chat_id, task_id.clone());
                                }
                                break "En attente de votre réponse (human-in-the-loop).".to_string();
                            }
                        }
                    }
                }
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
