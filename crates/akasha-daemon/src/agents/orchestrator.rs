//! Orchestrator — single entry point: receive (task_id, message), decompose (LLM), delegate to workers, aggregate (Phase E).

use akasha_core::{EventEnvelope, EventType};
use akasha_llm::CompletionRequest;
use akasha_store::{Schedule, ScheduleStore, Task, TaskStatus, TaskStore};
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
        r#"You are a task decomposer. Output one line per subtask: agent_type|message. One line per distinct user action (e.g. one for generating a report, another for creating a file).
Agent types: conversation (general chat), code (code gen), search (info search), schedule (create recurring task IN THE APP).
- If the user asks to CREATE a recurring/scheduled task (e.g. "tâche récurrente", "rappel toutes les 2 heures", "crée un rappel"), output exactly ONE line: schedule|interval_seconds|name|message
  where interval_seconds is in seconds (3600=1h, 7200=2h, 86400=1 day), name is a short title, message is the reminder text shown when the task runs. Example: schedule|7200|Rappel Github|Rappel: regarder l'avancement du projet sur GitHub
- If the user asks for several distinct deliverables or actions (e.g. "make a report and then create a file", "do X then do Y"), output one line per deliverable/action. Example: first line for the report, second line for creating the file.
- Otherwise output agent_type|message. Example: conversation|What is 2+2?

User request:

{}"#,
        message
    );
    // Models with "thinking" (e.g. glm-4.7-flash) use output tokens for thinking then response; 512 is too low and yields empty response (done_reason: length).
    let system_max_tokens = std::env::var("AKASHA_SYSTEM_TASK_MAX_TOKENS")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(4096);
    let request = CompletionRequest {
        prompt,
        max_tokens: Some(system_max_tokens),
        temperature: Some(0.2),
        preferred_task_type: Some("system".to_string()),
        image_data_urls: None,
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
                    let agent_type = if agent_type == "code" || agent_type == "search" || agent_type == "schedule" {
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
        while let Some(task) = rx.recv().await {
            let bus = self.bus.clone();
            let store_path = self.store_path.clone();
            let conv_tx = self.conversation_tx.clone();
            let progress = self.progress.clone();
            let llm_router = self.llm_router.clone();
            let root_task_id = task.task_id;
            let message = task.message;
            let session_id = task.session_id;
            let image_data_urls = task.image_data_urls;
            tokio::spawn(async move {
                if let Err(e) = process_root_task(
                    bus,
                    store_path.as_path(),
                    root_task_id,
                    message,
                    session_id,
                    image_data_urls,
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
    image_data_urls: Option<Vec<String>>,
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
    // Decomposition is fully driven by the LLM; no keyword-based override.
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

    // Single subtask (schedule): create recurring task in the app, no delegation to code agent.
    if steps.len() == 1 && steps[0].0 == "schedule" {
        let payload = steps[0].1.as_str();
        let parts: Vec<&str> = payload.splitn(3, '|').map(str::trim).collect();
        let (interval_secs, name, reminder_message) = if parts.len() >= 3 {
            let interval_secs = parts[0].parse::<u64>().unwrap_or(7200);
            let name = parts[1].to_string();
            let reminder_message = parts[2].to_string();
            (interval_secs, name, reminder_message)
        } else if parts.len() == 2 {
            let interval_secs = parts[0].parse::<u64>().unwrap_or(7200);
            (interval_secs, "Rappel".to_string(), parts[1].to_string())
        } else {
            (7200, "Rappel".to_string(), payload.to_string())
        };
        let schedule_store = match ScheduleStore::open(store_path) {
            Ok(s) => s,
            Err(e) => {
                let _ = bus.send(
                    EventEnvelope::new(
                        EventType::ProgressUpdate,
                        Some(serde_json::json!({
                            "task_id": root_task_id.to_string(),
                            "progress_pct": 100,
                            "message": format!("Erreur création récurrence : {}", e)
                        })),
                    )
                    .with_correlation(root_task_id),
                );
                let _ = store.update_status(root_task_id, TaskStatus::Failed);
                let _ = bus.send(
                    EventEnvelope::new(EventType::TaskFailed, Some(serde_json::json!({ "task_id": root_task_id.to_string() })))
                        .with_correlation(root_task_id),
                );
                return Ok(());
            }
        };
        let now = Utc::now();
        let schedule = Schedule {
            id: Uuid::new_v4(),
            name: name.clone(),
            description: format!("Rappel toutes les {} secondes", interval_secs),
            enabled: true,
            timezone: "UTC".to_string(),
            rrule: String::new(),
            interval_seconds: Some(interval_secs),
            start_at: now,
            end_at: None,
            channel_context: Some(reminder_message.clone()),
            created_at: now,
            updated_at: now,
        };
        if let Err(e) = schedule_store.insert_schedule(&schedule) {
            let _ = bus.send(
                EventEnvelope::new(
                    EventType::ProgressUpdate,
                    Some(serde_json::json!({
                        "task_id": root_task_id.to_string(),
                        "progress_pct": 100,
                        "message": format!("Erreur création récurrence : {}", e)
                    })),
                )
                .with_correlation(root_task_id),
            );
            let _ = store.update_status(root_task_id, TaskStatus::Failed);
            let _ = bus.send(
                EventEnvelope::new(EventType::TaskFailed, Some(serde_json::json!({ "task_id": root_task_id.to_string() })))
                    .with_correlation(root_task_id),
            );
            return Ok(());
        }
        let _ = bus.send(
            EventEnvelope::new(
                EventType::ScheduleCreated,
                Some(serde_json::json!({
                    "schedule_id": schedule.id.to_string(),
                    "name": schedule.name,
                    "interval_seconds": interval_secs
                })),
            )
            .with_correlation(root_task_id),
        );
        let interval_desc = if interval_secs >= 86400 {
            format!("tous les {} jours", interval_secs / 86400)
        } else if interval_secs >= 3600 {
            format!("toutes les {} heures", interval_secs / 3600)
        } else if interval_secs >= 60 {
            format!("toutes les {} minutes", interval_secs / 60)
        } else {
            format!("toutes les {} secondes", interval_secs)
        };
        let success_msg = format!(
            "Récurrence créée : « {} ». {} — Tu peux la voir dans l'onglet Calendrier.",
            name, interval_desc
        );
        let _ = bus.send(
            EventEnvelope::new(
                EventType::ProgressUpdate,
                Some(serde_json::json!({
                    "task_id": root_task_id.to_string(),
                    "progress_pct": 100,
                    "message": success_msg
                })),
            )
            .with_correlation(root_task_id),
        );
        let _ = store.update_status(root_task_id, TaskStatus::Completed);
        let _ = bus.send(
            EventEnvelope::new(
                EventType::TaskCompleted,
                Some(serde_json::json!({
                    "task_id": root_task_id.to_string(),
                    "status": "completed"
                })),
            )
            .with_correlation(root_task_id),
        );
        return Ok(());
    }

    // Single subtask (conversation): delegate to conversation worker for root (user sees reply on root_id).
    if steps.len() == 1 && steps[0].0 == "conversation" {
        conversation_tx
            .send(OrchestratorTask {
                task_id: root_task_id,
                message: steps[0].1.clone(),
                session_id,
                image_data_urls,
            })
            .await
            .map_err(|_| anyhow::anyhow!("conversation channel closed"))?;
        return Ok(());
    }

    // Multiple subtasks: create child tasks and delegate each; aggregator below collects all replies into one response.
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
        // Delegate to conversation worker for all types (code/search handled as conversation for now). Pass same session_id for memory. No image attachments for sub-steps.
        let _ = conversation_tx
            .send(OrchestratorTask {
                task_id: child_id,
                message: sub_message.clone(),
                session_id: session_id.clone(),
                image_data_urls: None,
            })
            .await;
    }
    // Aggregator: when all children are done, collect their replies then ask the conversation LLM to synthesize one structured answer.
    let store_path_buf = store_path.to_path_buf();
    let steps_count = steps.len();
    let user_message = message.clone();
    const GENERIC_MESSAGES: &[&str] = &["Done.", "Terminé.", "Échec.", "Annulé."];
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
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                let mut parts: Vec<String> = Vec::new();
                {
                    let g = progress.read().await;
                    for child in &children {
                        let content = g.get(&child.id).and_then(|q| q.back().map(|e| e.message.trim().to_string()));
                        let content = match content {
                            Some(ref s) if !s.is_empty() && !GENERIC_MESSAGES.contains(&s.as_str()) => s.clone(),
                            _ => {
                                if child.status == TaskStatus::Failed {
                                    "(Sous-tâche en échec)".to_string()
                                } else {
                                    "(Aucune réponse)".to_string()
                                }
                            }
                        };
                        parts.push(format!("[Agent {}]\n{}", child.assigned_agent, content));
                    }
                }
                if parts.len() < children.len() {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    let g = progress.read().await;
                    parts.clear();
                    for child in &children {
                        let content = g.get(&child.id).and_then(|q| q.back().map(|e| e.message.trim().to_string()));
                        let content = match content {
                            Some(ref s) if !s.is_empty() && !GENERIC_MESSAGES.contains(&s.as_str()) => s.clone(),
                            _ => {
                                if child.status == TaskStatus::Failed {
                                    "(Sous-tâche en échec)".to_string()
                                } else {
                                    "(Aucune réponse)".to_string()
                                }
                            }
                        };
                        parts.push(format!("[Agent {}]\n{}", child.assigned_agent, content));
                    }
                }
                let raw_responses = parts.join("\n\n");
                let aggregated = if raw_responses.is_empty() || raw_responses.trim() == "(Aucune réponse)" {
                    "Aucune réponse des sous-agents.".to_string()
                } else {
                    // Ask the conversation LLM to synthesize all sub-agent replies into one answer that directly addresses the user's question.
                    let synthesis_prompt = format!(
                        r#"Tu es un synthétiseur. La question de l'utilisateur est :

« {} »

Voici les réponses de différents agents spécialisés :

{}

Produis une seule réponse structurée et claire qui répond exactement à la question de l'utilisateur. Intègre les éléments utiles des réponses ci-dessus sans les lister ni citer les agents ; reformule de façon naturelle et directe pour l'utilisateur.
N'ajoute aucune information qui ne figure pas dans les réponses des agents ci-dessus. Si les réponses ne permettent pas de répondre à la question, dis simplement que tu n'as pas trouvé d'information."#,
                        user_message.trim(),
                        raw_responses
                    );
                    let req = CompletionRequest {
                        prompt: synthesis_prompt,
                        max_tokens: Some(4096),
                        temperature: Some(0.3),
                        preferred_task_type: Some("conversation".to_string()),
                        image_data_urls: None,
                    };
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(120),
                        llm_router.complete(&req),
                    )
                    .await
                    {
                        Ok(Ok(resp)) if !resp.text.trim().is_empty() => resp.text.trim().to_string(),
                        _ => {
                            // Fallback: show joined responses if synthesis fails or times out
                            if parts.len() == 1 {
                                parts.into_iter().next().unwrap_or_else(|| raw_responses)
                            } else {
                                format!("Réponses des sous-agents :\n\n{}", raw_responses)
                            }
                        }
                    }
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
