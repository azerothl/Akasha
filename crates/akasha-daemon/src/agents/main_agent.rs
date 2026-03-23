//! Main Agent — entry point, ack < 500ms, task creation, routing to Orchestrator or Direct to conversation (Plan: Architecture agents et pipeline).

use akasha_core::{EventEnvelope, EventType};
use akasha_llm::CompletionRequest;
use akasha_store::{Task, TaskStatus, TaskStore, TodoStatus};
use chrono::Utc;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::mpsc;
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
    /// Optional system-selected task type for routing (falls back to current classifier/router when absent).
    pub preferred_task_type: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SelectorAnswerMode {
    Direct,
    Delegate,
}

#[derive(Clone, Debug)]
struct SelectorDecision {
    answer_mode: SelectorAnswerMode,
    task_type: Option<String>,
    target_agent: Option<String>,
    reason: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct SelectorRawDecision {
    answer_mode: Option<String>,
    task_type: Option<String>,
    target_agent: Option<String>,
    reason: Option<String>,
}

fn normalize_task_type(task_type: Option<&str>) -> Option<String> {
    let t = task_type?.trim().to_lowercase();
    if t.is_empty() {
        return None;
    }
    let normalized = match t.as_str() {
        "conversation" | "code_generation" | "creative_writing" | "scientific_analysis"
        | "data_analysis" | "system_diagnostic" | "system" | "orchestrator" | "image_generation"
        | "financial" | "documentalist" | "project_manager" | "technical_writer" | "research"
        | "security_audit" | "creative" => t,
        _ => return None,
    };
    Some(normalized)
}

fn parse_selector_decision(raw_json: &str) -> Option<SelectorDecision> {
    let parsed: SelectorRawDecision = serde_json::from_str(raw_json).ok()?;
    let answer_mode = match parsed.answer_mode.as_deref().map(|s| s.trim().to_lowercase()) {
        Some(v) if v == "direct" => SelectorAnswerMode::Direct,
        Some(v) if v == "delegate" => SelectorAnswerMode::Delegate,
        _ => return None,
    };
    Some(SelectorDecision {
        answer_mode,
        task_type: normalize_task_type(parsed.task_type.as_deref()),
        target_agent: parsed
            .target_agent
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty()),
        reason: parsed.reason.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
    })
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
    llm_router: Arc<akasha_llm::LLMRouter>,
    /// When Some, Direct mode tasks are sent here (conversation worker) instead of to the orchestrator.
    direct_conversation_tx: Option<mpsc::Sender<OrchestratorTask>>,
}

impl MainAgent {
    pub fn new(
        bus: EventBus,
        orchestrator: OrchestratorSender,
        llm_router: Arc<akasha_llm::LLMRouter>,
    ) -> Self {
        Self {
            bus,
            orchestrator,
            llm_router,
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
    async fn system_selector_decision(&self, message: &str) -> Option<SelectorDecision> {
        let enabled = std::env::var("AKASHA_SYSTEM_TASK_SELECTOR")
            .ok()
            .map(|s| s != "0" && !s.eq_ignore_ascii_case("false"))
            .unwrap_or(true);
        if !enabled {
            return None;
        }
        let prompt = format!(
            "You are a strict routing selector for Akasha.\n\
Return ONLY compact JSON with schema:\n\
{{\"answer_mode\":\"direct|delegate\",\"task_type\":\"snake_case or empty\",\"target_agent\":\"optional\",\"reason\":\"short\"}}\n\
Rules:\n\
- answer_mode=direct when the request can be answered in one pass without decomposition/delegation.\n\
- answer_mode=delegate for multi-step/project/planning/complex implementation requests.\n\
- task_type must be one of: conversation, code_generation, creative_writing, scientific_analysis, data_analysis, system_diagnostic, system, orchestrator, image_generation.\n\
- If unsure, use answer_mode=delegate and task_type=conversation.\n\
User message:\n{}",
            message
        );
        let req = CompletionRequest {
            prompt,
            max_tokens: Some(128),
            temperature: Some(0.0),
            preferred_task_type: Some("system".to_string()),
            system_prompt: None,
            image_data_urls: None,
        };
        let timeout = std::time::Duration::from_secs(8);
        let resp = tokio::time::timeout(timeout, self.llm_router.complete(&req))
            .await
            .ok()?
            .ok()?;
        parse_selector_decision(resp.text.trim())
    }

    pub async fn handle_message(
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

        let selector_decision = if forward_to_orchestrator {
            self.system_selector_decision(message).await
        } else {
            None
        };
        let execution_mode = if forward_to_orchestrator {
            Some(classify_execution_mode(message))
        } else {
            None
        };

        let selector_direct = selector_decision
            .as_ref()
            .map(|d| d.answer_mode == SelectorAnswerMode::Direct)
            .unwrap_or(false);
        let use_direct = forward_to_orchestrator
            && (selector_direct || execution_mode == Some(ExecutionMode::Direct))
            && self.direct_conversation_tx.is_some();
        let preferred_task_type = selector_decision
            .as_ref()
            .and_then(|d| d.task_type.clone());
        let preferred_task_type_event = preferred_task_type.clone();
        let selector_used = selector_decision.is_some();
        let selector_mode = selector_decision.as_ref().map(|d| match d.answer_mode {
            SelectorAnswerMode::Direct => "direct",
            SelectorAnswerMode::Delegate => "delegate",
        });
        let selector_target_agent = selector_decision.as_ref().and_then(|d| d.target_agent.clone());
        let selector_reason = selector_decision.as_ref().and_then(|d| d.reason.clone());

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
                    "execution_mode": execution_mode.map(|e| e.as_str()),
                    "selector_used": selector_used,
                    "selector_mode": selector_mode,
                    "selector_task_type": preferred_task_type_event,
                    "selector_target_agent": selector_target_agent,
                    "selector_reason": selector_reason,
                    "fallback_used": !selector_used
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
                    preferred_task_type: None,
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
                        preferred_task_type,
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
        let message = match store.get_todos(task_id) {
            Ok(todos)
                if todos
                    .iter()
                    .any(|t| matches!(t.status, TodoStatus::Pending)) =>
            {
                "(Reprise automatique — poursuivre le plan d'étapes en cours ; ne pas repartir de zéro.)".to_string()
            }
            _ => task
                .initial_message
                .clone()
                .unwrap_or_else(|| "(Reprise)".to_string()),
        };
        // Session and execution mode are not persisted in the task store; use a
        // day-scoped session id and direct conversation mode when resuming.
        let session_id = format!("day-{}", chrono::Utc::now().format("%Y-%m-%d"));
        let execution_mode = None;
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
            preferred_task_type: None,
        };
        // Update status to Queued BEFORE enqueuing so the conversation worker won't
        // see a Paused/Interrupted status and silently drop the task.
        store.update_status(task_id, TaskStatus::Queued)?;
        if let Err(e) = tx.try_send(task_msg) {
            match e {
                mpsc::error::TrySendError::Full(_) => {
                    // Channel is full: roll back status and return a clear error instead of blocking.
                    let _ = store.update_status(task_id, original_status);
                    anyhow::bail!("conversation queue full when resuming task")
                }
                mpsc::error::TrySendError::Closed(_) => {
                    // Channel is closed: attempt to roll back the status and report a clear error.
                    let _ = store.update_status(task_id, original_status);
                    anyhow::bail!("conversation channel closed when resuming task")
                }
            }
        }
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

#[cfg(test)]
mod tests {
    use super::{normalize_task_type, parse_selector_decision, SelectorAnswerMode};

    #[test]
    fn selector_parse_valid_direct() {
        let raw = r#"{"answer_mode":"direct","task_type":"conversation","reason":"simple qa"}"#;
        let d = parse_selector_decision(raw).expect("decision should parse");
        assert_eq!(d.answer_mode, SelectorAnswerMode::Direct);
        assert_eq!(d.task_type.as_deref(), Some("conversation"));
    }

    #[test]
    fn selector_parse_invalid_mode_is_none() {
        let raw = r#"{"answer_mode":"maybe","task_type":"conversation"}"#;
        assert!(parse_selector_decision(raw).is_none());
    }

    #[test]
    fn selector_normalize_rejects_unknown_task_type() {
        assert_eq!(normalize_task_type(Some("unknown_type")), None);
    }
}
