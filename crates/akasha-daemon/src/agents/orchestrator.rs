//! Orchestrator — single entry point: receive (task_id, message), decompose (LLM), delegate to workers, aggregate (Phase E).

use akasha_core::{EventEnvelope, EventType};
use akasha_llm::CompletionRequest;
use akasha_store::{Task, TaskStatus, TaskStore};
use chrono::Utc;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::mpsc;
use uuid::Uuid;

use super::{EventBus, OrchestratorTask};
use crate::api::ProgressCache;

/// One subtask from decomposition: (agent_type, message for that agent).
pub type Subtask = (String, String);

/// Decompose a user request into one or more subtasks via LLM. Falls back to single "conversation" on error, timeout or empty.
async fn decompose_request(
    llm_router: &Arc<akasha_llm::LLMRouter>,
    message: &str,
) -> Vec<Subtask> {
    let prompt = format!(
        "You are a task decomposer. Output one line per subtask: agent_type|message. Agent types: conversation (general chat), code (code gen), search (info search). Use 'conversation' if one simple question. Example:\nconversation|What is 2+2?\n\nUser request:\n\n{}",
        message
    );
    let request = CompletionRequest {
        prompt,
        max_tokens: Some(512),
        temperature: Some(0.2),
    };
    let decompose_timeout = std::time::Duration::from_secs(120);
    match tokio::time::timeout(decompose_timeout, llm_router.complete(&request)).await {
        Ok(Ok(resp)) => {
            let text = resp.text.trim();
            let mut steps = Vec::new();
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some((agent_type, sub_message)) = line.split_once('|') {
                    let agent_type = agent_type.trim().to_lowercase();
                    let agent_type = if agent_type == "code" || agent_type == "search" {
                        agent_type
                    } else {
                        "conversation".to_string()
                    };
                    steps.push((agent_type, sub_message.trim().to_string()));
                }
            }
            if steps.is_empty() {
                vec![("conversation".to_string(), message.to_string())]
            } else {
                steps
            }
        }
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "Decompose LLM failed, using single conversation step");
            vec![("conversation".to_string(), message.to_string())]
        }
        Err(_) => {
            tracing::debug!("Decompose LLM timed out, using single conversation step");
            vec![("conversation".to_string(), message.to_string())]
        }
    }
}

pub struct Orchestrator {
    bus: EventBus,
    store_path: std::path::PathBuf,
    conversation_tx: mpsc::Sender<OrchestratorTask>,
    progress: ProgressCache,
    llm_router: Arc<akasha_llm::LLMRouter>,
}

impl Orchestrator {
    pub fn new(
        bus: EventBus,
        store_path: std::path::PathBuf,
        conversation_tx: mpsc::Sender<OrchestratorTask>,
        progress: ProgressCache,
        llm_router: Arc<akasha_llm::LLMRouter>,
    ) -> Self {
        Self {
            bus,
            store_path,
            conversation_tx,
            progress,
            llm_router,
        }
    }

    pub async fn run(
        self: Arc<Self>,
        mut rx: mpsc::Receiver<OrchestratorTask>,
    ) {
        while let Some((root_task_id, message, session_id)) = rx.recv().await {
            let bus = self.bus.clone();
            let store_path = self.store_path.clone();
            let conv_tx = self.conversation_tx.clone();
            let progress = self.progress.clone();
            let llm_router = self.llm_router.clone();
            tokio::spawn(async move {
                if let Err(e) = process_root_task(
                    bus,
                    store_path.as_path(),
                    root_task_id,
                    message,
                    session_id,
                    conv_tx,
                    progress,
                    llm_router,
                )
                .await
                {
                    tracing::error!(error = %e, task_id = %root_task_id, "Orchestrator failed");
                }
            });
        }
    }
}

async fn process_root_task(
    bus: EventBus,
    store_path: &Path,
    root_task_id: Uuid,
    message: String,
    session_id: String,
    conversation_tx: mpsc::Sender<OrchestratorTask>,
    progress: ProgressCache,
    llm_router: Arc<akasha_llm::LLMRouter>,
) -> anyhow::Result<()> {
    if !akasha_core::Role::OrchestratorAgent.can_spawn_agents() {
        anyhow::bail!("RBAC: orchestrator not allowed to spawn agents");
    }
    let store = TaskStore::open(store_path)?;
    store.update_status(root_task_id, TaskStatus::Running)?;

    // Progress immédiat pour que la TUI affiche un retour avant le premier appel LLM (chargement modèle possible).
    let _ = bus.send(
        EventEnvelope::new(
            EventType::ProgressUpdate,
            Some(serde_json::json!({
                "task_id": root_task_id.to_string(),
                "progress_pct": 0,
                "message": "Analyse de la demande…"
            })),
        )
        .with_correlation(root_task_id),
    );

    let steps = decompose_request(&llm_router, &message).await;
    let _ = bus.send(
        EventEnvelope::new(
            EventType::TaskDecomposed,
            Some(serde_json::json!({
                "task_id": root_task_id.to_string(),
                "subtask_count": steps.len(),
                "agents": steps.iter().map(|(a, _)| a.as_str()).collect::<Vec<_>>()
            })),
        )
        .with_correlation(root_task_id),
    );

    // Progress message so UIs can show "task delegated to specialized agent(s)"
    let delegation_msg = if steps.len() == 1 {
        format!("Tâche déléguée à l'agent « {} ».", steps[0].0)
    } else {
        let agents: Vec<&str> = steps.iter().map(|(a, _)| a.as_str()).collect();
        format!(
            "Tâche décomposée en {} sous-tâche(s) — agents spécialisés : {}.",
            steps.len(),
            agents.join(", ")
        )
    };
    let _ = bus.send(
        EventEnvelope::new(
            EventType::ProgressUpdate,
            Some(serde_json::json!({
                "task_id": root_task_id.to_string(),
                "progress_pct": 0,
                "message": delegation_msg
            })),
        )
        .with_correlation(root_task_id),
    );

    // Single subtask (conversation): delegate to conversation worker for root (user sees reply on root_id).
    if steps.len() == 1 && steps[0].0 == "conversation" {
        conversation_tx
            .send((root_task_id, steps[0].1.clone(), session_id))
            .await
            .map_err(|_| anyhow::anyhow!("conversation channel closed"))?;
        return Ok(());
    }

    // Multiple subtasks: create child tasks and delegate each (future: aggregate when all done).
    for (agent_type, sub_message) in &steps {
        let child_id = Uuid::new_v4();
        let task = Task {
            id: child_id,
            parent_task_id: Some(root_task_id),
            status: TaskStatus::Pending,
            assigned_agent: agent_type.clone(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        store.insert(&task)?;
        let _ = bus.send(
            EventEnvelope::new(
                EventType::SubAgentSpawned,
                Some(serde_json::json!({
                    "task_id": child_id.to_string(),
                    "parent_id": root_task_id.to_string(),
                    "agent": agent_type
                })),
            )
            .with_correlation(root_task_id),
        );
        // Delegate to conversation worker for all types (code/search handled as conversation for now). Pass same session_id for memory.
        let _ = conversation_tx.send((child_id, sub_message.clone(), session_id.clone())).await;
    }
    // Aggregator: when all children are done, collect their progress messages, push aggregated ProgressUpdate for root, then complete root.
    let store_path_buf = store_path.to_path_buf();
    let steps_count = steps.len();
    tokio::spawn(async move {
        let store = match TaskStore::open(&store_path_buf) {
            Ok(s) => s,
            Err(_) => return,
        };
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
        loop {
            interval.tick().await;
            let children = match store.get_children(root_task_id) {
                Ok(c) => c,
                Err(_) => break,
            };
            if children.is_empty() {
                break;
            }
            let all_done = children.iter().all(|t| matches!(t.status, TaskStatus::Completed | TaskStatus::Failed));
            if all_done {
                // Aggregate last progress message from each child for the root so the UI shows combined result.
                let mut parts: Vec<String> = Vec::new();
                {
                    let g = progress.read().await;
                    for child in &children {
                        if let Some(q) = g.get(&child.id) {
                            if let Some(last) = q.back() {
                                if !last.message.is_empty() && last.message != "Done." {
                                    parts.push(last.message.clone());
                                }
                            }
                        }
                    }
                }
                let aggregated = if parts.is_empty() {
                    "Done.".to_string()
                } else {
                    parts.join("\n\n---\n\n")
                };
                let _ = bus.send(
                    EventEnvelope::new(
                        EventType::ProgressUpdate,
                        Some(serde_json::json!({
                            "task_id": root_task_id.to_string(),
                            "progress_pct": 100,
                            "message": aggregated
                        })),
                    )
                    .with_correlation(root_task_id),
                );
                let any_failed = children.iter().any(|t| t.status == TaskStatus::Failed);
                let root_status = if any_failed { TaskStatus::Failed } else { TaskStatus::Completed };
                let status_str = root_status.as_str();
                let _ = store.update_status(root_task_id, root_status);
                let event_type = if any_failed { EventType::TaskFailed } else { EventType::TaskCompleted };
                let _ = bus.send(
                    EventEnvelope::new(
                        event_type,
                        Some(serde_json::json!({
                            "task_id": root_task_id.to_string(),
                            "status": status_str,
                            "subtasks": steps_count
                        })),
                    )
                    .with_correlation(root_task_id),
                );
                break;
            }
        }
    });
    Ok(())
}
