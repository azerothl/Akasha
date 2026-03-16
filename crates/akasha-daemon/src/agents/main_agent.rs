//! Main Agent — entry point, ack < 500ms, task creation, routing to Orchestrator or Direct to conversation (Plan: Architecture agents et pipeline).

use akasha_core::{EventEnvelope, EventType};
use akasha_store::{Task, TaskStatus, TaskStore};
use chrono::Utc;
use std::path::Path;
use tokio::sync::mpsc;
use tokio::task;
use uuid::Uuid;

use super::{classify_execution_mode, EventBus, ExecutionMode};

/// Priority for the task queue: high-priority tasks are processed before normal/scheduled (Phase 4.1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TaskPriority {
    #[default]
    UserNormal,
    UserHigh,
    Scheduled,
}

/// Message sent to the orchestrator or (when Direct) to the conversation worker: root task id, user message, session id (for memory), optional image data URLs for vision.
#[derive(Clone, Debug)]
pub struct OrchestratorTask {
    pub task_id: Uuid,
    pub message: String,
    pub session_id: String,
    pub image_data_urls: Option<Vec<String>>,
    /// When present, orchestrator may use for lighter (Guided) or full (Orchestrated) pipeline. Absent when sent Direct to conversation.
    pub execution_mode: Option<ExecutionMode>,
}

/// Sender that routes tasks to high or normal priority channel (multiplexer feeds orchestrator).
#[derive(Clone)]
pub struct OrchestratorSender {
    high_tx: mpsc::Sender<OrchestratorTask>,
    normal_tx: mpsc::Sender<OrchestratorTask>,
}

impl OrchestratorSender {
    pub fn new(high_tx: mpsc::Sender<OrchestratorTask>, normal_tx: mpsc::Sender<OrchestratorTask>) -> Self {
        Self { high_tx, normal_tx }
    }
    pub fn send(&self, task: OrchestratorTask, priority: TaskPriority) {
        let tx = match priority {
            TaskPriority::UserHigh => self.high_tx.clone(),
            TaskPriority::UserNormal | TaskPriority::Scheduled => self.normal_tx.clone(),
        };
        if let Err(e) = tx.try_send(task) {
            match e {
                mpsc::error::TrySendError::Full(task) => {
                    tracing::warn!(task_id = %task.task_id, "orchestrator channel full; sending asynchronously to avoid dropping task");
                    tokio::spawn(async move {
                        if let Err(send_err) = tx.send(task).await {
                            tracing::error!(task_id = %send_err.0.task_id, "orchestrator channel closed; task dropped");
                        }
                    });
                }
                mpsc::error::TrySendError::Closed(task) => {
                    tracing::error!(task_id = %task.task_id, "orchestrator channel closed; task dropped");
                }
            }
        }
    }
}

#[derive(Clone)]
pub struct MainAgent {
    bus: EventBus,
    orchestrator: OrchestratorSender,
    /// When Some, Direct mode tasks are sent here (conversation worker) instead of to the orchestrator.
    direct_conversation_tx: Option<mpsc::Sender<OrchestratorTask>>,
}

impl MainAgent {
    pub fn new(bus: EventBus, orchestrator: OrchestratorSender) -> Self {
        Self {
            bus,
            orchestrator,
            direct_conversation_tx: None,
        }
    }

    /// Builder: set the channel for Direct-mode tasks (conversation worker). When set, handle_message will route Direct tasks here.
    pub fn with_direct_conversation_tx(mut self, tx: mpsc::Sender<OrchestratorTask>) -> Self {
        self.direct_conversation_tx = Some(tx);
        self
    }

    /// Handle user message: ack immediately, create root task, emit events.
    /// If `forward_to_orchestrator` is true, send task to orchestrator for non-blocking delegation.
    /// If false, the caller is responsible for completing the task (e.g. via LLM and ProgressUpdate + TaskCompleted).
    /// session_id: used for short-term memory; if empty, a default "default" is used so all messages share one session.
    /// image_data_urls: optional list of data URLs (data:image/...;base64,...) for vision-capable models.
    /// priority: used when forward_to_orchestrator is true; UserHigh tasks are processed before UserNormal/Scheduled.
    pub fn handle_message(
        &self,
        store_path: &Path,
        message: &str,
        _correlation_id: Uuid,
        forward_to_orchestrator: bool,
        session_id: &str,
        image_data_urls: Option<Vec<String>>,
        priority: TaskPriority,
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

        const MAX_INITIAL_MESSAGE: usize = 500;
        let initial_message = if message.is_empty() {
            None
        } else {
            Some(if message.chars().count() > MAX_INITIAL_MESSAGE {
                message.chars().take(MAX_INITIAL_MESSAGE).chain(std::iter::once('…')).collect::<String>()
            } else {
                message.to_string()
            })
        };

        let execution_mode = if forward_to_orchestrator {
            Some(classify_execution_mode(message))
        } else {
            None
        };

        let use_direct = forward_to_orchestrator
            && execution_mode == Some(ExecutionMode::Direct)
            && self.direct_conversation_tx.is_some();

        let assigned_agent = if !forward_to_orchestrator {
            "llm".to_string()
        } else if use_direct {
            "conversation".to_string()
        } else {
            "orchestrator".to_string()
        };

        let store = TaskStore::open(store_path)?;
        let task = Task {
            id: task_id,
            parent_task_id: None,
            status: TaskStatus::Pending,
            assigned_agent: assigned_agent.clone(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            initial_message,
        };
        store.insert(&task)?;
        let _ = self.bus.send(
            EventEnvelope::new(
                EventType::TaskCreated,
                Some(serde_json::json!({
                    "task_id": task_id.to_string(),
                    "parent_task_id": null,
                    "assigned_agent": task.assigned_agent,
                    "execution_mode": execution_mode.map(|e| e.as_str())
                })),
            )
            .with_correlation(task_id),
        );

        if forward_to_orchestrator {
            if use_direct {
                let tx = self.direct_conversation_tx.as_ref().unwrap().clone();
                let task_msg = OrchestratorTask {
                    task_id,
                    message: message.to_string(),
                    session_id: session_id.to_string(),
                    image_data_urls,
                    execution_mode: None,
                };
                if let Err(e) = tx.try_send(task_msg) {
                    match e {
                        mpsc::error::TrySendError::Full(t) => {
                            tokio::spawn(async move {
                                let _ = tx.send(t).await;
                            });
                        }
                        mpsc::error::TrySendError::Closed(_) => {
                            tracing::error!(task_id = %task_id, "direct conversation channel closed; task dropped");
                        }
                    }
                }
            } else {
                self.orchestrator.send(
                    OrchestratorTask {
                        task_id,
                        message: message.to_string(),
                        session_id: session_id.to_string(),
                        image_data_urls,
                        execution_mode,
                    },
                    priority,
                );
            }
        }
        Ok(task_id)
    }

    pub fn bus(&self) -> &EventBus {
        &self.bus
    }

    /// Resume a Paused or Interrupted task: set status to Queued and re-inject into conversation worker (Phase 2 AI OS).
    pub fn resume_task(&self, store_path: &Path, task_id: Uuid) -> anyhow::Result<()> {
        let store = TaskStore::open(store_path)?;
        let task = store.get(task_id)?.ok_or_else(|| anyhow::anyhow!("task not found"))?;
        let resumable = matches!(
            task.status,
            TaskStatus::Paused | TaskStatus::Interrupted
        );
        if !resumable {
            anyhow::bail!("task not resumable (status: {})", task.status.as_str());
        }
        // Remember the original status so we can roll back on channel closure.
        let original_status = task.status;
        let message = task
            .initial_message
            .clone()
            .unwrap_or_else(|| "(Reprise)".to_string());
        let session_id = task
            .session_id
            .clone()
            .unwrap_or_else(|| format!("day-{}", chrono::Utc::now().format("%Y-%m-%d")));
        let execution_mode = task.execution_mode.clone();
        let tx = self
            .direct_conversation_tx
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("conversation channel not available"))?;
        let task_msg = OrchestratorTask {
            task_id,
            message,
            session_id,
            image_data_urls: None,
            execution_mode,
        };
        if let Err(e) = tx.try_send(task_msg) {
            match e {
                mpsc::error::TrySendError::Full(task_msg) => {
                    // Channel is full: fall back to an async send and block until there is capacity.
                    let send_result = task::block_in_place(|| {
                        tokio::runtime::Handle::current().block_on(async move {
                            tx.send(task_msg).await
                        })
                    });
                    send_result.map_err(|e| {
                        anyhow::anyhow!(
                            "failed to enqueue resumed task after channel full: {}",
                            e
                        )
                    })?;
                }
                mpsc::error::TrySendError::Closed(_) => {
                    // Channel is closed: attempt to roll back the status and report a clear error.
                    let _ = store.update_status(task_id, original_status);
                    anyhow::bail!("conversation channel closed when resuming task")
                }
            }
        }

        // Only mark the task as queued and emit the resume event after enqueueing is ensured.
        store.update_status(task_id, TaskStatus::Queued)?;
        let _ = self.bus.send(
            EventEnvelope::new(
                EventType::TaskResumed,
                Some(serde_json::json!({
                    "task_id": task_id.to_string(),
                    "resumed": true
                })),
            )
            .with_correlation(task_id),
        );
        Ok(())
    }
}
