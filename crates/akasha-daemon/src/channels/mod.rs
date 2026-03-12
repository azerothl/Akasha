//! Phase 4: Multi-channel adapters (Slack, Discord, Telegram, Teams).
//! Same task_id is visible and controllable from any channel; messages are normalized to internal format.

pub mod discord;
pub mod slack;
pub mod telegram;
pub mod teams;

/// Channel adapter configuration (secrets from vault, port for callbacks).
#[derive(Clone, Default)]
pub struct ChannelConfig {
    pub port: u16,
    pub slack_signing_secret: Option<String>,
    /// Microsoft Bot Framework: app id (from Azure Bot registration).
    pub teams_app_id: Option<String>,
    /// Microsoft Bot Framework: app password (from vault).
    pub teams_app_password: Option<String>,
}
