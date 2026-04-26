//! Main Agent — entry point, ack < 500ms, task creation, routing to Orchestrator or Direct to conversation (Plan: Architecture agents et pipeline).

use akasha_core::{EventEnvelope, EventType};
use akasha_llm::CompletionRequest;
use akasha_store::{Task, TaskStatus, TaskStore, TodoStatus};
use crate::agents::{classify_execution_mode, EventBus, ExecutionMode};
use crate::studio::StudioDiskRootRegistry;
use crate::latency::{emit_timeline_for_task, env_duration_ms};
use chrono::Utc;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use uuid::Uuid;

fn is_session_recall_message(message: &str) -> bool {
    let lower = message
        .trim()
        .to_lowercase()
        .replace('’', "'")
        .replace('\'', "'")
        .replace(['!', '?', '.', ',', ';', ':'], " ");
    if lower.is_empty() {
        return false;
    }
    [
        "rappeler",
        "rappelle",
        "rappel",
        "ce qu'on a fait",
        "ce qu on a fait",
        "on a fait",
        "what we did",
        "what we've done",
        "what we have done",
        "what did we do",
        "remind me",
        "recap",
        "recap what we did",
    ]
    .iter()
    .any(|k| lower.contains(k))
}

fn is_geolocation_request(message: &str) -> bool {
    let lower = message.trim().to_lowercase();
    let lower = lower
        .replace('\u{2019}', "'")
        .replace(['!', '?', '.', ',', ';', ':'], " ");
    if lower.is_empty() {
        return false;
    }
    [
        "distance",
        "distance entre",
        "combien de km",
        "how far",
        "how many km",
        "distance from",
        "distance to",
        "itineraire",
        "itinéraire",
        "route",
        "trajet",
        "chemin",
        "direction",
        "directions",
        "navigate",
        "geolocation",
        "géolocalisation",
        "localisation",
        "location",
        "coordinate",
        "coordonnées",
        "map",
        "carte",
        "gps",
        "position",
    ]
    .iter()
    .any(|k| lower.contains(k))
}

/// Fast-path detect: message is a weather / news / external-facts query.
/// Uses a focused subset of the keywords from `compute_message_intent_flags::external_info`,
/// targeting clear weather and news-related terms to avoid common false positives.
fn is_external_info_request(message: &str) -> bool {
    let lower = message
        .trim()
        .to_lowercase()
        .replace('\u{2019}', "'")
        .replace(['!', '?', '.', ',', ';', ':'], " ");
    if lower.is_empty() {
        return false;
    }
    [
        "météo",
        "meteo",
        "weather",
        "prévisions météo",
        "previsions meteo",
        "quel temps",
        "il fait quel temps",
        "il va faire",
        "forecast",
        "actualités",
        "actualites",
        "les news",
        "l'actu",
    ]
    .iter()
    .any(|k| lower.contains(k))
}

/// Strip `<think>…</think>` blocks emitted by reasoning models (e.g. Qwen3 via Ollama) before
/// the actual model output. Returns the text after the last `</think>` tag, or the original
/// text unchanged when no such tag is present.
fn strip_thinking_tokens(text: &str) -> &str {
    if let Some(pos) = text.rfind("</think>") {
        text[pos + "</think>".len()..].trim_start()
    } else {
        text
    }
}

/// Specialist agent names that can be forwarded as `assigned_agent` for direct-mode tasks.
/// These correspond to the non-None branches of `agent_role_system_prompt` (api.rs).
/// "conversation" is intentionally absent — it is the default fallback.
const SPECIALIST_AGENTS: &[&str] = &[
    "search",
    "code",
    "creative",
    "research",
    "system",
    "financial",
    "documentalist",
    "project_manager",
    "technical_writer",
    "security_audit",
    "analyst",
    "architect",
    "frontend",
    "backend",
    "database",
    "integration",
    "qa",
    "image_generation",
    "studio_scaffold",
    "studio_frontend",
    "studio_backend",
    "studio_fullstack",
    "studio_planner",
    "studio_project_manager",
];

/// True when `name` matches a routed specialist agent (case-insensitive).
pub fn is_specialist_agent(name: &str) -> bool {
    let n = name.trim();
    SPECIALIST_AGENTS.iter().any(|a| a.eq_ignore_ascii_case(n))
}
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

#[derive(Debug, Clone)]
struct SelectorRunResult {
    decision: Option<SelectorDecision>,
    enabled: bool,
    timed_out: bool,
    elapsed_ms: u64,
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
    /// Code Studio: maps root task id → sandboxed project disk root for tool execution.
    studio_disk_registry: StudioDiskRootRegistry,
}

impl MainAgent {
    pub fn new(
        bus: EventBus,
        orchestrator: OrchestratorSender,
        llm_router: Arc<akasha_llm::LLMRouter>,
        studio_disk_registry: StudioDiskRootRegistry,
    ) -> Self {
        Self {
            bus,
            orchestrator,
            llm_router,
            direct_conversation_tx: None,
            studio_disk_registry,
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
    async fn system_selector_decision(&self, message: &str) -> SelectorRunResult {
        let enabled = std::env::var("AKASHA_SYSTEM_TASK_SELECTOR")
            .ok()
            .map(|s| s != "0" && !s.eq_ignore_ascii_case("false"))
            .unwrap_or(true);
        if !enabled {
            return SelectorRunResult {
                decision: None,
                enabled: false,
                timed_out: false,
                elapsed_ms: 0,
            };
        }

        // Fast-path for geolocation/distance questions to avoid expensive selector round-trips.
        if is_geolocation_request(message) {
            tracing::debug!("selector: geolocation request detected; using direct conversation fast-path");
            return SelectorRunResult {
                decision: Some(SelectorDecision {
                    answer_mode: SelectorAnswerMode::Direct,
                    task_type: Some("conversation".to_string()),
                    target_agent: None,
                    reason: Some(
                        "geolocation/distance/routing request detected; prefer tools matched by dynamic plugin routing rules"
                            .to_string(),
                    ),
                }),
                enabled: true,
                timed_out: false,
                elapsed_ms: 0,
            };
        }

        // Fast-path for weather / news / external-facts queries.
        // These always require web_search and map cleanly to the "search" agent role.
        // Bypassing the LLM selector removes 5-90 s cold-start delays for local Ollama models.
        if is_external_info_request(message) {
            tracing::debug!("selector: external-info request detected; routing direct to search agent");
            return SelectorRunResult {
                decision: Some(SelectorDecision {
                    answer_mode: SelectorAnswerMode::Direct,
                    task_type: Some("conversation".to_string()),
                    target_agent: Some("search".to_string()),
                    reason: Some(
                        "weather/news/external-info request detected; route to search agent with web_search"
                            .to_string(),
                    ),
                }),
                enabled: true,
                timed_out: false,
                elapsed_ms: 0,
            };
        }

        let started = Instant::now();
        let message_capped = crate::llm_prompt_cap::truncate_utf8_bytes(
            message,
            crate::llm_prompt_cap::SELECTOR_USER_MESSAGE_MAX_BYTES,
        );
        let prompt = format!(
            "You are a strict routing selector for Akasha.\n\
Return ONLY compact JSON with schema:\n\
{{\"answer_mode\":\"direct|delegate\",\"task_type\":\"snake_case or empty\",\"target_agent\":\"optional\",\"reason\":\"short\"}}\n\
Rules:\n\
- answer_mode=delegate for multi-step/project/planning/complex implementation requests.\n\
- task_type must be one of: conversation, code_generation, creative_writing, scientific_analysis, data_analysis, system_diagnostic, system, orchestrator, image_generation.\n\
- If unsure, use answer_mode=delegate and task_type=conversation.\n\
User message:\n{}",
            message_capped
        );
        let req = CompletionRequest {
            prompt,
            max_tokens: Some(128),
            temperature: Some(0.0),
            preferred_task_type: Some("system".to_string()),
            system_prompt: None,
            image_data_urls: None,
            top_p: None,
            top_k: None,
            frequency_penalty: None,
            presence_penalty: None,
            repeat_penalty: None,
            num_ctx: None,
            num_gpu: None,
            thinking_level: None,
        };
        let timeout = env_duration_ms("AKASHA_SELECTOR_TIMEOUT_MS", 2_000);
        // Ollama needs extra time to load the model on first call (cold start can take 30-90s).
        // Use is_ollama_registered() rather than is_ollama_primary("system") because users
        // typically configure Ollama for conversation/orchestrator but not the "system" task type.
        // If Ollama is registered at all, assume it may be called and use the extended budget.
        let timeout = if self.llm_router.is_ollama_registered() {
            let ollama_timeout = env_duration_ms("AKASHA_SELECTOR_TIMEOUT_MS_OLLAMA", 90_000);
            tracing::debug!(
                timeout_ms = ollama_timeout.as_millis() as u64,
                "selector: Ollama registered, using extended timeout"
            );
            ollama_timeout
        } else {
            timeout
        };
        match tokio::time::timeout(timeout, self.llm_router.complete(&req)).await {
            Ok(Ok(resp)) => SelectorRunResult {
                decision: parse_selector_decision(strip_thinking_tokens(resp.text.trim())),
                enabled: true,
                timed_out: false,
                elapsed_ms: started.elapsed().as_millis() as u64,
            },
            Ok(Err(err)) => {
                tracing::warn!(error = %err, "selector completion failed; using immediate fallback");
                SelectorRunResult {
                    decision: None,
                    enabled: true,
                    timed_out: false,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                }
            }
            Err(_) => {
                tracing::info!(timeout_ms = timeout.as_millis() as u64, "selector timed out; using immediate fallback");
                SelectorRunResult {
                    decision: None,
                    enabled: true,
                    timed_out: true,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                }
            }
        }
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
        studio_disk_root: Option<std::path::PathBuf>,
        studio_forced_agent: Option<String>,
        studio_evolution_branch: Option<String>,
    ) -> anyhow::Result<Uuid> {
        let session_id = if session_id.is_empty() { "default" } else { session_id };
        let task_id = Uuid::new_v4();

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

        // Determine a preliminary assigned_agent (without LLM) so the task can be persisted
        // immediately, satisfying the "ack < 500ms" guarantee and ensuring the task_id is in
        // the DB before any events that carry it as a correlation ID.
        let execution_mode = if forward_to_orchestrator {
            Some(classify_execution_mode(message))
        } else {
            None
        };
        let preliminary_agent: &str = if let Some(ref a) = studio_forced_agent {
            let normalized = a.trim();
            if let Some(canonical) = SPECIALIST_AGENTS.iter().find(|s| s.eq_ignore_ascii_case(normalized)) {
                canonical
            } else if !forward_to_orchestrator {
                "llm"
            } else {
                "orchestrator"
            }
        } else if !forward_to_orchestrator {
            "llm"
        } else {
            "orchestrator"
        };

        // Insert the task into the store before any network/LLM call so events can be
        // correlated against a task that actually exists. The TaskStore (non-Send SQLite
        // connection) must be dropped before the .await below (system_selector_decision).
        {
            let store = TaskStore::open(store_path)?;
            let task = Task {
                id: task_id,
                parent_task_id: None,
                status: TaskStatus::Pending,
                assigned_agent: preliminary_agent.to_string(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
                initial_message,
            };
            store.insert(&task)?;
        }

        if let Some(p) = studio_disk_root.clone() {
            crate::studio::register_studio_root(&self.studio_disk_registry, task_id, p).await;
        }

        let mut message_for_llm = message.to_string();
        // Keep the unmodified user message for routing decisions (recall detection, selector).
        // The studio prefix is only for the LLM prompt — routing logic should see the original intent.
        let original_user_message = message.to_string();
        if let Some(ref b) = studio_evolution_branch {
            message_for_llm = format!(
                "[Studio: apply changes on git branch `{b}`]\n\n{}",
                message_for_llm
            );
        }

        // Use task_id as correlation so GET /api/tasks/{task_id}/events returns these events.
        let _ = self.bus.send(EventEnvelope::new(EventType::UserRequestReceived, Some(serde_json::json!({ "message": message }))).with_correlation(task_id));
        emit_timeline_for_task(&self.bus, Some(store_path), task_id, "request_received", None);
        let _ = self.bus.send(
            EventEnvelope::new(
                EventType::AcknowledgmentSent,
                Some(serde_json::json!({ "task_id": task_id.to_string() })),
            )
            .with_correlation(task_id),
        );

        // Selector + dispatch run in a background task so the HTTP response (task_id ack) can
        // be returned immediately, well within Tauri / HTTP client timeouts.
        // With Ollama the selector alone can take 30-90 s for model cold-start; blocking the
        // HTTP handler for that long causes "operation timed out" errors in the frontend even
        // though the task actually completes correctly in the background.
        let agent_clone = self.clone();
        let studio_forced_spawn = studio_forced_agent.clone();
        let message_owned = message_for_llm;
        let original_user_message_owned = original_user_message;
        let session_id_owned = session_id.to_string();
        let store_path_buf = store_path.to_path_buf();
        let preliminary_agent_str = preliminary_agent.to_string();
        tokio::spawn(async move {
            let store_path = store_path_buf.as_path();
            let message = message_owned.as_str();
            let original_message = original_user_message_owned.as_str();
            let session_id = session_id_owned.as_str();
            let preliminary_agent = preliminary_agent_str.as_str();

            // Run the LLM selector after the task is safely persisted.
            // Use the original (unprefixed) message so studio prefixes don't break recall detection.
            let skip_selector_for_recall = forward_to_orchestrator && is_session_recall_message(original_message);
            let selector_result = if let Some(agent) = studio_forced_spawn
                .as_ref()
                .filter(|a| is_specialist_agent(a.as_str()))
                .cloned()
            {
                if forward_to_orchestrator && !skip_selector_for_recall {
                    emit_timeline_for_task(&agent_clone.bus, Some(store_path), task_id, "selector_start", None);
                    emit_timeline_for_task(
                        &agent_clone.bus,
                        Some(store_path),
                        task_id,
                        "selector_end",
                        Some(serde_json::json!({
                            "duration_ms": 0u64,
                            "selector_enabled": true,
                            "timed_out": false,
                            "decision_found": true,
                            "studio_forced_agent": agent,
                        })),
                    );
                }
                SelectorRunResult {
                    decision: Some(SelectorDecision {
                        answer_mode: SelectorAnswerMode::Direct,
                        task_type: Some("code_generation".to_string()),
                        target_agent: Some(agent),
                        reason: Some("studio_assigned_agent".to_string()),
                    }),
                    enabled: true,
                    timed_out: false,
                    elapsed_ms: 0,
                }
            } else if forward_to_orchestrator && !skip_selector_for_recall {
                emit_timeline_for_task(&agent_clone.bus, Some(store_path), task_id, "selector_start", None);
                agent_clone.system_selector_decision(message).await
            } else {
                SelectorRunResult {
                    decision: None,
                    enabled: false,
                    timed_out: false,
                    elapsed_ms: 0,
                }
            };
            if forward_to_orchestrator && !skip_selector_for_recall {
                emit_timeline_for_task(
                    &agent_clone.bus,
                    Some(store_path),
                    task_id,
                    "selector_end",
                    Some(serde_json::json!({
                        "duration_ms": selector_result.elapsed_ms,
                        "selector_enabled": selector_result.enabled,
                        "timed_out": selector_result.timed_out,
                        "decision_found": selector_result.decision.is_some(),
                    })),
                );
                tracing::info!(
                    task_id = %task_id,
                    selector_ms = selector_result.elapsed_ms,
                    selector_timed_out = selector_result.timed_out,
                    selector_used = selector_result.decision.is_some(),
                    "selector finished"
                );
            }

            let selector_decision = selector_result.decision;

            let selector_direct = selector_decision
                .as_ref()
                .map(|d| d.answer_mode == SelectorAnswerMode::Direct)
                .unwrap_or(false);
            let use_direct = forward_to_orchestrator
                && (selector_direct || execution_mode == Some(ExecutionMode::Direct))
                && agent_clone.direct_conversation_tx.is_some();
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
                // Use the selector's target_agent when it names a recognised specialist agent.
                // This allows fast-path detectors (e.g. is_external_info_request) to route
                // directly to "search" (or other agents) without an extra round-trip.
                selector_decision
                    .as_ref()
                    .and_then(|d| d.target_agent.as_deref())
                    .filter(|a| SPECIALIST_AGENTS.contains(a))
                    .map(str::to_string)
                    .unwrap_or_else(|| "conversation".to_string())
            } else {
                "orchestrator".to_string()
            };

            // Update the assigned_agent if the selector changed it from our preliminary value.
            if assigned_agent != preliminary_agent {
                match TaskStore::open(store_path) {
                    Ok(store) => {
                        if let Err(e) = store.update_assigned_agent(task_id, &assigned_agent) {
                            tracing::warn!(task_id = %task_id, assigned_agent = %assigned_agent, err = %e, "failed to update assigned_agent after selector");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(task_id = %task_id, err = %e, "failed to open store to update assigned_agent after selector");
                    }
                }
            }

            let _ = agent_clone.bus.send(
                EventEnvelope::new(
                    EventType::TaskCreated,
                    Some(serde_json::json!({
                        "task_id": task_id.to_string(),
                        "parent_task_id": null,
                        "assigned_agent": assigned_agent,
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
                    let tx = agent_clone.direct_conversation_tx.as_ref().unwrap().clone();
                    let guardrail_prefix = if selector_result.timed_out {
                        "[Guardrail: the routing selector timed out. Do NOT write any files unless the user explicitly mentioned a file path or asked to save something. For external information (schedules, weather, news, timetables), use TOOL: web_search first. Reformulate the user's intent carefully before taking any action.]\n\n"
                    } else {
                        ""
                    };
                    let task_msg = OrchestratorTask {
                        task_id,
                        message: format!("{}{}", guardrail_prefix, message),
                        session_id: session_id.to_string(),
                        image_data_urls,
                        execution_mode: None,
                        preferred_task_type: preferred_task_type.clone(),
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
                    agent_clone.orchestrator.send(
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
        });

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
    use super::{
        is_external_info_request, normalize_task_type, parse_selector_decision,
        strip_thinking_tokens, SelectorAnswerMode,
    };

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

    // --- strip_thinking_tokens ---

    #[test]
    fn strip_thinking_tokens_removes_think_block() {
        let input = "<think>reasoning here</think>\n{\"answer_mode\":\"direct\"}";
        assert_eq!(strip_thinking_tokens(input), "{\"answer_mode\":\"direct\"}");
    }

    #[test]
    fn strip_thinking_tokens_returns_original_when_no_tag() {
        let input = "{\"answer_mode\":\"direct\"}";
        assert_eq!(strip_thinking_tokens(input), input);
    }

    #[test]
    fn strip_thinking_tokens_uses_last_tag() {
        let input = "<think>first</think><think>second</think>\njson";
        assert_eq!(strip_thinking_tokens(input), "json");
    }

    #[test]
    fn selector_parse_survives_thinking_tokens() {
        let raw = "<think>let me think about this</think>\n{\"answer_mode\":\"direct\",\"task_type\":\"conversation\",\"reason\":\"weather\"}";
        let stripped = strip_thinking_tokens(raw.trim());
        let d = parse_selector_decision(stripped).expect("should parse after stripping");
        assert_eq!(d.answer_mode, SelectorAnswerMode::Direct);
    }

    // --- is_external_info_request ---

    #[test]
    fn external_info_detects_french_weather() {
        assert!(is_external_info_request("quel temps il va faire aujourd'hui à Cognac ?"));
        assert!(is_external_info_request("météo demain à Paris"));
        assert!(is_external_info_request("donne-moi les prévisions météo"));
    }

    #[test]
    fn external_info_detects_english_weather() {
        assert!(is_external_info_request("what's the weather in London today?"));
        assert!(is_external_info_request("weather forecast for tomorrow"));
    }

    #[test]
    fn external_info_rejects_unrelated_messages() {
        assert!(!is_external_info_request("crée-moi un site web"));
        assert!(!is_external_info_request("écris un script Python"));
        assert!(!is_external_info_request("bonjour, comment vas-tu ?"));
        assert!(!is_external_info_request(""));
    }

    #[test]
    fn external_info_detects_news() {
        assert!(is_external_info_request("quelles sont les actualités du jour ?"));
        assert!(is_external_info_request("les news de ce matin"));
    }
}
