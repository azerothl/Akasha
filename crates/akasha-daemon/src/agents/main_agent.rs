//! Main Agent — entry point, ack < 500ms, task creation, routing to Orchestrator

use akasha_core::{EventEnvelope, EventType};
use akasha_store::{Task, TaskStatus, TaskStore};
use chrono::Utc;
use std::path::Path;
use tokio::sync::mpsc;
use uuid::Uuid;

use super::EventBus;

/// Message sent to the orchestrator: root task id, user message, session id (for memory).
pub type OrchestratorTask = (Uuid, String, String);

#[derive(Clone)]
pub struct MainAgent {
    bus: EventBus,
    orchestrator_tx: mpsc::Sender<OrchestratorTask>,
}

impl MainAgent {
    pub fn new(bus: EventBus, orchestrator_tx: mpsc::Sender<OrchestratorTask>) -> Self {
        Self { bus, orchestrator_tx }
    }

    /// Handle user message: ack immediately, create root task, emit events.
    /// If `forward_to_orchestrator` is true, send (task_id, message, session_id) to orchestrator for non-blocking delegation.
    /// If false, the caller is responsible for completing the task (e.g. via LLM and ProgressUpdate + TaskCompleted).
    /// session_id: used for short-term memory; if empty, a default "default" is used so all messages share one session.
    pub fn handle_message(
        &self,
        store_path: &Path,
        message: &str,
        _correlation_id: Uuid,
        forward_to_orchestrator: bool,
        session_id: &str,
    ) -> anyhow::Result<Uuid> {
        let session_id = if session_id.is_empty() { "default" } else { session_id };
        let task_id = Uuid::new_v4();

        // Use task_id as correlation so GET /api/tasks/{task_id}/events returns these events.
        let _ = self.bus.send(EventEnvelope::new(EventType::UserRequestReceived, Some(serde_json::json!({ "message": message }))).with_correlation(task_id));
        let _ = self.bus.send(
            EventEnvelope::new(
                EventType::AcknowledgmentSent,
                Some(serde_json::json!({ "task_id": task_id.to_string() })),
            )
            .with_correlation(task_id),
        );

        let store = TaskStore::open(store_path)?;
        let task = Task {
            id: task_id,
            parent_task_id: None,
            status: TaskStatus::Pending,
            assigned_agent: if forward_to_orchestrator { "orchestrator" } else { "llm" }.to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        store.insert(&task)?;
        let _ = self.bus.send(
            EventEnvelope::new(
                EventType::TaskCreated,
                Some(serde_json::json!({
                    "task_id": task_id.to_string(),
                    "parent_task_id": null,
                    "assigned_agent": task.assigned_agent
                })),
            )
            .with_correlation(task_id),
        );

        if forward_to_orchestrator {
            let _ = self.orchestrator_tx.try_send((task_id, message.to_string(), session_id.to_string()));
        }
        Ok(task_id)
    }

    pub fn bus(&self) -> &EventBus {
        &self.bus
    }
}
