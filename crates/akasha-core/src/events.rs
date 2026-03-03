//! Event Envelope - Standard format for all Akasha events
//! Based on spec/09_event_model.yaml

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Event types from the specification
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    UserRequestReceived,
    AcknowledgmentSent,
    TaskCreated,
    TaskDecomposed,
    SubAgentSpawned,
    ProgressUpdate,
    TaskCompleted,
    TaskFailed,
    SecurityAlert,
    NodeJoinedCluster,
    NodeFailed,
    LeaderElected,
    ModelFallbackActivated,
    DegradedModeEnabled,
    PluginReputationUpdated,
    ImmutableLogEntryAdded,
}

impl EventType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::UserRequestReceived => "user_request_received",
            Self::AcknowledgmentSent => "acknowledgment_sent",
            Self::TaskCreated => "task_created",
            Self::TaskDecomposed => "task_decomposed",
            Self::SubAgentSpawned => "sub_agent_spawned",
            Self::ProgressUpdate => "progress_update",
            Self::TaskCompleted => "task_completed",
            Self::TaskFailed => "task_failed",
            Self::SecurityAlert => "security_alert",
            Self::NodeJoinedCluster => "node_joined_cluster",
            Self::NodeFailed => "node_failed",
            Self::LeaderElected => "leader_elected",
            Self::ModelFallbackActivated => "model_fallback_activated",
            Self::DegradedModeEnabled => "degraded_mode_enabled",
            Self::PluginReputationUpdated => "plugin_reputation_updated",
            Self::ImmutableLogEntryAdded => "immutable_log_entry_added",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "user_request_received" => Some(Self::UserRequestReceived),
            "acknowledgment_sent" => Some(Self::AcknowledgmentSent),
            "task_created" => Some(Self::TaskCreated),
            "task_decomposed" => Some(Self::TaskDecomposed),
            "sub_agent_spawned" => Some(Self::SubAgentSpawned),
            "progress_update" => Some(Self::ProgressUpdate),
            "task_completed" => Some(Self::TaskCompleted),
            "task_failed" => Some(Self::TaskFailed),
            "security_alert" => Some(Self::SecurityAlert),
            "node_joined_cluster" => Some(Self::NodeJoinedCluster),
            "node_failed" => Some(Self::NodeFailed),
            "leader_elected" => Some(Self::LeaderElected),
            "model_fallback_activated" => Some(Self::ModelFallbackActivated),
            "degraded_mode_enabled" => Some(Self::DegradedModeEnabled),
            "plugin_reputation_updated" => Some(Self::PluginReputationUpdated),
            "immutable_log_entry_added" => Some(Self::ImmutableLogEntryAdded),
            _ => None,
        }
    }
}

/// Standard event envelope for all Akasha events
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    /// Unique event identifier
    pub id: Uuid,
    /// Event type from the spec
    pub event_type: EventType,
    /// Optional payload (event-specific data)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    /// Event timestamp (UTC)
    pub timestamp: DateTime<Utc>,
    /// Correlation ID for tracing related events
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<Uuid>,
}

impl EventEnvelope {
    pub fn new(event_type: EventType, payload: Option<serde_json::Value>) -> Self {
        Self {
            id: Uuid::new_v4(),
            event_type,
            payload,
            timestamp: Utc::now(),
            correlation_id: None,
        }
    }

    pub fn with_correlation(mut self, correlation_id: Uuid) -> Self {
        self.correlation_id = Some(correlation_id);
        self
    }

    pub fn with_payload(mut self, payload: serde_json::Value) -> Self {
        self.payload = Some(payload);
        self
    }
}
