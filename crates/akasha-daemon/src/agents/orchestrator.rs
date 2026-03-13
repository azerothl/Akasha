//! Orchestrator — single entry point: receive (task_id, message), decompose (LLM), delegate to workers, aggregate (Phase E).
//! Includes satisfaction check: only complete when the aggregated response is satisfactory or agents clearly could not perform the task; otherwise one refinement round.

use akasha_core::{EventEnvelope, EventType};
use akasha_llm::CompletionRequest;
use akasha_store::{Schedule, ScheduleStore, Task, TaskStatus, TaskStore};
use chrono::Utc;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::mpsc;
use uuid::Uuid;

use super::{EventBus, OrchestratorTask};
use crate::api::{message_suggests_tool_only_action, ProgressCache, TaskCompletionRegistry};

/// Outcome of evaluating whether the aggregated sub-agent response satisfies the user request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SatisfactionOutcome {
    /// Response fully or substantially answers the request → complete task.
    Satisfactory,
    /// Agents clearly stated they could not perform the task → complete with that answer.
    CannotDo,
    /// Response is incomplete or does not directly address the request → run one refinement round.
    NeedsRefinement,
}

/// One subtask from decomposition: (agent_type, message for that agent).
pub type Subtask = (String, String);

/// Applies the tool-only override: if the decomposer returned a single "code" step but the
/// step message suggests a tool-only action (camera, web search, save file, image gen),
/// re-route to conversation so the agent uses TOOL: instead of generating a script.
/// Exposed for unit tests.
pub(crate) fn apply_decomposition_override(_message: &str, steps: Vec<Subtask>) -> Vec<Subtask> {
    if steps.len() == 1
        && steps[0].0 == "code"
        && message_suggests_tool_only_action(&steps[0].1)
    {
        vec![("conversation".to_string(), steps[0].1.clone())]
    } else {
        steps
    }
}

const DECOMPOSER_PROMPT_TEMPLATE: &str = r#"You are a task decomposer. Output one line per subtask: agent_type|message. One line per distinct user action (e.g. one for generating a report, another for creating a file).
Agent types: conversation (general chat), code (code gen), search (info search), schedule (create recurring task IN THE APP), financial (budget, costs, reports), documentalist (answer from user's document base / RAG), project_manager (project tracking, milestones, planning), technical_writer (technical docs, procedures, tutorials), research (deep research, multi-source synthesis), security_audit (security review of code/config), creative (copywriting, marketing content).
- Use **conversation** when the user asks to *perform* an action using existing tools: take a photo (camera/webcam), web search, save a file, generate an image (AI), run a command. The conversation agent will use tools (device_invoke, web_search, write_file, generate_image, run_command); do NOT choose "code" for these.
- Reserve **code** only for *explicit* requests to write or generate code/script (e.g. "écris un script qui…", "génère du code pour…").
- If the user asks to CREATE a recurring/scheduled task (e.g. "tâche récurrente", "rappel toutes les 2 heures", "crée un rappel"), output exactly ONE line: schedule|interval_seconds|name|message
  where interval_seconds is in seconds (3600=1h, 7200=2h, 86400=1 day), name is a short title, message is the reminder text shown when the task runs. Example: schedule|7200|Rappel Github|Rappel: regarder l'avancement du projet sur GitHub
- If the user asks for several distinct deliverables or actions (e.g. "make a report and then create a file", "do X then do Y"), output one line per deliverable/action. Example: first line for the report, second line for creating the file.
- Otherwise output agent_type|message. Examples: "Prends une photo avec la caméra" → conversation|Prends une photo avec la caméra. "Écris un script Python qui lit un fichier" → code|Écris un script Python qui lit un fichier.

User request:

"#;

/// Builds the decomposer prompt string (template + message). Used by benchmarks and by decompose_request.
pub fn build_decomposer_prompt(message: &str) -> String {
    format!("{}{}", DECOMPOSER_PROMPT_TEMPLATE, message)
}

/// Decompose a user request into one or more subtasks via LLM. Falls back to single "conversation" on error, timeout or empty.
async fn decompose_request(
    llm_router: &Arc<akasha_llm::LLMRouter>,
    message: &str,
) -> Vec<Subtask> {
    let prompt = build_decomposer_prompt(message);
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
                    const RECOGNIZED: &[&str] = &[
                        "code", "search", "schedule", "financial", "documentalist", "project_manager",
                        "technical_writer", "research", "security_audit", "creative",
                    ];
                    let agent_type = if RECOGNIZED.contains(&agent_type.as_str()) {
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

/// Asks the LLM whether the aggregated response satisfies the user request or agents clearly could not do the task.
/// On timeout or parse error, returns Satisfactory (current behaviour: complete as-is).
async fn check_satisfaction(
    llm_router: &Arc<akasha_llm::LLMRouter>,
    user_request: &str,
    aggregated_response: &str,
) -> SatisfactionOutcome {
    let prompt = format!(
        r#"You are an evaluator. Given the user's request and the combined response from sub-agents, output exactly one word:

SATISFACTORY — the response fully or substantially answers the user's request.
CANNOT_DO — the agents clearly stated they could not perform the task, lack information, or do not have the necessary tools.
NEEDS_REFINEMENT — the response is incomplete, vague, off-topic, or does not directly address the request (e.g. only a promise to do something, or partial information).

User request: "{}"

Combined response: "{}"

Output only: SATISFACTORY, CANNOT_DO, or NEEDS_REFINEMENT"#,
        user_request.trim().chars().take(500).collect::<String>(),
        aggregated_response.trim().chars().take(2000).collect::<String>()
    );
    let req = CompletionRequest {
        prompt,
        max_tokens: Some(32),
        temperature: Some(0.0),
        preferred_task_type: Some("system".to_string()),
        image_data_urls: None,
    };
    match tokio::time::timeout(
        std::time::Duration::from_secs(30),
        llm_router.complete(&req),
    )
    .await
    {
        Ok(Ok(resp)) => {
            let t = resp.text.to_uppercase();
            if t.contains("NEEDS_REFINEMENT") {
                SatisfactionOutcome::NeedsRefinement
            } else if t.contains("CANNOT_DO") {
                SatisfactionOutcome::CannotDo
            } else {
                SatisfactionOutcome::Satisfactory
            }
        }
        _ => SatisfactionOutcome::Satisfactory,
    }
}

pub struct Orchestrator {
    bus: EventBus,
    store_path: std::path::PathBuf,
    conversation_tx: mpsc::Sender<OrchestratorTask>,
    progress: ProgressCache,
    llm_router: Arc<akasha_llm::LLMRouter>,
    task_completion: TaskCompletionRegistry,
}

impl Orchestrator {
    pub fn new(
        bus: EventBus,
        store_path: std::path::PathBuf,
        conversation_tx: mpsc::Sender<OrchestratorTask>,
        progress: ProgressCache,
        llm_router: Arc<akasha_llm::LLMRouter>,
        task_completion: TaskCompletionRegistry,
    ) -> Self {
        Self {
            bus,
            store_path,
            conversation_tx,
            progress,
            llm_router,
            task_completion,
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
            let task_completion = self.task_completion.clone();
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
                    task_completion,
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
    task_completion: TaskCompletionRegistry,
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

    let mut steps = decompose_request(&llm_router, &message).await;
    steps = apply_decomposition_override(&message, steps);
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
    let mut child_notifies: Vec<(Uuid, Arc<tokio::sync::Notify>)> = Vec::new();
    for (agent_type, sub_message) in &steps {
        let child_id = Uuid::new_v4();
        let task = Task {
            id: child_id,
            parent_task_id: Some(root_task_id),
            status: TaskStatus::Pending,
            assigned_agent: agent_type.clone(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            initial_message: {
                const MAX: usize = 500;
                if sub_message.chars().count() > MAX {
                    Some(sub_message.chars().take(MAX).chain(std::iter::once('…')).collect::<String>())
                } else if sub_message.is_empty() {
                    None
                } else {
                    Some(sub_message.clone())
                }
            },
        };
        store.insert(&task)?;
        let _ = bus.send(
            EventEnvelope::new(
                EventType::SubAgentSpawned,
                Some(serde_json::json!({
                    "task_id": child_id.to_string(),
                    "parent_id": root_task_id.to_string(),
                    "agent": agent_type,
                    "delegation_reason": serde_json::Value::Null
                })),
            )
            .with_correlation(root_task_id),
        );
        // Register notifier *before* sending to conv_tx so the worker can fire it immediately.
        let notify = Arc::new(tokio::sync::Notify::new());
        task_completion.write().await.insert(child_id, notify.clone());
        // Delegate to conversation worker for all types (code/search handled as conversation for now). Pass same session_id for memory. No image attachments for sub-steps.
        let _ = conversation_tx
            .send(OrchestratorTask {
                task_id: child_id,
                message: sub_message.clone(),
                session_id: session_id.clone(),
                image_data_urls: None,
            })
            .await;
        child_notifies.push((child_id, notify));
    }
    // Aggregator: wait for all child completion notifications (no polling), then synthesize.
    let store_path_buf = store_path.to_path_buf();
    let steps_count = steps.len();
    let user_message = message.clone();
    let conversation_tx_aggregator = conversation_tx.clone();
    let session_id_aggregator = session_id.clone();
    const GENERIC_MESSAGES: &[&str] = &["Done.", "Terminé.", "Échec.", "Annulé."];
    // Per-child timeout: mirrors the delegation handler's 5-minute limit.  Children are processed
    // sequentially by the conversation worker, so total wait is bounded by N × PER_CHILD_TIMEOUT.
    const PER_CHILD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
    tokio::spawn(async move {
        // Wait for each child in turn; tokio::Notify buffers one pending notification so children
        // that finish early are captured immediately when their slot comes.
        for (child_id, notify) in &child_notifies {
            if tokio::time::timeout(PER_CHILD_TIMEOUT, notify.notified()).await.is_err() {
                tracing::warn!(child_id = %child_id, parent_id = %root_task_id, "Aggregator: child task timed out");
            }
            task_completion.write().await.remove(child_id);
        }
        // Ensure any remaining registry entries are cleaned up (timeout path).
        {
            let mut reg = task_completion.write().await;
            for (child_id, _) in &child_notifies {
                reg.remove(child_id);
            }
        }
        // Single store read to get final child statuses.
        let store = match TaskStore::open(&store_path_buf) {
            Ok(s) => s,
            Err(_) => return,
        };
        let children = match store.get_children(root_task_id) {
            Ok(c) => c,
            Err(_) => return,
        };
        if children.is_empty() {
            return;
        }
        let all_done = children.iter().all(|t| matches!(t.status, TaskStatus::Completed | TaskStatus::Failed));
        if !all_done {
            // Some children timed out without completing; treat the root task as failed.
            let _ = store.update_status(root_task_id, TaskStatus::Failed);
            let _ = bus.send(
                EventEnvelope::new(
                    EventType::TaskFailed,
                    Some(serde_json::json!({
                        "task_id": root_task_id.to_string(),
                        "status": "failed",
                        "subtasks": steps_count
                    })),
                )
                .with_correlation(root_task_id),
            );
            return;
        }
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
        // Satisfaction check: only complete when response is satisfactory or agents clearly could not do the task.
        let mut final_aggregated = aggregated.clone();
        let satisfaction = check_satisfaction(&llm_router, &user_message, &aggregated).await;
        if satisfaction == SatisfactionOutcome::NeedsRefinement {
            // One refinement round: ask a single conversation agent to produce a complete answer or clearly state what is missing.
            let refinement_prompt = format!(
                r#"Demande initiale de l'utilisateur : « {} »

Réponses actuelles des sous-agents (incomplètes ou insuffisantes) :

{}

Tu dois soit : (1) produire une réponse complète et directe à la demande de l'utilisateur en t'appuyant sur les éléments ci-dessus, soit (2) indiquer clairement que tu ne peux pas réaliser la tâche et expliquer pourquoi (information manquante, outil indisponible, etc.). Ne te contente pas de promettre de faire quelque chose — réponds ou dis clairement que tu ne peux pas."#,
                user_message.trim(),
                aggregated.trim()
            );
            let refinement_child_id = Uuid::new_v4();
            let refinement_task = Task {
                id: refinement_child_id,
                parent_task_id: Some(root_task_id),
                status: TaskStatus::Pending,
                assigned_agent: "conversation".to_string(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
                initial_message: Some(
                    refinement_prompt
                        .chars()
                        .take(500)
                        .chain(std::iter::once('…'))
                        .collect::<String>(),
                ),
            };
            if store.insert(&refinement_task).is_ok() {
                let notify_refinement = Arc::new(tokio::sync::Notify::new());
                task_completion.write().await.insert(refinement_child_id, notify_refinement.clone());
                let _ = conversation_tx_aggregator
                    .send(OrchestratorTask {
                        task_id: refinement_child_id,
                        message: refinement_prompt,
                        session_id: session_id_aggregator.clone(),
                        image_data_urls: None,
                    })
                    .await;
                if tokio::time::timeout(PER_CHILD_TIMEOUT, notify_refinement.notified())
                    .await
                    .is_ok()
                {
                    task_completion.write().await.remove(&refinement_child_id);
                    let refinement_content = {
                        let g = progress.read().await;
                        g.get(&refinement_child_id)
                            .and_then(|q| q.back().map(|e| e.message.trim().to_string()))
                            .filter(|s| !s.is_empty() && !GENERIC_MESSAGES.contains(&s.as_str()))
                    };
                    if let Some(content) = refinement_content {
                        final_aggregated = content;
                    }
                }
                let _ = store.update_status(refinement_child_id, TaskStatus::Completed);
            }
        }
        let _ = bus.send(
            EventEnvelope::new(
                EventType::ProgressUpdate,
                Some(serde_json::json!({
                    "task_id": root_task_id.to_string(),
                    "progress_pct": 100,
                    "message": final_aggregated
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
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{apply_decomposition_override, Subtask};

    fn step(agent: &str, msg: &str) -> Subtask {
        (agent.to_string(), msg.to_string())
    }

    #[test]
    fn override_single_code_step_tool_only_to_conversation() {
        let steps = vec![step("code", "Prends une photo")];
        let out = apply_decomposition_override("Prends une photo", steps);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, "conversation");
        assert_eq!(out[0].1, "Prends une photo");
    }

    #[test]
    fn override_keeps_code_step_when_explicit_code_request() {
        let steps = vec![step("code", "Écris un script Python")];
        let out = apply_decomposition_override("Écris un script Python", steps);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, "code");
        assert_eq!(out[0].1, "Écris un script Python");
    }

    #[test]
    fn override_leaves_multiple_steps_unchanged() {
        let steps = vec![step("code", "foo"), step("search", "bar")];
        let out = apply_decomposition_override("do both", steps);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].0, "code");
        assert_eq!(out[1].0, "search");
    }

    #[test]
    fn override_leaves_single_search_step_unchanged() {
        let steps = vec![step("search", "Quelle météo ?")];
        let out = apply_decomposition_override("Quelle météo ?", steps);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, "search");
    }
}
