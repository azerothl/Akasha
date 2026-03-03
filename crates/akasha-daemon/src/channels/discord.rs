//! Discord adapter: bot that receives messages, creates tasks via daemon API, polls until done, replies in channel.
//! Same task_id is visible from UI/Slack/Discord (GET /api/tasks/:id).

use serenity::all::{Context, Message};
use serenity::client::EventHandler;
use std::time::Duration;
use tracing::{info, warn};

const DISCORD_PREFIX: &str = "!akasha ";
const DISCORD_POLL_INTERVAL_MS: u64 = 1500;
const DISCORD_MAX_POLL_SECS: u64 = 600;

/// Discord handler that forwards messages to the daemon and posts results back.
pub struct DiscordHandler {
    pub daemon_base_url: String,
}

impl DiscordHandler {
    pub fn new(daemon_base_url: String) -> Self {
        Self { daemon_base_url }
    }
}

#[serenity::async_trait]
impl EventHandler for DiscordHandler {
    async fn message(&self, ctx: Context, msg: Message) {
        // Ignore own messages
        if msg.author.bot {
            return;
        }
        let content = msg.content.trim();
        if !content.starts_with(DISCORD_PREFIX) {
            return;
        }
        let text = content
            .strip_prefix(DISCORD_PREFIX)
            .unwrap_or("")
            .trim()
            .to_string();
        if text.is_empty() {
            let _ = msg.channel_id.say(&ctx, "Usage: `!akasha <your message>`").await;
            return;
        }
        if let Err(e) = akasha_core::check_prompt_injection(&text) {
            let _ = msg.channel_id.say(&ctx, format!("Rejected: {}", e)).await;
            return;
        }

        let url = format!("{}/api/message", self.daemon_base_url);
        let client = reqwest::Client::new();
        let body = serde_json::json!({ "message": text });
        let resp = match client.post(&url).json(&body).send().await {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "Discord: failed to POST to daemon");
                let _ = msg.channel_id.say(&ctx, "Failed to reach Akasha daemon.").await;
                return;
            }
        };
        if !resp.status().is_success() {
            let _ = msg.channel_id.say(&ctx, "Daemon returned an error.").await;
            return;
        }
        let json: serde_json::Value = match resp.json().await {
            Ok(j) => j,
            Err(_) => {
                let _ = msg.channel_id.say(&ctx, "Invalid daemon response.").await;
                return;
            }
        };
        let task_id = match json.get("task_id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => {
                let _ = msg.channel_id.say(&ctx, "No task_id in response.").await;
                return;
            }
        };
        info!(task_id = %task_id, "Discord task created, polling until done");

        let task_url = format!("{}/api/tasks/{}", self.daemon_base_url, task_id);
        let deadline = std::time::Instant::now() + Duration::from_secs(DISCORD_MAX_POLL_SECS);
        loop {
            if std::time::Instant::now() > deadline {
                let _ = msg.channel_id.say(&ctx, "Task timed out.").await;
                break;
            }
            tokio::time::sleep(Duration::from_millis(DISCORD_POLL_INTERVAL_MS)).await;
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
                let _ = msg.channel_id.say(&ctx, summary).await;
                break;
            }
        }
    }
}

/// Run the Discord bot (blocks until shutdown). Call from a tokio::spawn.
pub async fn run_discord_bot(token: String, daemon_port: u16) -> anyhow::Result<()> {
    let base = format!("http://127.0.0.1:{}", daemon_port);
    let handler = DiscordHandler::new(base);
    let intents = serenity::all::GatewayIntents::GUILD_MESSAGES
        | serenity::all::GatewayIntents::DIRECT_MESSAGES
        | serenity::all::GatewayIntents::MESSAGE_CONTENT;
    let mut client = serenity::Client::builder(&token, intents)
        .event_handler(handler)
        .await
        .map_err(|e| anyhow::anyhow!("Discord client build: {}", e))?;
    info!("Discord bot starting");
    client.start().await.map_err(|e| anyhow::anyhow!("Discord client: {}", e))?;
    Ok(())
}
