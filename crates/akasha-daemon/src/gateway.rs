//! Gateway layer: normalizes inputs from all channels into a single MessageEnvelope
//! and delegates task creation + routing to the core (Main Agent).
//!
//! See spec/48_gateway_layer.md.

use crate::agents::{MainAgent, TaskPriority};
use std::path::Path;
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
}

impl MessageEnvelope {
    pub fn api(session_id: String, raw_message: String, image_data_urls: Option<Vec<String>>, priority: TaskPriority) -> Self {
        Self {
            channel_type: ChannelType::Api,
            channel_id: None,
            session_id,
            user_id: None,
            workspace_id: None,
            raw_message,
            image_data_urls,
            priority,
        }
    }

    pub fn slack(session_id: String, raw_message: String) -> Self {
        Self {
            channel_type: ChannelType::Slack,
            channel_id: None,
            session_id,
            user_id: None,
            workspace_id: None,
            raw_message,
            image_data_urls: None,
            priority: TaskPriority::UserNormal,
        }
    }

    pub fn teams(session_id: String, raw_message: String, channel_id: Option<String>) -> Self {
        Self {
            channel_type: ChannelType::Teams,
            channel_id,
            session_id,
            user_id: None,
            workspace_id: None,
            raw_message,
            image_data_urls: None,
            priority: TaskPriority::UserNormal,
        }
    }
}

/// Single entry point: create task and push to Main Agent from a normalized envelope.
/// Returns the created task_id on success.
pub fn handle_envelope(
    main_agent: &MainAgent,
    store_path: &Path,
    envelope: &MessageEnvelope,
) -> anyhow::Result<Uuid> {
    main_agent.handle_message(
        store_path,
        &envelope.raw_message,
        Uuid::new_v4(),
        true,
        &envelope.session_id,
        envelope.image_data_urls.clone(),
        envelope.priority,
    )
}
