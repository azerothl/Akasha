//! Phase 4: Multi-channel adapters (Slack, Discord, Telegram).
//! Same task_id is visible and controllable from any channel; messages are normalized to internal format.

pub mod discord;
pub mod slack;
pub mod telegram;

/// Channel adapter configuration (secrets from vault, port for callbacks).
#[derive(Clone, Default)]
pub struct ChannelConfig {
    pub port: u16,
    pub slack_signing_secret: Option<String>,
}
