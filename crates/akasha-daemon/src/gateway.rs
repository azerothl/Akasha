//! Gateway layer: normalizes inputs from all channels into a single MessageEnvelope
//! and delegates task creation + routing to the core (Main Agent).
//!
//! See spec/48_gateway_layer.md.

use crate::agents::{MainAgent, TaskPriority};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Channel type identifier for the message source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelType {
    Api,
    Slack,
    Teams,
    Discord,
    Telegram,
}

impl ChannelType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ChannelType::Api => "api",
            ChannelType::Slack => "slack",
            ChannelType::Teams => "teams",
            ChannelType::Discord => "discord",
            ChannelType::Telegram => "telegram",
        }
    }
}

/// Normalized message envelope produced by channel adapters and consumed by the core.
#[derive(Debug, Clone)]
pub struct MessageEnvelope {
    /// Source channel type.
    pub channel_type: ChannelType,
    /// Optional channel identifier (e.g. Slack channel id, Teams conversation id).
    pub channel_id: Option<String>,
    /// Session identifier for short-term memory and grouping (e.g. "day-2025-03-15", "slack", or UUID string).
    pub session_id: String,
    /// Optional user identifier.
    pub user_id: Option<String>,
    /// Optional workspace identifier.
    pub workspace_id: Option<String>,
    /// User message text (possibly enriched with attachments or context by the adapter).
    pub raw_message: String,
    /// Optional image data URLs for vision-capable models.
    pub image_data_urls: Option<Vec<String>>,
    /// Task priority when forwarding to orchestrator.
    pub priority: TaskPriority,
    /// When set, tools use this disk root for `workspace:/` mirror and `run_command` cwd (Code Studio).
    pub studio_disk_root: Option<PathBuf>,
    /// When set, skip the system selector and route direct to this specialist agent (must be in SPECIALIST_AGENTS).
    pub studio_forced_agent: Option<String>,
    /// Optional git branch hint prepended to the user message for agents.
    pub studio_evolution_branch: Option<String>,
    /// When true, skip long-term memory promotion for this message.
    pub incognito: bool,
}

impl MessageEnvelope {
    pub fn api<S, M>(
        session_id: S,
        raw_message: M,
        image_data_urls: Option<Vec<String>>,
        priority: TaskPriority,
        incognito: bool,
    ) -> Self
    where
        S: Into<String>,
        M: Into<String>,
    {
        Self {
            channel_type: ChannelType::Api,
            channel_id: None,
            session_id: session_id.into(),
            user_id: None,
            workspace_id: None,
            raw_message: raw_message.into(),
            image_data_urls,
            priority,
            studio_disk_root: None,
            studio_forced_agent: None,
            studio_evolution_branch: None,
            incognito,
        }
    }

    pub fn slack<S, M>(session_id: S, raw_message: M) -> Self
    where
        S: Into<String>,
        M: Into<String>,
    {
        Self {
            channel_type: ChannelType::Slack,
            channel_id: None,
            session_id: session_id.into(),
            user_id: None,
            workspace_id: None,
            raw_message: raw_message.into(),
            image_data_urls: None,
            priority: TaskPriority::UserNormal,
            studio_disk_root: None,
            studio_forced_agent: None,
            studio_evolution_branch: None,
            incognito: false,
        }
    }

    pub fn teams<S, M>(session_id: S, raw_message: M, channel_id: Option<String>) -> Self
    where
        S: Into<String>,
        M: Into<String>,
    {
        Self {
            channel_type: ChannelType::Teams,
            channel_id,
            session_id: session_id.into(),
            user_id: None,
            workspace_id: None,
            raw_message: raw_message.into(),
            image_data_urls: None,
            priority: TaskPriority::UserNormal,
            studio_disk_root: None,
            studio_forced_agent: None,
            studio_evolution_branch: None,
            incognito: false,
        }
    }
}

/// Single entry point: create task and push to Main Agent from a normalized envelope.
/// Returns the created task_id on success.
pub async fn handle_envelope(
    main_agent: &MainAgent,
    store_path: &Path,
    envelope: MessageEnvelope,
) -> anyhow::Result<Uuid> {
    if let Some(data_dir) = store_path.parent() {
        let hook_payload = serde_json::json!({
            "channel_type": envelope.channel_type.as_str(),
            "channel_id": envelope.channel_id,
            "session_id": envelope.session_id,
            "user_id": envelope.user_id,
            "message_preview": envelope.raw_message.chars().take(800).collect::<String>(),
        });
        crate::plugin_hook_bus::dispatch_hook_event(
            data_dir,
            "on_channel_message",
            &hook_payload.to_string(),
        );
    }
    main_agent
        .handle_message(
        store_path,
        &envelope.raw_message,
        Uuid::nil(),
        true,
        &envelope.session_id,
        envelope.image_data_urls,
        envelope.priority,
        envelope.studio_disk_root.clone(),
        envelope.studio_forced_agent.clone(),
        envelope.studio_evolution_branch.clone(),
        envelope.incognito,
    )
        .await
}
