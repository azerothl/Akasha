//! Main LLM + tools loop (`run_message_via_llm`) extracted from `api.rs` (P3 / v0.11).

use crate::agents::{interpret_message, EventBus, OrchestratorTask};
use crate::api::{
    agent_role_system_prompt, available_tools_instruction, available_tools_instruction_exact,
    build_image_markdown, build_session_recap_reply, cancel_task_watchdogs,
    canonicalize_tool_name, captured_media_suitable_for_vision_injection,
    classify_small_talk_message, code_studio_tools_for_prompt, compact_short_term_if_needed,
    compute_message_intent_flags, detect_session_recall_intent, do_install_skill,
    do_uninstall_skill, embedded_tools_instruction_hint, ensure_no_open_code_block,
    execute_tool_call, extract_semaphore, extract_x_profile_handle, insert_task_tracking_event,
    learn_from_task_outcome_async, load_permission_mode, log_tool_journal_if_write,
    looks_like_manual_file_patch_reply, looks_like_meta_agent_response,
    looks_like_off_topic_greeting, looks_like_placeholder_after_tools, memory_profile_for_task,
    memory_recall_timeout_secs, message_suggests_project, normalize_captured_media_for_vision_turn,
    normalize_tool_path_hint, notify_task_completion, parse_tool_name_from_toolcall_delta,
    policy_allows_primary_disk_write, progress_message_for_tool,
    response_looks_off_topic_for_small_talk, schedule_code_studio_index_for_root,
    should_skip_capture_content, should_use_compact_local_prompt, small_talk_fast_lane,
    small_talk_fast_reply, spawn_progress_watchdog, spawn_task_stall_watchdog,
    studio_semantic_acceptance_review, studio_verify_analyzing_progress_line,
    studio_verify_build_excerpt_label, studio_verify_detect_user_language,
    studio_verify_explain_failure_to_user, studio_verify_failure_banner,
    studio_verify_run_llm_autofix_rounds, studio_verify_summary_heading_markdown, tool_scope_key,
    vision_inject_max_chars, vision_payload_within_cap, web_followup_tools_configured,
    workspace_lineage_root_task_id, AgentProfile, AgentProfileCache, DelegationRequest, HumanInputStore,
    MemoryProfile, MessageIntentFlags, PendingHumanInput, ProcessRegistry, SessionRecallIntent,
    SessionRecallRange, SmallTalkIntent, SmallTalkLanguage, SteeringQueueStore,
    TaskCompletionRegistry, TaskUsageStore, TaskWorkspaceStore, APP_CONTEXT, CAPTURE_MAX_CHARS,
    CAPTURE_MAX_PER_TURN, CODE_DEV_SANDBOX_REMINDER, CODE_STUDIO_APP_CONTEXT,
    DEVICE_CAMERA_REMINDER, EMBEDDED_APP_CONTEXT, GEO_DISTANCE_REMINDER_NO_TOOL,
    GEO_DISTANCE_REMINDER_WITH_TOOLS, GITHUB_VAULT_REMINDER, IMAGE_GENERATION_REMINDER,
    ORCH_DISK_DELIVERABLES_MARKER, SOCIAL_FEED_REMINDER, STUDIO_AGENT_QUALITY_REMINDER,
    STUDIO_DISK_REMINDER, TRANSPORT_REMINDER, TRANSPORT_REMINDER_NO_SEARCH, WEB_SEARCH_FOLLOWUP_REMINDER,
    WEB_SEARCH_REMINDER, WEB_SEARCH_UNAVAILABLE_REMINDER, WRITE_FILE_REMINDER,
    get_or_load_agent_profile, set_agent_profile_cache,
};
use crate::api_path_utils::{parse_read_file_args, READ_FILE_PARTIAL_DEFAULT_MARKER};
use crate::api_tool_parser::parse_tool_calls;
use crate::autonomous_mission_config::{AutonomousMissionConfig, MissionStatusYaml};
use crate::latency::{
    clear_task_milestones, emit_timeline_once_for_task, log_latency_metric, resolve_root_task_id,
};
use crate::memory::ShortTermStore;
use crate::memory_actor::LongTermMemoryClient;
use crate::user_profile::UserProfile;
use akasha_core::{EventEnvelope, EventType};
use akasha_llm::CompletionRequest;
use akasha_plugin_api::{PluginKind, PluginManifest};
use akasha_store::{
    format_todos_plan_block, parse_todos_from_payload, TaskStatus, TaskStore, TodoStatus,
    WorkspaceGraphStore,
};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, RwLock};
use uuid::Uuid;

/// Run LLM completion for a user message, with short-term + long-term memory (and compaction), optional tool-use loop. Push reply as progress, mark task completed.
/// image_data_urls: optional list of data URLs (data:image/...;base64,...) for vision-capable models.
/// preferred_task_type_override: when set (e.g. system selector task_type), overrides routing/reminders vs. assigned_agent alone.
pub(crate) async fn run_message_via_llm(
    bus: EventBus,
    llm_router: Arc<akasha_llm::LLMRouter>,
    store_path: std::path::PathBuf,
    spec_dir: std::path::PathBuf,
    task_id: Uuid,
    message: String,
    session_id: String,
    image_data_urls: Option<Vec<String>>,
    // When set (e.g. system selector task_type), overrides resolve_task_type_for_agent(assigned_agent) for LLM routing and image-gen reminders.
    preferred_task_type_override: Option<String>,
    short_term: Option<std::sync::Arc<ShortTermStore>>,
    long_term_client: Option<LongTermMemoryClient>,
    tools_executor: Option<
        std::sync::Arc<tokio::sync::RwLock<std::sync::Arc<akasha_tools::ToolExecutor>>>,
    >,
    tools_policy_path: Option<std::path::PathBuf>,
    skill_registry: Option<std::sync::Arc<crate::skills::SkillRegistry>>,
    plugin_registry: Option<std::sync::Arc<crate::plugins::PluginRegistry>>,
    process_registry: Option<ProcessRegistry>,
    conv_tx: Option<mpsc::Sender<OrchestratorTask>>,
    human_input_store: Option<HumanInputStore>,
    steering_queue: Option<SteeringQueueStore>,
    delegation_tx: Option<mpsc::Sender<DelegationRequest>>,
    task_completion_registry: Option<TaskCompletionRegistry>,
    agent_profile_cache: Option<AgentProfileCache>,
    task_usage_store: Option<std::sync::Arc<TaskUsageStore>>,
    device_bridge: Option<std::sync::Arc<crate::device_bridge::DeviceBridge>>,
    workspace_store: Option<TaskWorkspaceStore>,
    browser_registry: Option<crate::browser::BrowserSessionRegistry>,
    autonomous_mission: Option<Arc<RwLock<AutonomousMissionConfig>>>,
    studio_disk_registry: crate::studio::StudioDiskRootRegistry,
    studio_worktree_registry: crate::studio_worktree::StudioWorktreeRegistry,
    incognito: bool,
) {
    if let Ok(store) = TaskStore::open(&store_path) {
        let _ = store.update_status(task_id, TaskStatus::Running);
    }
    let _ = bus.send(
        EventEnvelope::new(
            EventType::ProgressUpdate,
            Some(serde_json::json!({
                "task_id": task_id.to_string(),
                "progress_pct": 5,
                "message": "Analyzing your request…"
            })),
        )
        .with_correlation(task_id),
    );
    // Start stall/progress watchdogs immediately after the first progress line so a hang in
    // studio setup, session_state I/O, or context assembly still surfaces updates and can fail the task.
    let meaningful_progress_flag =
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let timeline_correlation = task_id;
    let (watchdog_tx, watchdog_rx) = oneshot::channel();
    let mut watchdog_cancel = Some(watchdog_tx);
    let (stall_tx, stall_rx) = oneshot::channel();
    let mut stall_cancel = Some(stall_tx);
    spawn_progress_watchdog(
        bus.clone(),
        store_path.clone(),
        timeline_correlation,
        task_id,
        watchdog_rx,
    );
    spawn_task_stall_watchdog(
        bus.clone(),
        store_path.clone(),
        timeline_correlation,
        task_id,
        meaningful_progress_flag.clone(),
        task_completion_registry.clone(),
        steering_queue.clone(),
        stall_rx,
    );
    let (message, embedded_studio_acceptance) =
        crate::api_studio::strip_embedded_acceptance_json(&message);
    let lineage_for_studio = workspace_lineage_root_task_id(task_id, Some(store_path.as_path()));
    let data_dir_for_studio_flags = store_path.parent().unwrap_or_else(|| store_path.as_ref());
    let tool_disk_workspace_root: std::path::PathBuf = {
        if let Some(wt) = crate::studio_worktree::get_worktree_for_task(&studio_worktree_registry, task_id).await
        {
            wt.worktree_path
        } else {
        let reg = studio_disk_registry.read().await;
        if let Some(p) = reg.get(&lineage_for_studio) {
            p.clone()
        } else {
            drop(reg);
            let mut resolved: Option<std::path::PathBuf> = None;
            if let Some(pid) = TaskStore::open(&store_path)
                .ok()
                .and_then(|s| s.get(lineage_for_studio).ok().flatten())
                .and_then(|t| t.studio_project_id.clone())
            {
                if let Ok(dir) = crate::studio::resolve_studio_project_dir(data_dir_for_studio_flags, &pid) {
                    resolved = Some(dir);
                }
            }
            if let Some(dir) = resolved {
                let mut w = studio_disk_registry.write().await;
                w.insert(lineage_for_studio, dir.clone());
                dir
            } else {
                store_path
                    .parent()
                    .map(|x| x.to_path_buf())
                    .unwrap_or_else(|| std::path::PathBuf::from("."))
            }
        }
        }
    };
    let code_studio_disk_task = tool_disk_workspace_root
        .starts_with(crate::studio::studio_projects_base(data_dir_for_studio_flags));
    let studio_disk_system_append = {
        let proj_base = crate::studio::studio_projects_base(data_dir_for_studio_flags);
        if tool_disk_workspace_root.starts_with(proj_base) {
            format!("{}{}", STUDIO_DISK_REMINDER, STUDIO_AGENT_QUALITY_REMINDER)
        } else {
            String::new()
        }
    };
    // NOTE: interpret_message is called below, after guardrail extraction, so it uses clean_message.
    let task_snapshot = TaskStore::open(&store_path)
        .ok()
        .and_then(|s| s.get(task_id).ok().flatten());
    let assigned_agent = task_snapshot
        .as_ref()
        .map(|t| t.assigned_agent.clone())
        .unwrap_or_else(|| "conversation".to_string());
    let role_agent_for_system_prompt =
        if preferred_task_type_override.as_deref() == Some("image_generation") {
            "image_generation"
        } else {
            assigned_agent.as_str()
        };
    let is_subagent = task_snapshot
        .as_ref()
        .and_then(|t| t.parent_task_id)
        .is_some();
    let router_task_type_early = if preferred_task_type_override.as_deref()
        == Some("image_generation")
        || assigned_agent == "image_generation"
    {
        llm_router.resolve_task_type_for_agent("conversation")
    } else {
        preferred_task_type_override
            .clone()
            .unwrap_or_else(|| llm_router.resolve_task_type_for_agent(&assigned_agent))
    };
    if let Some((provider, model)) =
        llm_router.primary_route_for_task_type(&router_task_type_early)
    {
        insert_task_tracking_event(
            store_path.as_path(),
            task_id,
            "llm_route_planned",
            serde_json::json!({
                "task_type": router_task_type_early,
                "provider": provider,
                "model": model,
                "phase": "pre_context",
            }),
        );
    }
    let embedded_primary_route = llm_router
        .primary_route_for_task_type(&router_task_type_early)
        .map(|(p, m)| should_use_compact_local_prompt(&p, &m))
        .unwrap_or(false);
    if let Some(ref sq) = steering_queue {
        sq.register_active(task_id, session_id.clone(), !is_subagent)
            .await;
    }
    if code_studio_disk_task && !is_subagent {
        let dd = data_dir_for_studio_flags.to_path_buf();
        let root = tool_disk_workspace_root.clone();
        let tid = task_id;
        match tokio::time::timeout(
            std::time::Duration::from_secs(45),
            tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
            let snap_path = dd.join("studio-task-snapshots").join(format!("{tid}.json"));
            if snap_path.exists() {
                return Ok(false);
            }
            crate::studio_task_snapshot::capture_task_snapshot(&dd, tid, &root)?;
            Ok(true)
        }),
        )
        .await
        {
            Ok(Ok(Ok(true))) => {}
            Ok(Ok(Ok(false))) => tracing::debug!(task_id = %tid, "studio task snapshot already captured; skipping"),
            Ok(Ok(Err(e))) => tracing::warn!(task_id = %tid, error = %e, "studio task snapshot capture failed"),
            Ok(Err(e)) => tracing::warn!(task_id = %tid, error = %e, "studio task snapshot join failed"),
            Err(_) => tracing::warn!(task_id = %tid, "studio task snapshot timed out"),
        }
    }
    // When main_agent prepends a guardrail block on selector timeout, extract the real user message.
    // Format: "[Guardrail: …]\n\n<actual message>"
    const GUARDRAIL_MARKER: &str = "[Guardrail:";
    const GUARDRAIL_END: &str = "]\n\n";
    let (guardrail_reminder_block, clean_message): (String, &str) =
        if message.starts_with(GUARDRAIL_MARKER) {
            if let Some(end) = message.find(GUARDRAIL_END) {
                let block = format!("{}\n\n", &message[..end + 1]);
                let actual = message[end + GUARDRAIL_END.len()..].trim_start();
                (block, actual)
            } else {
                (String::new(), message.as_str())
            }
        } else {
            (String::new(), message.as_str())
        };
    // Whether this is an orchestrated subtask message (starts with "[Task]\n").
    // These messages must not pollute session goals, short-term history, or trigger full
    // memory context — they are internal planner artefacts, not real user messages.
    let is_orchestrated_task_msg = clean_message.starts_with("[Task]\n");

    // Anchor the CLEAN user goal (without guardrail prefix) in session state at task start.
    // Skip for orchestrated task messages — their [Task]\nObjective text is not a user goal.
    if !is_subagent && !is_orchestrated_task_msg && !clean_message.trim().is_empty() {
        let data_dir_goal = store_path.parent().unwrap_or_else(|| store_path.as_ref()).to_path_buf();
        let goal_text = clean_message.chars().take(240).collect::<String>();
        let session_id_goal = session_id.clone();
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(8),
            tokio::task::spawn_blocking(move || {
                crate::session_state::merge(&data_dir_goal, &session_id_goal, |s| {
                    if s.goals.iter().all(|g| g != &goal_text) {
                        s.goals.push(goal_text);
                    }
                })
            }),
        )
        .await;
    }
    let timeline_correlation = resolve_root_task_id(&store_path, task_id)
        .or_else(|| task_snapshot.as_ref().and_then(|t| t.parent_task_id))
        .unwrap_or(task_id);

    // All intent detection / classification uses clean_message so a guardrail prefix never
    // breaks fast-lane matching or memory profile selection.
    let structured = interpret_message(clean_message);
    let orch_disk_deliverables = clean_message.contains(ORCH_DISK_DELIVERABLES_MARKER);
    // Slim prompt for embedded + small Ollama (also for subagents — full RULE dumps cause rule-echo).
    let embedded_compact = embedded_primary_route && !code_studio_disk_task;
    // Code Studio tasks run on studio-projects/* disk roots: do not treat user prompts as
    // "small talk" or suppress tool-heavy LLM replies — that blocked write_file / TOOL lines.
    let small_talk_intent = if code_studio_disk_task {
        None
    } else {
        classify_small_talk_message(clean_message)
    };
    // Never treat Code Studio disk tasks as « session recap » — injected prefixes can contain
    // words like « rappellent » (substring « rappel » used to trigger the recap fast path).
    let session_recall_intent = if code_studio_disk_task {
        None
    } else {
        detect_session_recall_intent(clean_message)
    };
    tracing::debug!(
        ?session_recall_intent,
        "[RECALL_DEBUG] session_recall_intent"
    );
    let is_small_talk_fast_lane = if code_studio_disk_task {
        false
    } else {
        small_talk_fast_lane(clean_message).is_some()
    };
    let is_session_recall = session_recall_intent.is_some();
    let mut memory_profile = if is_small_talk_fast_lane || is_session_recall {
        MemoryProfile {
            recent_turns_limit: 0,
            recent_context_max_chars: 0,
            semantic_top_k: 0,
            episodic_limit: 0,
            facts_limit: 0,
            user_rag_top_k: 0,
            workspace_graph_top_k: 0,
            expand_by_graph: false,
            graph_expand_hops: 0,
            compact_before_prompt: false,
            allow_project_recall: false,
            allow_identity_lookup: false,
        }
    } else {
        memory_profile_for_task(
            clean_message,
            &assigned_agent,
            is_subagent,
            orch_disk_deliverables,
        )
    };
    if incognito {
        memory_profile.recent_turns_limit = 0;
        memory_profile.recent_context_max_chars = 0;
        memory_profile.semantic_top_k = 0;
        memory_profile.episodic_limit = 0;
        memory_profile.facts_limit = 0;
        memory_profile.user_rag_top_k = 0;
        memory_profile.workspace_graph_top_k = 0;
        memory_profile.graph_expand_hops = 0;
        memory_profile.expand_by_graph = false;
        memory_profile.allow_project_recall = false;
        memory_profile.allow_identity_lookup = false;
    }
    // For external/transport/general-knowledge queries, always isolate from recent context.
    // Loading previous dev/Akasha-specific turns from short-term history actively misleads
    // small local models: they latch onto the most recent topic (e.g. Akasha CLI discussion)
    // and copy it instead of answering the actual question.
    // This cap is unconditional — it does NOT require a guardrail prefix to be active.
    let message_intent_flags_clean = compute_message_intent_flags(clean_message);
    if !is_small_talk_fast_lane && !is_subagent {
        if message_intent_flags_clean.external_info || message_intent_flags_clean.transport {
            memory_profile.recent_turns_limit = 0;
            memory_profile.episodic_limit = 0;
            memory_profile.semantic_top_k = 0;
            memory_profile.facts_limit = 0;
            memory_profile.allow_project_recall = false;
        }
    }
    // Orchestrated task messages ([Task]\n prefix) already carry full context inside the message.
    // Loading unrelated recent_turns from the short-term store only introduces noise and causes
    // context contamination (e.g. bankr venv script appearing for a train/car question).
    if is_orchestrated_task_msg {
        memory_profile.recent_turns_limit = 0;
        memory_profile.semantic_top_k = memory_profile.semantic_top_k.min(1);
        memory_profile.compact_before_prompt = false;
    }
    // Code Studio: avoid global long-term memory, user document RAG, multi-workspace graph indexes,
    // and cross-session episodic bleed — the prompt already carries plan/stack and disk context.
    if code_studio_disk_task {
        memory_profile.semantic_top_k = 0;
        memory_profile.episodic_limit = 0;
        memory_profile.facts_limit = 0;
        memory_profile.user_rag_top_k = 0;
        memory_profile.workspace_graph_top_k = 0;
        memory_profile.expand_by_graph = false;
        memory_profile.allow_project_recall = false;
        memory_profile.recent_context_max_chars = memory_profile.recent_context_max_chars.min(12_000);
    }
    if embedded_compact {
        memory_profile.recent_turns_limit = memory_profile.recent_turns_limit.min(4);
        memory_profile.recent_context_max_chars = memory_profile.recent_context_max_chars.min(4_000);
        memory_profile.semantic_top_k = memory_profile.semantic_top_k.min(2);
        memory_profile.episodic_limit = memory_profile.episodic_limit.min(2);
        memory_profile.facts_limit = memory_profile.facts_limit.min(2);
        memory_profile.user_rag_top_k = memory_profile.user_rag_top_k.min(1);
        memory_profile.workspace_graph_top_k = 0;
        memory_profile.expand_by_graph = false;
        memory_profile.compact_before_prompt = true;
    }

    let tools_executor_snapshot = match &tools_executor {
        Some(r) => Some((*r.read().await).clone()),
        None => None,
    };

    let message_webhook_url = std::env::var("AKASHA_MESSAGE_WEBHOOK_URL").ok();

    // Emit user message so TUI/API can show "what this task is about"
    let _ = bus.send(
        EventEnvelope::new(
            EventType::UserRequestReceived,
            Some(serde_json::json!({ "message": message })),
        )
        .with_correlation(task_id),
    );

    let max_tokens = std::env::var("AKASHA_MAX_RESPONSE_TOKENS")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(4096);
    let mut completion_max_tokens = if is_small_talk_fast_lane {
        max_tokens.min(256).max(64)
    } else {
        max_tokens
    };
    // Compact prompt ≠ tiny completion budget. Ollama mid-size (e.g. qwen3.5:9b) with
    // thinking enabled can burn 512 tokens on reasoning and return an empty `response`.
    let compact_is_embedded_provider = llm_router
        .primary_route_for_task_type(&router_task_type_early)
        .map(|(p, _)| p == "akasha_embedded" || p == "akasha_core")
        .unwrap_or(false);
    if embedded_compact {
        let (env_key, default_cap) = if compact_is_embedded_provider {
            ("AKASHA_EMBEDDED_MAX_TOKENS", 512u32)
        } else {
            ("AKASHA_OLLAMA_COMPACT_MAX_TOKENS", 2048u32)
        };
        let cap = std::env::var(env_key)
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .filter(|&n| n >= 16)
            .unwrap_or(default_cap);
        completion_max_tokens = completion_max_tokens.min(cap);
    }
    // Prefer a visible answer over long silent reasoning on local/Ollama compact paths.
    let completion_thinking_level: Option<String> = if embedded_compact {
        Some("off".to_string())
    } else {
        None
    };

    let mut code_studio_system_tools_block = String::new();
    let tool_instruction = if is_small_talk_fast_lane {
        String::new()
    } else if embedded_compact && tools_executor_snapshot.is_some() {
        let allowed_tools = tools_executor_snapshot
            .as_ref()
            .and_then(|e| e.policy.allowed_tool_list());
        embedded_tools_instruction_hint(allowed_tools.as_deref())
    } else if tools_executor_snapshot.is_some() {
        let mut allowed_tools = tools_executor_snapshot
            .as_ref()
            .and_then(|e| e.policy.allowed_tool_list());
        if let Some(ref list) = allowed_tools {
            let has_wildcard = tools_executor_snapshot
                .as_ref()
                .map(|e| {
                    e.policy
                        .allowed_commands
                        .iter()
                        .any(|c| c.trim().eq_ignore_ascii_case("*"))
                })
                .unwrap_or(false);
            if has_wildcard && !list.iter().any(|t| t == "run_command") {
                let mut list = list.clone();
                list.push("run_command".to_string());
                allowed_tools = Some(list);
            }
        }
        if orch_disk_deliverables {
            if let Some(ref list) = allowed_tools {
                if !list.iter().any(|t| t == "write_file" || t == "write_code") {
                    tracing::warn!(
                        task_id = %task_id,
                        "Orchestrated deliverables: tools_policy default_profile omits write_file/write_code; \
                         workspace file tools will NOT be advertised to the model — add write_file or write_code \
                         (and other file tools) to the profile to enable disk deliverables."
                    );
                }
            }
        }
        let studio_tool_list = if code_studio_disk_task {
            Some(code_studio_tools_for_prompt(
                allowed_tools.as_deref(),
                assigned_agent.as_str(),
            ))
        } else {
            None
        };
        let base = if let Some(ref v) = studio_tool_list {
            available_tools_instruction_exact(v)
        } else {
            available_tools_instruction(allowed_tools.as_deref())
        };
        let run_command_os_rule = match std::env::consts::OS {
            "windows" => "RUN_COMMAND OS: You are on Windows. Prefer cmd, PowerShell, curl.exe; avoid grep, cat, sed (not in default PATH). Use full path or .exe when needed. To test that the vault token works (e.g. GitHub API), use Invoke-WebRequest: TOOL: run_command VAULT:GITHUB_TOKEN=GITHUB_TOKEN powershell -NoProfile -Command \"Invoke-WebRequest -Uri 'https://api.github.com/repos/owner/repo' -Headers @{ Authorization = 'Bearer ' + $env:GITHUB_TOKEN } | Select-Object -Expand Content\" (replace owner/repo). Ensure 'powershell' is in allowed_commands in tools_policy.yaml. The system injects the vault value into the environment for the command.\n\
             ",
            _ => "RUN_COMMAND OS: You are on Linux/macos. Standard Unix commands (curl, grep, etc.) are available.\n\
             ",
        };
        if code_studio_disk_task {
            let skills_part_studio = match &skill_registry {
                Some(reg) => {
                    let list = reg.list().await;
                    if list.is_empty() {
                        String::new()
                    } else {
                        let names: Vec<&str> = list.iter().map(|s| s.name.as_str()).collect();
                        format!(
                            " ; Skills (n’utiliser que si pertinent pour ce dépôt ; sinon ignorer) : {}",
                            names.join(", ")
                        )
                    }
                }
                None => String::new(),
            };
            code_studio_system_tools_block = format!(
                "[Code Studio — outils]\n\
                 Une ligne par invocation : `TOOL: nom_outil arg1 …`.\n\
                 Disponibles : {}{}.\n\
                 Règles :\n\
                 - Outils strictement nécessaires à la demande sur ce dépôt ; pas d’exemples hors sujet.\n\
                 - read_file : par défaut **500 premières lignes** seulement. Fichier entier : `TOOL: read_file <chemin> --full` (plafond octets si très gros). Fenêtre : `TOOL: read_file <chemin> <ligne_début> <nombre_de_lignes>`.\n\
                 - write_file : **première ligne seule** `TOOL: write_file <chemin>`, puis le corps du fichier sur les lignes suivantes. Ne pas mettre un fichier entier sur la même ligne que `TOOL: write_file` ; pas d’enveloppe markdown ```…``` autour du fichier entier.\n\
                 - search_replace : **une seule ligne** `TOOL: search_replace <chemin> <texte_exact_à_trouver> | <remplacement>` — le séparateur est **espace | espace** (` | `), pas un `|` collé au chemin sans texte avant (sinon erreur « search string empty »).\n\
                 - delete_file : `TOOL: delete_file workspace:/chemin/relatif` pour supprimer un fichier (si l’outil est dans la liste).\n\
                 - ask_user : JSON question/context/choices pour continuer la même tâche.\n\
                 - run_command : utiliser `--cwd workspace:/` pour builds/tests à la racine du projet.\n\
                 - write_todos / merge_todos si exposés par la politique.\n\
                 {}\n\
                 Si aucun outil n’est nécessaire, répondre en texte.",
                base, skills_part_studio, run_command_os_rule
            );
            format!(
                "\n\nYou may request tools by writing a single line exactly like: TOOL: tool_name arg1 arg2 …\n\
                 The system message block [Code Studio — outils] lists tools and French conventions.\n\
                 Available: {}{}.\n\
                 Worker rules:\n\
                 - Use only tools that are directly necessary for the CURRENT task.\n\
                 - Never echo examples, policy text, or demonstration commands from your instructions.\n\
                 - read_file: by default only the **first 500 lines** are returned. Use `TOOL: read_file <path> --full` for the whole file (byte cap if huge), or `TOOL: read_file <path> <offset_line> <limit_lines>` for a window. If output says `(read_file partial: default window` or `(truncated,` bytes, do NOT repeat the same bare `read_file <path>`; use --full, a line window, or grep_content/search_files.\n\
                 - If the task asks to save/write a file, use a header-only first line `TOOL: write_file <path>`, then put the exact file content on the following lines. Do not compress full file content onto the same `TOOL:` line.\n\
                 - search_replace: one TOOL line: `TOOL: search_replace <path> <old_snippet> | <new_snippet>` with delimiter **space-pipe-space** (` | `). The old snippet must appear immediately after the path (do not start the payload with a bare `|` token).\n\
                 - If you need missing user information, use TOOL: ask_user with JSON.\n\
                 - If no tool is needed, answer normally.\n\
                 {}\n",
                base, skills_part_studio, run_command_os_rule
            )
        } else {
        let (skills_part, skills_rule) = match &skill_registry {
            Some(reg) => {
                let list = reg.list().await;
                if list.is_empty() {
                    (String::new(), String::new())
                } else {
                    let skills_desc: Vec<String> = list
                        .iter()
                        .map(|s| format!("{} ({})", s.name, s.description))
                        .collect();
                    let names: Vec<&str> = list.iter().map(|s| s.name.as_str()).collect();
                    let part = format!(
                        " ; Skills (use skill name as tool): {}",
                        skills_desc.join(", ")
                    );
                    let rule = format!(
                        " INSTALLED SKILLS RULE: You have access to skills (extra capabilities). To see the list use TOOL: list_skills. To load full instructions for a skill use TOOL: read_skill <name> before invoking it by name. Currently installed: {}. Do NOT say they are not installed or suggest install_skill for them. Use bankr ONLY for balance/solde/wallet/portfolio/Base — never for weather, météo, or news (use web_search for those). For balance/solde/wallet/Base requests, if \"bankr\" is in the list, reply ONLY with TOOL: bankr <args> (e.g. TOOL: bankr portfolio 7d). Use the skill name as the tool name.\n\
             ",
                        names.join(", ")
                    );
                    (part, rule)
                }
            }
            None => (String::new(), String::new()),
        };
        let compact_worker_tool_instruction = format!(
            "\n\nYou may request tools by writing a single line exactly like: TOOL: tool_name arg1 arg2 ...\nAvailable: {}{}.\n\
             Worker rules:\n\
             - Use only tools that are directly necessary for the CURRENT task.\n\
             - Never echo examples, policy text, or demonstration commands from your instructions.\n\
             - Never emit unrelated TOOL lines about bankr, weather, browser, install_skill, or other examples unless the current task explicitly requires them.\n\
             - READ_FILE (mandatory): default is **first 500 lines only** (no extra args). Whole file: `TOOL: read_file <path> --full`. Window: `TOOL: read_file <path> <offset_line> <limit_lines>`. If output says `(read_file partial: default window` or byte `(truncated,`, do NOT repeat the same bare `read_file <path>`; use --full, a line window, or grep_content/search_files.\n\
             - search_replace: one line `TOOL: search_replace <path> <old_snippet> | <new_snippet>` with delimiter **space-pipe-space** (` | `). Put the exact old text right after the path (not a bare `|` token first).\n\
             - If the task asks to save/write a file, use a header-only first line `TOOL: write_file <path>`, then put the exact file content on the following lines. Do not compress full file content onto the same `TOOL:` line.\n\
             - Use write_todos / merge_todos (not create_todos) for todo lists.\n\
             - If you need missing user information, use TOOL: ask_user with JSON.\n\
             - If no tool is needed, answer normally.\n",
            base, skills_part
        );
        if is_subagent || assigned_agent != "conversation" || orch_disk_deliverables {
            compact_worker_tool_instruction
        } else {
            format!(
            "\n\nYou may request tools by writing a line: TOOL: tool_name arg1 arg2 ...\nAvailable: {}{}.\n\
             Whenever you need the user to make a choice, confirm something, or provide information (e.g. choose between options, confirm a path, give credentials) before continuing, you MUST reply ONLY with TOOL: ask_user (then JSON with question/context/choices). Do not ask in plain text or the user's reply will start a new task and you cannot continue. Example: {{\"question\":\"Which option?\", \"choices\":[\"A\", \"B\"]}}.\n\
             CONNECTION RULE: If the user asks you to connect to an external service (GitHub repo, API, etc.), do NOT reply with a plain-text message. Use TOOL: ask_user. If the user has already confirmed credentials are configured, do NOT send another ask_user; proceed. Do not invent commands (e.g. /status repo:... does not exist); real commands are in /help.\n\
             CAMERA RULE (priority over WRITE): When the user asks for a webcam/camera photo (e.g. \"prends une photo\", \"take a photo\", \"photo depuis la webcam\", \"affiche-la dans le chat\", \"display it in the chat\"), you MUST reply ONLY with TOOL: device_discover local_media then TOOL: device_invoke local_media camera capture. Do NOT mention tools_policy.yaml, allowed_write_paths, or file writing. After the tool returns, if the user asked to \"display in the chat\" / \"affiche-la dans le chat\" / \"show it in the chat\", reply with ONLY a short confirmation in the user's language (e.g. in French: \"Photo prise. Elle s'affiche ci-dessous.\"; in English: \"Photo captured. It is shown below.\"). Do NOT offer \"save to file\", \"get a description\", \"take another photo\", or \"What would you like to do next?\" — the image is appended automatically below your message. Use the same language as the user (French if they wrote in French).\n\
             WRITE RULE (mandatory): When the user asks to save, record, or write a file (e.g. \"enregistre\", \"sauvegarde\", \"save to\", \"write to file\", or gives a folder path), you MUST reply ONLY with: a first header line \"TOOL: write_file <full_path>\" then on the following lines the exact file content. Do NOT put full file content on the same TOOL line. Do NOT answer with \"I cannot write to disk\" or \"copy-paste the code yourself\". Use write_file; if the path is denied, the tool returns an error and you then explain tools_policy.yaml (allowed_write_paths). Paths can be Windows (C:\\Users\\...\\file.py) or Unix. Do NOT apply this rule when the user only asked for a webcam photo.\n\
             WEATHER RULE (PRIORITY): When the user asks for weather, météo, or forecasts (e.g. \"quel temps\", \"météo demain\", \"weather in X\"), you MUST use TOOL: web_search <query> first, then if snippets lack numeric detail use TOOL: web_fetch <url> on a trusted result URL and/or TOOL: browser navigate <url> then TOOL: browser snapshot (many weather sites are JS-heavy). Do NOT use bankr, portfolio, or any other skill for weather — use web_search plus web_fetch/browser as needed.\n\
             BROWSER RULE (PRIORITY): When the user explicitly asks to open the browser, go to a website, or show something on X/Twitter (e.g. \"ouvre le navigateur\", \"open the browser\", \"va sur X\", \"go to twitter\", \"cherche sur X\", \"ouvre le navigateur et cherche\"), you MUST use TOOL: browser navigate <url> first with the appropriate URL (e.g. https://x.com/akashabot for a profile, https://x.com for the home page). You may then add a short message. Do NOT use only web_search when the user asked to open the browser or go to X/Twitter.\n\
             SOCIAL / LOGGED-IN SITES RULE: If tools_policy allows the domain, use TOOL: browser navigate <https URL> and TOOL: browser snapshot when the user asks to open or inspect X/Twitter or similar. Do NOT refuse with vague \"security\", \"confidentiality\", or \"structural policy\" claims — the user runs Akasha locally and controls tools_policy. Real limitation: you cannot type the user's password or complete interactive MFA inside the managed browser on their behalf; if a login wall blocks content, say that clearly and offer practical options (user logs in manually in that same browser session if their environment keeps the session, or official API access via TOOL: run_command with VAULT:... when applicable). Do NOT state that vault-backed API access is forbidden when the user has configured secrets — follow VAULT ENV RULE.\n\
             WEB SEARCH RULE: When the user asks for external information (weather, news, forecasts, schedules, etc.) that you do not have, you MUST use TOOL: web_search <query> first, then answer from fetched content — not only from snippets. If snippets are insufficient, use TOOL: web_fetch <url> on a relevant result URL, or TOOL: browser navigate <url> then TOOL: browser snapshot so YOU retrieve the page text inside Akasha (managed browser), then summarize for the user. Do NOT reply with \"I did not find it\" or suggest sites without having called web_search. Do NOT tell the user to open links in their own browser when web_fetch or browser snapshot is available and allowed — retrieve and answer yourself.\n\
             READ_FILE (mandatory): default is **first 500 lines only** (no extra args). Whole file: `TOOL: read_file <path> --full`. Window: `TOOL: read_file <path> <offset_line> <limit_lines>`. If output says `(read_file partial: default window` or byte `(truncated,`, do NOT repeat the same bare `read_file <path>`; use --full, a line window, or grep_content/search_files.\n\
             SEARCH_REPLACE: one line `TOOL: search_replace <path> <old_snippet> | <new_snippet>` — delimiter **space-pipe-space** (` | `). The old snippet must follow the path immediately (tokenizer may emit `|` as its own token; do not start the payload with `|` alone).\n\
             INSTALL CLI RULE: When the user asks to install a CLI or package globally (e.g. \"install bankr CLI\", \"npm install -g @bankr/cli\", \"install the bankr cli in global\"), you MUST reply ONLY with TOOL: run_command <cmd> <args> (e.g. TOOL: run_command npm install -g @bankr/cli). Do NOT generate a script or ask the user to run commands themselves; run the installation command via the tool.\n\
             VAULT ENV RULE: To use a vault secret in a command you MUST call TOOL: run_command with VAULT:<vault_key>=<ENV_VAR> as the FIRST argument(s), then the command. The system injects the secret value into ENV_VAR for that command only. Example: TOOL: run_command VAULT:GITHUB_TOKEN=GITHUB_TOKEN curl -sS -H \"Authorization: Bearer $GITHUB_TOKEN\" https://api.github.com/repos/owner/repo. FORBIDDEN: never tell the user to run GITHUB_TOKEN=VAULT:GITHUB_TOKEN or export GITHUB_TOKEN=... or VAULT:GITHUB_TOKEN=ghp_... — you must output the TOOL: line yourself so the system runs the command and injects the token. For GitHub with token in vault: use TOOL: run_command VAULT:GITHUB_TOKEN=GITHUB_TOKEN curl -sS -H \"Authorization: Bearer $GITHUB_TOKEN\" https://api.github.com/repos/owner/repo (or gh repo view owner/repo). The vault key may be GITHUB_TOKEN or github_token; the part after = is the env var name the command uses (e.g. $GITHUB_TOKEN). Do NOT say you cannot access the repo without having called run_command with VAULT:... first.\n\
             {}\
             INSTALL SKILL RULE: When the user asks to install, download, get, fetch, or add a skill from a URL (e.g. \"install the bankr skill from …\", \"download the skill at this url\", \"get the skill from this url\", \"récupère le skill …\"), you MUST reply ONLY with TOOL: install_skill <url>. Do not give manual steps; perform the installation yourself. If the user says \"follow the SKILL.md instructions\" or \"follow the instructions in SKILL.md\", you MUST first reply with TOOL: install_skill <url> so the skill is registered; only after it is installed can you invoke it by name (e.g. TOOL: <skill_name> <args>). Do NOT use web_fetch or read_file to fetch SKILL.md and then execute its steps manually.\n\
             UNINSTALL SKILL RULE: When the user asks to uninstall or remove a skill (e.g. \"désinstalle bankr\", \"remove the bankr skill\"), you MUST reply ONLY with TOOL: uninstall_skill <name> (e.g. TOOL: uninstall_skill bankr).\n\
             SKILL USE RULE: When the user asks you to perform an action using a skill (e.g. \"vérifie mon wallet bankr\", \"check my balance with bankr\", \"run bankr whoami\"), you MUST reply ONLY with a single line: TOOL: <skill_name> <args> (e.g. TOOL: bankr whoami). The system will execute the command and return the result. Do NOT tell the user to run the command themselves or to \"use TOOL: bankr whoami\"; you must output that line yourself so the tool is executed.\n\
             PROJECT RULE: For requests that imply a substantial deliverable (novel, comic/BD, code project, series of chapters or files), never claim completion after one response if the full scope is not delivered. State clearly what was done, what remains to do, and that you will continue on the user's next message (or via a sub-task). Do not say \"C'est terminé\" or \"Voilà, c'est fait\" until all requested deliverables are done. If the user says \"continue\", \"la suite\", or \"and the rest\", resume the project in progress (use memory_search for project context if available) and continue without saying \"terminé\" until the full scope is delivered. For project-like work, use memory_store to save project state (objective, steps done, deliverables) after each significant progress, with source project:<name> so context is reloaded on the next message. For multi-step tasks, use TOOL: write_todos for the initial plan (or full replan only); use TOOL: merge_todos to add steps without wiping the list; use read_todos/update_todo to mark progress. If the user message is prefixed with a block [Plan de la tâche — à respecter], execute the \"Prochaine étape\" (next pending step) before broad replanning.\n\
             {}\
             If you need no tool, reply normally with your answer.\n\
             If write_file or read_file returns \"path not allowed by policy\" or \"denied\", tell the user that they CAN configure this: edit the file tools_policy.yaml \
             (in the Akasha data directory) and add path prefixes under allowed_write_paths or allowed_read_paths. It is not impossible — the user controls this YAML file.",
            base, skills_part, run_command_os_rule, skills_rule
        )
        }
        }
    } else {
        String::new()
    };

    // Build prompt: system (rules + role + personality) vs user (reminder + memory + turns + message).
    let data_dir = store_path.parent().unwrap_or_else(|| store_path.as_ref());
    let agent_profile = match &agent_profile_cache {
        Some(cache) => get_or_load_agent_profile(data_dir, cache).await,
        None => AgentProfile::load(data_dir),
    };
    let completion_temperature = agent_profile
        .temperature
        .filter(|t| *t >= 0.0 && *t <= 2.0)
        .map(|t| t as f32)
        .unwrap_or(0.7);
    let profile_block = if code_studio_disk_task || embedded_compact {
        String::new()
    } else {
        crate::personality::build_personality_prompt(
            spec_dir.as_path(),
            &agent_profile,
            Some(&assigned_agent),
        )
    };
    let os_env_block = match std::env::consts::OS {
        "windows" => "[Environment] The daemon runs on Windows. For run_command, prefer cmd, PowerShell, curl.exe; avoid Unix-only commands (grep, cat, sed) that are not in the default PATH (except WSL).\n\n",
        _ => "[Environment] The daemon runs on Linux/macOS. You can use usual Unix commands (curl, grep, etc.).\n\n",
    };
    let mut system_prompt = String::with_capacity(8192);
    if code_studio_disk_task {
        system_prompt.push_str(CODE_STUDIO_APP_CONTEXT);
    } else if embedded_compact {
        system_prompt.push_str(EMBEDDED_APP_CONTEXT);
    } else {
        system_prompt.push_str(APP_CONTEXT);
    }
    if !embedded_compact {
        system_prompt.push_str(os_env_block);
    }
    if let Some(role_prompt) = agent_role_system_prompt(role_agent_for_system_prompt) {
        system_prompt.push_str("[Role]\n");
        if embedded_compact {
            let short: String = role_prompt.chars().take(800).collect();
            system_prompt.push_str(&short);
            if role_prompt.len() > 800 {
                system_prompt.push_str("…");
            }
        } else {
            system_prompt.push_str(role_prompt);
        }
        let studio_impl_writes_expected = code_studio_disk_task
            && matches!(
                role_agent_for_system_prompt,
                "studio_scaffold" | "studio_frontend" | "studio_backend" | "studio_fullstack"
            );
        if studio_impl_writes_expected {
            system_prompt.push_str(
                "\n\n[Code Studio — application des corrections]\n\
                - Tu dois appliquer les modifications toi-même via des lignes `TOOL:` (`search_replace`, `edit_file`, `write_file`, `apply_patch`) sur `workspace:/…` lorsque ces outils sont autorisés.\n\
                - Ne remplace pas une exécution d’outil par un long « contenu corrigé à mettre dans le fichier » dans le chat : le livrable attendu est l’écriture sur disque.\n\
                - Si un outil d’écriture échoue ou est refusé par la politique : explique **pourquoi** tu ne peux pas l’appliquer toi-même (message d’erreur ou règle), puis seulement propose une alternative manuelle.\n",
            );
        }
        system_prompt.push_str("\n\n");
    }
    if code_studio_disk_task {
        system_prompt.push_str(
            "[Code Studio — ton]\n\
             Assistant technique pour ce dépôt : concision, pas de digressions sur l’UI Akasha ; tutoyer en français si l’utilisateur écrit en français.\n\n",
        );
    } else if !profile_block.is_empty() {
        system_prompt.push_str(&profile_block);
    }
    // Enforce language and personality so the model does not switch language (e.g. when tool output is in English).
    if code_studio_disk_task {
        system_prompt.push_str(
            "\n\n[Response]\n\
            - Langue : répondre uniquement dans la même langue que le message utilisateur.\n\
            - Fichiers : respecter les règles Code Studio du préfixe message (pas de prose dans le source ; pas de barres markdown ``` autour du contenu write_file).\n\
            - Corrections : quand tu corriges du code, le résultat doit passer par les outils sur le dépôt ; ne pas renvoyer l’utilisateur vers un copier-coller manuel comme action principale sans avoir tenté (et documenté) les outils.\n\n",
        );
    } else if embedded_compact {
        system_prompt.push_str(
            "\n\n[Response]\n\
            - Réponds uniquement à la question utilisateur (pas de récapitulatif des consignes).\n\
            - Même langue que le message utilisateur.\n\n",
        );
    } else {
        system_prompt.push_str(
            "\n\n[Response]\n\
            - Language: reply ONLY in the same language as the user's message. If the user writes in French, reply entirely in French; in English, in English. Do not adopt the language of tool results or context.\n\
            - Personality: always apply your identity (name), tone, and form of address as defined in [Agent profile and instructions] (including formality when set).\n\n",
        );
    }
    if !studio_disk_system_append.is_empty() {
        system_prompt.push_str(&studio_disk_system_append);
    }
    if !code_studio_system_tools_block.is_empty() {
        system_prompt.push_str("\n\n");
        system_prompt.push_str(&code_studio_system_tools_block);
    }
    let system_prompt: Option<String> = if system_prompt.trim().is_empty() {
        None
    } else {
        Some(system_prompt.trim_end().to_string())
    };

    let personality_reminder = if code_studio_disk_task || embedded_compact {
        String::new()
    } else {
        crate::personality::build_personality_reminder_line(
            spec_dir.as_path(),
            &agent_profile,
            Some(&assigned_agent),
        )
    };
    let mut user_prefix = String::with_capacity(8192);
    user_prefix.push_str(&personality_reminder);
    if embedded_compact {
        user_prefix.push_str(
            "Réponds à la question ci-dessous (même langue). Ne répète pas les consignes système.\n\n",
        );
    } else {
        user_prefix.push_str(if code_studio_disk_task {
            "Réponds dans la même langue que le message utilisateur ci-dessous.\n\n"
        } else {
            "Reply in the same language as the user message below (French, English, etc.).\n\n"
        });
    }
    if !is_small_talk_fast_lane && !code_studio_disk_task && !embedded_compact {
        user_prefix.push_str(&crate::agents::current_date_context_block(chrono::Local::now()));
        if let Some(hint) = crate::agents::calendar_tools_hint_if_relevant(clean_message) {
            user_prefix.push_str(&hint);
        }
    }
    if let Some(ref am) = autonomous_mission {
        let g = am.read().await;
        if g.enabled && g.status == MissionStatusYaml::Active && session_id == g.session_id {
            user_prefix.push_str(
                "\n\n[Autonomous mission mode — do not ask the user questions]\n\
                - Do NOT ask clarifying questions unless a hard blocker remains (missing vault credentials, or tools_policy denies the action).\n\
                - Prefer tools (read_file, write_file, memory_store, web_search, run_command) and record decisions in markdown under the mission report directory.\n",
            );
            if !g.operating_rules.trim().is_empty() {
                user_prefix.push_str("- Mission operating rules:\n");
                for line in g.operating_rules.lines().take(32) {
                    user_prefix.push_str("  ");
                    user_prefix.push_str(line);
                    user_prefix.push('\n');
                }
                user_prefix.push('\n');
            }
        }
    }
    let turns_empty = match &short_term {
        Some(st) => st.get_turns(&session_id).await.is_empty(),
        None => true,
    };
    let process_id_for_recall = resolve_root_task_id(store_path.as_path(), task_id)
        .map(|id| id.to_string())
        .unwrap_or_else(|| task_id.to_string());
    let user_profile = UserProfile::load(data_dir);
    let user_identity_prefix = user_profile.format_for_prompt();
    let constitution = crate::constitution::Constitution::load(data_dir);
    let mut recall_params = crate::memory_orchestrator::RecallParams {
        message: message.clone(),
        session_id: session_id.clone(),
        semantic_top_k: memory_profile.semantic_top_k,
        episodic_limit: memory_profile.episodic_limit,
        facts_limit: memory_profile.facts_limit,
        filter_by_session: !turns_empty,
        suggest_project: memory_profile.allow_project_recall
            && message_suggests_project(clean_message),
        // Code Studio: do not run global LT search for "user name" on first turn — it pulls unrelated memories.
        is_first_message: turns_empty
            && memory_profile.allow_identity_lookup
            && !code_studio_disk_task,
        expand_by_graph: memory_profile.expand_by_graph,
        graph_expand_hops: memory_profile.graph_expand_hops,
        process_id: Some(process_id_for_recall),
        task_id: Some(task_id.to_string()),
        user_identity_prefix: if user_identity_prefix.is_empty()
            || !memory_profile.allow_identity_lookup
        {
            None
        } else {
            Some(user_identity_prefix)
        },
        task_outcomes_limit: if incognito {
            0
        } else if code_studio_disk_task {
            6
        } else {
            8
        },
        task_outcomes_scope_session: code_studio_disk_task || !turns_empty,
        include_preference_and_personality_episodic: !code_studio_disk_task,
        constitution: if !embedded_compact && constitution.is_configured() {
            Some(constitution)
        } else {
            None
        },
        ..Default::default()
    };
    if memory_profile.semantic_top_k > 0
        && !is_small_talk_fast_lane
        && !code_studio_disk_task
    {
        let _ = bus.send(
            EventEnvelope::new(
                EventType::ProgressUpdate,
                Some(serde_json::json!({
                    "task_id": task_id.to_string(),
                    "progress_pct": 6,
                    "message": "Préparation du contexte…"
                })),
            )
            .with_correlation(task_id),
        );
        let mut search_queries =
            crate::memory_retrieval_enhance::expand_queries(&llm_router, &message).await;
        if let Some(hyde) =
            crate::memory_retrieval_enhance::hyde_document(&llm_router, &message).await
        {
            search_queries.push(hyde);
        }
        recall_params.search_queries = search_queries;
    }
    let mut workspace_registry_lines: Vec<String> = Vec::new();
    if memory_profile.workspace_graph_top_k > 0 {
        let graph_query = message.clone();
        let graph_k = memory_profile.workspace_graph_top_k;
        let sp = store_path.clone();
        if let Some((workspace_lines, graph_lines)) = tokio::task::spawn_blocking(move || {
            let store = WorkspaceGraphStore::open(&sp)?;
            let workspaces = store.list_workspaces()?;
            let workspace_lines: Vec<String> = workspaces
                .iter()
                .map(|w| format!("- \"{}\" — id {} — {}", w.name, w.id, w.root_path))
                .collect();
            let graph_lines = store.search_graph_context(&graph_query, graph_k, None)?;
            Ok::<_, anyhow::Error>((workspace_lines, graph_lines))
        })
        .await
        .ok()
        .and_then(|r| r.ok())
        {
            workspace_registry_lines = workspace_lines;
            recall_params.workspace_graph_lines = graph_lines;
        }
    }
    if memory_profile.semantic_top_k > 0
        || memory_profile.episodic_limit > 0
        || memory_profile.facts_limit > 0
        || recall_params.user_identity_prefix.is_some()
        || recall_params.task_outcomes_limit > 0
        || !recall_params.workspace_graph_lines.is_empty()
    {
        let _ = bus.send(
            EventEnvelope::new(
                EventType::ProgressUpdate,
                Some(serde_json::json!({
                    "task_id": task_id.to_string(),
                    "progress_pct": 8,
                    "message": "Chargement du contexte et de la mémoire…"
                })),
            )
            .with_correlation(task_id),
        );
        let recall_timeout = std::time::Duration::from_secs(memory_recall_timeout_secs());
        insert_task_tracking_event(
            store_path.as_path(),
            task_id,
            "memory_recall_started",
            serde_json::json!({
                "timeout_secs": recall_timeout.as_secs(),
                "semantic_top_k": memory_profile.semantic_top_k,
            }),
        );
        let (fused, recall_timed_out) = match tokio::time::timeout(
            recall_timeout,
            crate::memory_orchestrator::recall_context(long_term_client.as_ref(), recall_params),
        )
        .await
        {
            Ok(ctx) => (ctx, false),
            Err(_) => {
                tracing::warn!(
                    task_id = %task_id,
                    timeout_secs = recall_timeout.as_secs(),
                    "memory recall timed out; continuing without long-term context"
                );
                (
                    crate::memory_orchestrator::FusedMemoryContext::default(),
                    true,
                )
            }
        };
        insert_task_tracking_event(
            store_path.as_path(),
            task_id,
            "memory_recall_finished",
            serde_json::json!({
                "had_results": !fused.to_context_string().is_empty(),
                "timed_out": recall_timed_out,
            }),
        );
        let fused_str = fused.to_context_string();
        if !fused_str.is_empty() {
            user_prefix.push_str(&fused_str);
        }
        let recall_had_results = !fused_str.is_empty();
        crate::memory_maintenance::schedule_post_retrieval(
            long_term_client.clone(),
            message.clone(),
            Some(session_id.clone()),
            recall_had_results,
        );
    }
    if memory_profile.user_rag_top_k > 0 {
        let user_rag_store = crate::user_rag::UserRagStore::new(data_dir);
        let rag_query = message.clone();
        let rag_top_k = memory_profile.user_rag_top_k;
        let data_dir_rag = data_dir.to_path_buf();
        let chunks =
            tokio::task::spawn_blocking(move || {
                #[cfg(any(feature = "embeddings", feature = "embeddings-tract"))]
                let query_embedding = {
                    use akasha_embeddings::Embedder;
                    let cache_dir = data_dir_rag.join("embedding_model");
                    Embedder::new(&cache_dir).embed_one(&rag_query).ok()
                };
                #[cfg(not(any(feature = "embeddings", feature = "embeddings-tract")))]
                let query_embedding: Option<Vec<f32>> = None;
                user_rag_store.retrieve_hybrid(&rag_query, query_embedding.as_deref(), rag_top_k)
            })
                .await
                .ok()
                .and_then(|res| res.ok())
                .unwrap_or_default();
        if !chunks.is_empty() {
            user_prefix.push_str("[User documents — use these excerpts if relevant to answer]\n");
            for c in &chunks {
                user_prefix.push_str("- ");
                user_prefix.push_str(&c.replace('\n', " "));
                user_prefix.push_str("\n");
            }
            user_prefix.push_str("\n");
        }
    }
    if workspace_registry_lines.len() > 1 {
        const MAX_WORKSPACE_LINES: usize = 8;
        user_prefix.push_str(
            "[Project knowledge graphs — registered workspaces (use id with workspace_graph_search --workspace)]\n",
        );
        for line in workspace_registry_lines.iter().take(MAX_WORKSPACE_LINES) {
            user_prefix.push_str(line);
            user_prefix.push_str("\n");
        }
        if workspace_registry_lines.len() > MAX_WORKSPACE_LINES {
            user_prefix.push_str(&format!(
                "- … ({} more workspaces not shown)\n",
                workspace_registry_lines.len() - MAX_WORKSPACE_LINES
            ));
        }
        user_prefix.push_str(
            "To fetch more symbols or files from the index, call: workspace_graph_search <keywords> [--workspace <id>]\n\n",
        );
    }
    let router_task_type_for_compact = if preferred_task_type_override.as_deref()
        == Some("image_generation")
        || assigned_agent == "image_generation"
    {
        llm_router.resolve_task_type_for_agent("conversation")
    } else {
        preferred_task_type_override
            .clone()
            .unwrap_or_else(|| llm_router.resolve_task_type_for_agent(&assigned_agent))
    };
    let (tok_prov, tok_model) = llm_router
        .primary_route_for_task_type(&router_task_type_for_compact)
        .unwrap_or_else(|| ("default".to_string(), "default".to_string()));
    if let Some(ref st) = short_term {
        if memory_profile.compact_before_prompt && !incognito {
            let new_msg_tokens = ShortTermStore::estimate_tokens_calibrated(
                tok_prov.as_str(),
                tok_model.as_str(),
                &message,
            );
            compact_short_term_if_needed(
                st,
                &session_id,
                &llm_router,
                new_msg_tokens,
                long_term_client.as_ref(),
                tok_prov.as_str(),
                tok_model.as_str(),
                &store_path,
            )
            .await;
        }
        let turns = st.get_turns(&session_id).await;
        let final_turns: Vec<_> = turns
            .iter()
            .rev()
            .take(memory_profile.recent_turns_limit)
            .cloned()
            .rev()
            .collect();
        let short_ctx = ShortTermStore::turns_to_context(&final_turns);
        if !short_ctx.is_empty() {
            let capped = if memory_profile.recent_context_max_chars > 0
                && short_ctx.chars().count() > memory_profile.recent_context_max_chars
            {
                short_ctx
                    .chars()
                    .take(memory_profile.recent_context_max_chars)
                    .collect::<String>()
                    + "…"
            } else {
                short_ctx
            };
            if !capped.is_empty() {
                user_prefix.push_str("[Recent context (this session)]\n");
                user_prefix.push_str(capped.trim_end());
                user_prefix.push_str("\n\n");
            }
        }
    }
    let store = match TaskStore::open(&store_path) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(task_id = %task_id, error_kind = "store_open", error = %e, "LLM task: store open failed before prompt assembly");
            if let Some(ref sq) = steering_queue {
                sq.unregister_active(task_id).await;
            }
            notify_task_completion(&task_completion_registry, task_id).await;
            return;
        }
    };
    if !is_small_talk_fast_lane {
        if let Ok(todos) = store.get_todos(task_id) {
            if let Some(block) = format_todos_plan_block(&todos) {
                user_prefix.push_str(&block);
                user_prefix.push_str("\n");
            }
        }
    }
    if is_small_talk_fast_lane {
        user_prefix.push_str(
            "\n[Brief small-talk only: reply in 1–3 short sentences. Do not use tools. \
             Follow your identity, personality, and traits from the system instructions.]\n",
        );
    }
    // IMPORTANT: intent classification must use the clean user message (without guardrail/prefix
    // injections). Using the raw `message` can falsely trigger intents (e.g. transport/maps)
    // from injected context blocks and activate unrelated plugin routing.
    let intent_flags = compute_message_intent_flags(clean_message);
    let plugin_catalog_reminder = if code_studio_disk_task
        || is_small_talk_fast_lane
        || plugin_registry.is_none()
    {
        String::new()
    } else {
        match plugin_registry.as_ref() {
            None => String::new(),
            Some(reg) => {
                let all_tool: Vec<PluginManifest> = reg
                    .manifests()
                    .into_iter()
                    .filter(|m| m.kind == PluginKind::Tool)
                    .collect();
                if all_tool.is_empty() {
                    String::new()
                } else {
                    let selected_ids = crate::plugins::selection::select_relevant_plugins_via_llm(
                        llm_router.as_ref(),
                        clean_message,
                        &all_tool,
                    )
                    .await;
                    let id_lower: std::collections::HashSet<String> = selected_ids
                        .iter()
                        .map(|s| s.to_lowercase())
                        .collect();
                    let mut picked: Vec<PluginManifest> = all_tool
                        .iter()
                        .filter(|m| id_lower.contains(&m.id.to_lowercase()))
                        .cloned()
                        .collect();
                    if picked.is_empty() {
                        picked.clone_from(&all_tool);
                    }
                    crate::plugins::selection::build_plugin_catalog_block(&picked)
                }
            }
        }
    };
    let write_reminder = if intent_flags.save_file {
        WRITE_FILE_REMINDER
    } else {
        ""
    };
    // web_search is available when allowed by profile, enabled in policy, and a provider chain exists
    // (Brave/Tavily/Serper/Google PSE with keys, or keyless SearXNG + DuckDuckGo).
    let web_search_effectively_available = tools_executor_snapshot
        .as_ref()
        .map(|e| {
            e.policy.can_use_tool("web_search")
                && akasha_tools::any_provider_available(&e.policy)
        })
        .unwrap_or(false);
    let web_search_reminder: &str = if intent_flags.external_info {
        if web_search_effectively_available {
            // web_search is available: instruct the model to use it.
            WEB_SEARCH_REMINDER
        } else {
            // web_search is absent (not allowed, not enabled, or no API key): prevent the model
            // from ignoring the question and returning a generic capability introduction.
            WEB_SEARCH_UNAVAILABLE_REMINDER
        }
    } else {
        ""
    };
    let web_search_followup_reminder: &str = if intent_flags.external_info
        && web_search_effectively_available
        && tools_executor_snapshot
            .as_ref()
            .map(|e| web_followup_tools_configured(&e.policy))
            .unwrap_or(false)
    {
        WEB_SEARCH_FOLLOWUP_REMINDER
    } else {
        ""
    };
    let transport_reminder: &str = if intent_flags.transport {
        // Always inject a transport reminder so the model cannot mistake a travel question
        // for a file-creation or project task (e.g. "Quel est le chemin complet du fichier").
        // Use the full reminder when web_search is available; use the no-search fallback otherwise.
        let has_web_search = web_search_effectively_available;
        if has_web_search {
            TRANSPORT_REMINDER
        } else {
            TRANSPORT_REMINDER_NO_SEARCH
        }
    } else {
        ""
    };
    let geolocation_distance_reminder: &str = if intent_flags.geolocation_distance {
        let has_any_tool = tools_executor_snapshot
            .as_ref()
            .map(|e| e.policy.can_use_tool("web_search") || e.policy.can_use_tool("plugin.call"))
            .unwrap_or(false);
        tracing::info!(
            task_id = %task_id,
            has_any_tool,
            "geolocation-distance intent detected; applying generic fallback guardrail"
        );
        if has_any_tool {
            GEO_DISTANCE_REMINDER_WITH_TOOLS
        } else {
            GEO_DISTANCE_REMINDER_NO_TOOL
        }
    } else {
        ""
    };
    let social_feed_reminder = if intent_flags.social_feed_fetch
        && tools_executor_snapshot.as_ref().map_or(false, |e| {
            e.policy.can_use_tool("web_search") || e.policy.can_use_tool("browser")
        }) {
        SOCIAL_FEED_REMINDER
    } else {
        ""
    };
    let device_camera_reminder = if intent_flags.camera_or_mic
        && tools_executor_snapshot
            .as_ref()
            .map(|e| e.policy.can_use_device_interface("local_media"))
            .unwrap_or(false)
    {
        DEVICE_CAMERA_REMINDER
    } else {
        ""
    };
    let image_generation_reminder = if intent_flags.image_generation
        || preferred_task_type_override.as_deref() == Some("image_generation")
    {
        IMAGE_GENERATION_REMINDER
    } else {
        ""
    };
    let github_vault_reminder = if intent_flags.github_with_vault
        && tools_executor_snapshot
            .as_ref()
            .map(|e| e.policy.can_use_tool("run_command"))
            .unwrap_or(false)
    {
        GITHUB_VAULT_REMINDER
    } else {
        ""
    };
    let code_dev_sandbox_reminder = if intent_flags.code_generation {
        CODE_DEV_SANDBOX_REMINDER
    } else {
        ""
    };
    // When user clearly wants a photo from camera, prefix the message with an imperative so the model responds with device_invoke directly (no ask_user).
    let mut user_message = if !device_camera_reminder.is_empty() {
        format!(
            "[Répondre par: TOOL: device_discover local_media puis TOOL: device_invoke local_media camera capture. Ne pas utiliser ask_user.]\n\n{}",
            clean_message
        )
    } else {
        clean_message.to_string()
    };
    let mut current_prompt = if user_prefix.trim().is_empty() {
        format!(
            "{0}{1}{2}{3}{4}{5}{6}{7}{8}{9}{10}{11}User:\n{12}",
            guardrail_reminder_block,
            write_reminder,
            web_search_reminder,
            web_search_followup_reminder,
            transport_reminder,
            geolocation_distance_reminder,
            plugin_catalog_reminder,
            social_feed_reminder,
            device_camera_reminder,
            image_generation_reminder,
            github_vault_reminder,
            code_dev_sandbox_reminder,
            user_message
        )
    } else {
        format!(
            "{0}{1}{2}{3}{4}{5}{6}{7}{8}{9}{10}{11}{12}User:\n{13}",
            user_prefix.trim_end(),
            guardrail_reminder_block,
            write_reminder,
            web_search_reminder,
            web_search_followup_reminder,
            transport_reminder,
            geolocation_distance_reminder,
            plugin_catalog_reminder,
            social_feed_reminder,
            device_camera_reminder,
            image_generation_reminder,
            github_vault_reminder,
            code_dev_sandbox_reminder,
            user_message
        )
    };
    let mut reply_text = String::new();
    let mut last_llm_model_used: Option<String> = None;
    let mut first_meaningful_progress_sent = false;

    if let Some(intent) = session_recall_intent {
        tracing::debug!(?intent, "[RECALL_LOAD] loading recall turns");
        let recall_turns = match intent.range {
            SessionRecallRange::Yesterday => {
                tracing::debug!("[RECALL_LOAD] reading YESTERDAY data");
                if short_term.is_some() {
                    let short_term_dir = data_dir.join("short_term");
                    let yesterday = chrono::Utc::now() - chrono::Duration::days(1);
                    let sid = format!("day-{}", yesterday.format("%Y-%m-%d"));
                    tracing::debug!(session_id = %sid, "[RECALL_LOAD] yesterday session_id");
                    let turns =
                        crate::memory::ShortTermStore::read_day_from_disk(&sid, &short_term_dir)
                            .unwrap_or_default();
                    tracing::debug!(
                        count = turns.len(),
                        "[RECALL_LOAD] read turns from yesterday"
                    );
                    turns
                } else {
                    tracing::debug!("[RECALL_LOAD] short_term is None, returning empty");
                    Vec::new()
                }
            }
            SessionRecallRange::CurrentDay => {
                tracing::debug!("[RECALL_LOAD] reading CURRENT_DAY data");
                if let Some(st) = short_term.as_ref() {
                    let turns = st.get_turns(&session_id).await;
                    tracing::debug!(
                        count = turns.len(),
                        "[RECALL_LOAD] read turns from current day"
                    );
                    turns
                } else {
                    tracing::debug!("[RECALL_LOAD] short_term is None, returning empty");
                    Vec::new()
                }
            }
        };
        tracing::debug!(count = recall_turns.len(), "[RECALL_BUILD] building recap");
        reply_text = build_session_recap_reply(&recall_turns, intent).unwrap_or_else(|| {
            match intent.language {
                SmallTalkLanguage::French => "Je n'ai pas encore de résumé fiable à te partager pour cette période. Si tu veux, je peux te faire un récap dès qu'on a un peu plus d'historique utile.".to_string(),
                SmallTalkLanguage::English => "I don't have a reliable recap for that period yet. If you want, I can provide one as soon as we have a bit more useful history.".to_string(),
            }
        });
        first_meaningful_progress_sent = true;
        meaningful_progress_flag.store(true, std::sync::atomic::Ordering::Relaxed);
        cancel_task_watchdogs(&mut watchdog_cancel, &mut stall_cancel);
        if emit_timeline_once_for_task(
            &bus,
            Some(store_path.as_path()),
            task_id,
            "first_meaningful_progress",
            Some(serde_json::json!({ "source": "session_recap_fast_path" })),
        ) {
            log_latency_metric(store_path.as_path(), task_id, "ttfr_ms");
        }
    } else {
        let _ = bus.send(
            EventEnvelope::new(
                EventType::ProgressUpdate,
                Some(serde_json::json!({
                    "task_id": task_id.to_string(),
                    "progress_pct": 12,
                    "message": "Génération de la réponse…"
                })),
            )
            .with_correlation(task_id),
        );
        let mut max_tool_rounds = std::env::var("AKASHA_MAX_TOOL_ROUNDS")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(10);
        // Orchestrated subtasks / remediation: extra tool rounds (models often read/search first).
        if orch_disk_deliverables {
            let floor = std::env::var("AKASHA_MAX_TOOL_ROUNDS_ORCH_DELIVERABLES")
                .ok()
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(24);
            if max_tool_rounds < floor {
                max_tool_rounds = floor;
            }
        }
        let mut round = 0u32;
        // Tours d'affilée avec uniquement des outils d'exploration (Code Studio).
        let mut studio_read_only_streak_rounds: u32 = 0;
        let mut social_snapshot_seen = false;
        // Loop detection history is tracked per "agent key".
        // For now, agent key = current task_id (sub-agent tasks each have their own task_id).
        let mut tool_loop_history_by_agent: std::collections::HashMap<
            String,
            Vec<(String, String)>,
        > = std::collections::HashMap::new();
        let loop_agent_key = task_id.to_string();
        let mut last_tool_results_blob: Option<String> = None;
        let mut force_synthesis_attempted = false;
        let mut meta_response_retry_count = 0u32;
        let mut small_talk_off_topic_retries = 0u32;
        // Per-agent (agent key = task_id) set of paths where a full read_file was truncated.
        // Used to hard-block repeated full-file reads and force chunked/windowed reads.
        let mut truncated_read_file_paths_by_agent: std::collections::HashMap<
            String,
            std::collections::HashSet<String>,
        > = std::collections::HashMap::new();
        // Orchestrated deliverables: re-prompts when the model returns no parseable TOOL lines.
        let mut orch_disk_write_nags = 0u32;
        // Code Studio implementation agents: prevent "copy/paste this file" fallback
        // when write tools are available but unused.
        let mut studio_manual_patch_nags = 0u32;
        let mut studio_prose_only_write_nags = 0u32;
        let mut studio_zero_tool_promise_nags = 0u32;
        let mut studio_unparsed_tool_marker_nags = 0u32;
        let mut studio_pm_mandatory_delegate_nags = 0u32;
        let mut last_captured_image_base64: Option<String> = None;
        // Images from tools (browser screenshot, camera, generate_image) queued for the next CompletionRequest for multimodal models.
        let mut pending_completion_image_urls: Option<Vec<String>> = None;

        let llm_timeout_secs = std::env::var("AKASHA_LLM_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or_else(|| llm_router.default_timeout_secs());
        let idle_timeout_secs = std::env::var("AKASHA_LLM_STREAM_IDLE_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(if embedded_compact { 180 } else { 60 });
        // First chunk can take long (model load, first token on CPU). Use longer wait so we don't hit idle before any data.
        let first_chunk_timeout_secs = std::env::var("AKASHA_LLM_FIRST_CHUNK_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or_else(|| {
                if embedded_compact {
                    llm_timeout_secs.max(600)
                } else {
                    llm_timeout_secs.min(300)
                }
            });

        'tool_rounds: loop {
            match store.get(task_id).ok().flatten().map(|t| t.status) {
                Some(TaskStatus::Paused)
                | Some(TaskStatus::Cancelled)
                | Some(TaskStatus::Failed) => {
                    break 'tool_rounds;
                }
                _ => {}
            }
            if let Some(ref sq) = steering_queue {
                let steering_items = sq.drain_steering(task_id).await;
                for item in steering_items {
                    let steer_text = format!(
                        "[Steering — instruction utilisateur à appliquer maintenant]\n{}",
                        item.text
                    );
                    if let Some(st) = short_term.as_ref() {
                        st.append(&session_id, "user", steer_text.clone()).await;
                    }
                    user_message = format!("{user_message}\n\n{steer_text}");
                    let _ = store.insert_event(
                        task_id,
                        "user_steering_applied",
                        Some(&serde_json::json!({
                            "queue_id": item.id,
                            "preview": item.text.chars().take(200).collect::<String>(),
                            "schema_version": 1
                        })),
                        &chrono::Utc::now().to_rfc3339(),
                    );
                    let _ = bus.send(
                        EventEnvelope::new(
                            EventType::ProgressUpdate,
                            Some(serde_json::json!({
                                "message": format!("[Steering] {}", item.text.chars().take(120).collect::<String>()),
                                "task_id": task_id.to_string()
                            })),
                        )
                        .with_correlation(task_id),
                    );
                }
            }
            // Quota: stop task if session cost or tokens exceed configured limits (Phase 2.3).
            if let Some(ref store) = task_usage_store {
                let (session_tokens, session_cost) =
                    store.get_session(&session_id).await.unwrap_or((0, 0.0));
                if let Some(max_cost) = std::env::var("AKASHA_MAX_COST_PER_SESSION_USD")
                    .ok()
                    .and_then(|s| s.parse::<f64>().ok())
                {
                    if max_cost > 0.0 && session_cost >= max_cost {
                        reply_text = "Budget dépassé pour cette session (AKASHA_MAX_COST_PER_SESSION_USD). Démarrez une nouvelle session ou augmentez le plafond.".to_string();
                        break 'tool_rounds;
                    }
                }
                if let Some(max_tokens) = std::env::var("AKASHA_MAX_TOKENS_PER_SESSION")
                    .ok()
                    .and_then(|s| s.parse::<u64>().ok())
                {
                    if max_tokens > 0 && session_tokens >= max_tokens {
                        reply_text = "Quota de tokens dépassé pour cette session (AKASHA_MAX_TOKENS_PER_SESSION). Démarrez une nouvelle session ou augmentez le plafond.".to_string();
                        break 'tool_rounds;
                    }
                }
            }
            // `task_types.image_generation` in llm_router.yaml configures the *pixel backend* for the
            // `generate_image` tool (see image_generation.rs). Routing chat completion to that task
            // type sends image-only models (e.g. Ollama z-image) through the text completion path, which
            // expects a `response` string — those models return images/empty text and trigger fallback warnings.
            // Here we always use a normal text route for the LLM turn; the tool call still uses image_generation config.
            let router_task_type_for_llm = if preferred_task_type_override.as_deref()
                == Some("image_generation")
                || assigned_agent == "image_generation"
            {
                llm_router.resolve_task_type_for_agent("conversation")
            } else {
                preferred_task_type_override
                    .clone()
                    .unwrap_or_else(|| llm_router.resolve_task_type_for_agent(&assigned_agent))
            };
            let preferred_task_type = Some(router_task_type_for_llm);
            let mut merged_image_urls: Vec<String> = Vec::new();
            if tool_loop_history_by_agent
                .get(&loop_agent_key)
                .map(|v| v.is_empty())
                .unwrap_or(true)
            {
                if let Some(ref u) = image_data_urls {
                    merged_image_urls.extend(u.iter().cloned());
                }
            }
            if let Some(mut pending) = pending_completion_image_urls.take() {
                merged_image_urls.append(&mut pending);
            }
            let merged_image_data_urls = if merged_image_urls.is_empty() {
                None
            } else {
                Some(merged_image_urls)
            };
            if let Some((provider, model)) = preferred_task_type
                .as_deref()
                .and_then(|tt| llm_router.primary_route_for_task_type(tt))
            {
                insert_task_tracking_event(
                    store_path.as_path(),
                    task_id,
                    "llm_call_started",
                    serde_json::json!({
                        "round": round + 1,
                        "task_type": preferred_task_type,
                        "provider": provider,
                        "model": model,
                    }),
                );
                // Embedded load + first token can exceed the default stall watchdog (180s) without stream chunks.
                if provider == "akasha_embedded" || provider == "akasha_core" {
                    meaningful_progress_flag.store(true, std::sync::atomic::Ordering::Relaxed);
                    let _ = bus.send(
                        EventEnvelope::new(
                            EventType::ProgressUpdate,
                            Some(serde_json::json!({
                                "task_id": task_id.to_string(),
                                "progress_pct": 15,
                                "message": "Modèle embarqué — chargement / premier token (peut prendre 1–3 min)…"
                            })),
                        )
                        .with_correlation(task_id),
                    );
                }
            }
            let request = CompletionRequest {
                prompt: format!("{}{}", current_prompt, tool_instruction),
                max_tokens: Some(completion_max_tokens),
                temperature: Some(completion_temperature),
                preferred_task_type,
                system_prompt: system_prompt.clone(),
                image_data_urls: merged_image_data_urls,
                top_p: None,
                top_k: None,
                frequency_penalty: None,
                presence_penalty: None,
                repeat_penalty: None,
                num_ctx: None,
                num_gpu: None,
                thinking_level: completion_thinking_level.clone(),
            };
            // Streaming path: single forwarder thread → tokio channel (avoids spawn_blocking per chunk).
            // Overall deadline bounds the full generation; idle timeout bounds inter-chunk wait.
            let (stream_tx, std_rx) = std::sync::mpsc::channel::<String>();
            let (tok_tx, mut tok_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            std::thread::Builder::new()
                .name("akasha-stream-fwd".to_string())
                .spawn(move || {
                    for chunk in std_rx {
                        if tok_tx.send(chunk).is_err() {
                            break;
                        }
                    }
                })
                .ok();
            let router = llm_router.clone();
            let stream_join =
                tokio::spawn(async move { router.complete_stream(&request, stream_tx).await });
            let mut accumulated = String::new();
            let mut first_wait = true;
            let overall_deadline =
                tokio::time::Instant::now() + std::time::Duration::from_secs(llm_timeout_secs);
            loop {
                // Check the overall deadline before waiting for a chunk to avoid spurious zero-duration timeouts.
                if tokio::time::Instant::now() >= overall_deadline {
                    tracing::warn!(
                        timeout_secs = llm_timeout_secs,
                        "Overall LLM timeout exceeded; aborting task"
                    );
                    stream_join.abort();
                    reply_text = if accumulated.is_empty() {
                        format!("LLM response timed out after {} seconds.", llm_timeout_secs)
                    } else {
                        accumulated
                    };
                    break 'tool_rounds;
                }
                let idle = if first_wait {
                    first_wait = false;
                    std::time::Duration::from_secs(first_chunk_timeout_secs)
                } else {
                    std::time::Duration::from_secs(idle_timeout_secs)
                };
                match tokio::time::timeout(idle, tok_rx.recv()).await {
                    Ok(Some(chunk)) => {
                        if !first_meaningful_progress_sent && !chunk.trim().is_empty() {
                            first_meaningful_progress_sent = true;
                            meaningful_progress_flag
                                .store(true, std::sync::atomic::Ordering::Relaxed);
                            cancel_task_watchdogs(&mut watchdog_cancel, &mut stall_cancel);
                            if emit_timeline_once_for_task(
                                &bus,
                                Some(store_path.as_path()),
                                task_id,
                                "first_meaningful_progress",
                                Some(serde_json::json!({ "source": "stream_chunk" })),
                            ) {
                                log_latency_metric(store_path.as_path(), task_id, "ttfr_ms");
                            }
                        }
                        const MAX_ACCUMULATED: usize = 2 * 1024 * 1024; // 2 MiB cap to prevent unbounded allocation on long streams
                        let chunk_ref: &str = if chunk.len() > MAX_ACCUMULATED {
                            tracing::warn!(
                                chunk_len = chunk.len(),
                                max = MAX_ACCUMULATED,
                                "Stream chunk larger than progress cap; truncating for accumulated progress buffer"
                            );
                            let mut end = MAX_ACCUMULATED;
                            while end > 0 && !chunk.is_char_boundary(end) {
                                end -= 1;
                            }
                            &chunk[..end]
                        } else {
                            chunk.as_str()
                        };
                        if accumulated.len() + chunk_ref.len() > MAX_ACCUMULATED {
                            let mut keep_len = MAX_ACCUMULATED.saturating_sub(chunk_ref.len());
                            while keep_len > 0 && !accumulated.is_char_boundary(keep_len) {
                                keep_len -= 1;
                            }
                            accumulated.truncate(keep_len);
                        }
                        accumulated.push_str(chunk_ref);
                        let _ = bus.send(
                            EventEnvelope::new(
                                EventType::ProgressUpdate,
                                Some(serde_json::json!({
                                    "task_id": task_id.to_string(),
                                    "progress_pct": 50,
                                    "message": accumulated
                                })),
                            )
                            .with_correlation(task_id),
                        );
                        if !chunk_ref.is_empty() {
                            let _ = store.insert_event(
                                task_id,
                                "assistant_text_delta",
                                Some(&serde_json::json!({
                                    "delta": chunk_ref,
                                    "schema_version": 1
                                })),
                                &chrono::Utc::now().to_rfc3339(),
                            );
                            if chunk_ref.contains("TOOL:") || chunk_ref.contains("<tool_call") {
                                let tool_name = parse_tool_name_from_toolcall_delta(chunk_ref);
                                let _ = bus.send(
                                    EventEnvelope::new(
                                        EventType::ProgressUpdate,
                                        Some(serde_json::json!({
                                            "task_id": task_id.to_string(),
                                            "event_type": "toolcall_delta",
                                            "delta": chunk_ref,
                                            "delta_raw": chunk_ref,
                                            "tool_name": tool_name
                                        })),
                                    )
                                    .with_correlation(task_id),
                                );
                                let _ = store.insert_event(
                                    task_id,
                                    "toolcall_delta",
                                    Some(&serde_json::json!({
                                        "delta": chunk_ref,
                                        "delta_raw": chunk_ref,
                                        "tool_name": tool_name,
                                        "schema_version": 1
                                    })),
                                    &chrono::Utc::now().to_rfc3339(),
                                );
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(_) => {
                        tracing::debug!(
                            idle_secs = idle_timeout_secs,
                            "Stream idle timeout, waiting for final response"
                        );
                        break;
                    }
                }
            }
            // Wrap stream_join.await with remaining overall budget; guard against zero remaining.
            let remaining = overall_deadline.saturating_duration_since(tokio::time::Instant::now());
            let response = if remaining.is_zero() {
                tracing::warn!(
                    timeout_secs = llm_timeout_secs,
                    "Overall LLM timeout on stream completion"
                );
                reply_text = if accumulated.is_empty() {
                    format!("LLM response timed out after {} seconds.", llm_timeout_secs)
                } else {
                    accumulated
                };
                break;
            } else {
                match tokio::time::timeout(remaining, stream_join).await {
                    Ok(Ok(Ok(resp))) => {
                        last_llm_model_used = Some(resp.model_used.clone());
                        insert_task_tracking_event(
                            store_path.as_path(),
                            task_id,
                            "llm_call_finished",
                            serde_json::json!({
                                "round": round + 1,
                                "success": true,
                                "model_used": resp.model_used,
                                "latency_ms": resp
                                    .total_duration_ns
                                    .map(|ns| ns / 1_000_000)
                                    .unwrap_or(0),
                                "prompt_tokens": resp.usage.as_ref().map(|u| u.prompt_tokens).unwrap_or(0),
                                "completion_tokens": resp
                                    .usage
                                    .as_ref()
                                    .map(|u| u.completion_tokens)
                                    .unwrap_or(0),
                            }),
                        );
                        if let Some(ref store) = task_usage_store {
                            let prompt_tokens =
                                resp.usage.as_ref().map(|u| u.prompt_tokens).unwrap_or(0);
                            let completion_tokens = resp
                                .usage
                                .as_ref()
                                .map(|u| u.completion_tokens)
                                .unwrap_or(0);
                            let cost = resp.cost_usd.unwrap_or(0.0);
                            let latency_ms = resp
                                .total_duration_ns
                                .map(|ns| ns / 1_000_000)
                                .unwrap_or(0);
                            store
                                .add(
                                    task_id,
                                    &session_id,
                                    prompt_tokens,
                                    completion_tokens,
                                    cost,
                                    latency_ms,
                                    Some(resp.model_used.as_str()),
                                )
                                .await;
                        }
                        resp.text.trim().to_string()
                    }
                    Ok(Ok(Err(e))) => {
                        tracing::warn!(error = %e, "LLM completion failed");
                        insert_task_tracking_event(
                            store_path.as_path(),
                            task_id,
                            "llm_call_finished",
                            serde_json::json!({
                                "round": round + 1,
                                "success": false,
                                "error": e.to_string(),
                            }),
                        );
                        reply_text = format!("Sorry, I couldn't get a response (error: {}).", e);
                        break;
                    }
                    Ok(Err(join_err)) => {
                        tracing::warn!(error = %join_err, "Stream task join failed");
                        reply_text = if accumulated.is_empty() {
                            format!("LLM task error: {}", join_err)
                        } else {
                            accumulated
                        };
                        break;
                    }
                    Err(_timeout) => {
                        tracing::warn!(
                            timeout_secs = llm_timeout_secs,
                            "Overall LLM timeout on stream completion"
                        );
                        reply_text = if accumulated.is_empty() {
                            format!("LLM response timed out after {} seconds.", llm_timeout_secs)
                        } else {
                            accumulated
                        };
                        break;
                    }
                }
            };
            // Some providers return the full text only in stream chunks while `resp.text` is empty, or drop `TOOL:` lines
            // from the final body. Parsing tools only from `resp.text` then skips execution entirely (user sees text, no disk writes).
            // Use `parse_tool_calls` (which normalizes sloppy prefixes like `- Tool:` / `**TOOL:**`) instead of a raw
            // `contains("TOOL:")` check so that any provider-specific formatting is handled consistently.
            let response =
                crate::api_llm_stream::choose_merged_response_for_tools(&response, &accumulated);

            if let Some(intent) = small_talk_intent {
                if response_looks_off_topic_for_small_talk(&response) {
                    if is_small_talk_fast_lane && small_talk_off_topic_retries < 1 {
                        small_talk_off_topic_retries += 1;
                        tracing::warn!(
                            task_id = %task_id,
                            "Small-talk guardrail: retrying with stricter brief-reply instruction"
                        );
                        current_prompt = format!(
                            "{}\n\n[Regeneration]: Your previous reply was not appropriate for simple small talk \
                             (tools, policies, file paths, or too long). Reply ONLY with a brief polite exchange \
                             (1–2 short sentences) in the same language as the user, following your identity and \
                             personality from the system instructions. No tools.\n",
                            current_prompt
                        );
                        continue;
                    }
                    tracing::warn!(task_id = %task_id, "Small-talk guardrail triggered; suppressing off-topic/tool-heavy reply");
                    reply_text = small_talk_fast_reply(&message, intent);
                    break 'tool_rounds;
                }
            }

            let parsed_tool_calls = tools_executor_snapshot.as_ref().and_then(|_| {
                let calls = parse_tool_calls(&response);
                if calls.is_empty() {
                    None
                } else {
                    Some(calls)
                }
            });
            if code_studio_disk_task && assigned_agent.eq_ignore_ascii_case("studio_project_manager")
            {
                if let Some(calls) = parsed_tool_calls.as_ref() {
                    let delegate_calls = calls
                        .iter()
                        .filter(|(name, _)| name.eq_ignore_ascii_case("delegate_to_agent"))
                        .count();
                    if delegate_calls > 1 {
                        let _ = bus.send(
                        EventEnvelope::new(
                                EventType::ProgressUpdate,
                                Some(serde_json::json!({
                                    "task_id": task_id.to_string(),
                                    "progress_pct": 47,
                                    "message": format!(
                                        "Conflit d'orchestration détecté: {} délégations dans le même tour (anti-collision activé).",
                                        delegate_calls
                                    ),
                                    "studio_notice_type": "studio_conflict_notice",
                                    "reason": "Plusieurs délégations sous-agents dans le même tour (risque d'écrasement croisé).",
                                    "delegate_calls": delegate_calls
                                })),
                            )
                            .with_correlation(timeline_correlation),
                        );
                        current_prompt = format!(
                            "User request: {}\n\nYour previous reply tried {} delegate_to_agent calls in a single turn.\n\n[Code Studio — anti-conflict delegation]\nUse exactly ONE `TOOL: delegate_to_agent <agent> <message>` per turn. Wait for its completion, then read impacted files and continue with the next delegation in a later turn.\nDo not run parallel delegation batches in the same answer.",
                            user_message, delegate_calls
                        );
                        continue;
                    }
                }
            }
            let no_parseable_tools_this_round = parsed_tool_calls.is_none();
            let response_plain = response
                .lines()
                .filter(|l| !l.trim_start().starts_with("TOOL:"))
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string();

            // Code Studio PM mode with explicit mandatory delegation prefix:
            // do not allow the root PM to finish in prose after only read-only checks.
            let studio_delegate_mandatory =
                code_studio_disk_task && message.contains("[Délégation obligatoire (Code Studio");
            let pm_delegate_required = studio_delegate_mandatory
                && assigned_agent.eq_ignore_ascii_case("studio_project_manager");
            let pm_has_delegated = tool_loop_history_by_agent
                .get(&loop_agent_key)
                .map(|v| {
                    v.iter()
                        .any(|(tool, _)| tool.eq_ignore_ascii_case("delegate_to_agent"))
                })
                .unwrap_or(false);
            const MAX_STUDIO_PM_MANDATORY_DELEGATE_NAGS: u32 = 4;
            if pm_delegate_required
                && !pm_has_delegated
                && no_parseable_tools_this_round
                && studio_pm_mandatory_delegate_nags < MAX_STUDIO_PM_MANDATORY_DELEGATE_NAGS
            {
                studio_pm_mandatory_delegate_nags += 1;
                let _ = bus.send(
                    EventEnvelope::new(
                        EventType::ProgressUpdate,
                        Some(serde_json::json!({
                            "task_id": task_id.to_string(),
                            "progress_pct": 47,
                            "message": "Relance PM Code Studio : délégation obligatoire non exécutée (`delegate_to_agent` requis)."
                        })),
                    )
                    .with_correlation(timeline_correlation),
                );
                current_prompt = format!(
                    "User request: {}\n\nYour previous reply:\n{}\n\n[Code Studio — mandatory PM delegation gate]\nYou are `studio_project_manager` and the task is configured with mandatory delegation. You must execute at least one `TOOL: delegate_to_agent <agent_type> <message>` now before any final synthesis. Do not end with prose only.\n\nValid first step examples:\n- TOOL: delegate_to_agent studio_scaffold <task>\n- TOOL: delegate_to_agent studio_frontend <task>\n- TOOL: delegate_to_agent studio_backend <task>\n- TOOL: delegate_to_agent studio_fullstack <task>\n- TOOL: delegate_to_agent code <task>\n- TOOL: delegate_to_agent qa <task>\n",
                    user_message,
                    response_plain
                );
                continue;
            }

            if no_parseable_tools_this_round
                && meta_response_retry_count < 2
                && (is_subagent || assigned_agent != "conversation" || orch_disk_deliverables)
                && looks_like_meta_agent_response(&response_plain)
            {
                meta_response_retry_count += 1;
                current_prompt = format!(
                "User request: {}\n\nYour previous reply:\n{}\n\nThat reply was meta/instruction recitation, not actual progress on the assigned task. Do the work now. Do NOT describe your role, say you are ready, mention instructions, or narrate a generic Phase 2 plan. If files are required, start with TOOL: read_file / write_file on the exact workspace paths. If you are blocked, state only the concrete missing input or exact tool failure.",
                user_message,
                response_plain
            );
                continue;
            }

            const MAX_STUDIO_PROSE_ONLY_WRITE_NAGS: u32 = 4;
            const MAX_STUDIO_ZERO_TOOL_PROMISE_NAGS: u32 = 3;
            const MAX_STUDIO_UNPARSED_TOOL_MARKER_NAGS: u32 = 4;

            let policy_allows_write = tools_executor_snapshot
                .as_ref()
                .map(|e| policy_allows_primary_disk_write(&e.policy))
                .unwrap_or(false);

            let prose_path_base = code_studio_disk_task
                && no_parseable_tools_this_round
                && studio_prose_only_write_nags < MAX_STUDIO_PROSE_ONLY_WRITE_NAGS;
            let prose_heuristic = prose_path_base
                && policy_allows_write
                && crate::api_studio::looks_like_code_studio_prose_only_implementation_reply(
                    &response_plain,
                );

            // Code Studio: first model turn often returns only "Je vais examiner / diagnostic…" with zero TOOL lines, then the task ends.
            let tool_history_empty = tool_loop_history_by_agent
                .get(&loop_agent_key)
                .map(|v| v.is_empty())
                .unwrap_or(true);
            let enforce_pm_zero_tool_guard = code_studio_disk_task
                || assigned_agent.eq_ignore_ascii_case("studio_project_manager");
            let promise_path_base = enforce_pm_zero_tool_guard
                && tools_executor_snapshot.is_some()
                && no_parseable_tools_this_round
                && tool_history_empty
                && !crate::api_studio::code_studio_skip_zero_tool_mandatory_retry(
                    assigned_agent.as_str(),
                )
                && studio_zero_tool_promise_nags < MAX_STUDIO_ZERO_TOOL_PROMISE_NAGS;
            let promise_heuristic = promise_path_base
                && crate::api_studio::looks_like_code_studio_promise_before_any_tools(&response_plain);

            let mut prose_fire = prose_heuristic;
            let promise_fire = promise_heuristic;
            if crate::api_studio::studio_llm_response_auditor_enabled()
                && (prose_heuristic || promise_heuristic)
            {
                if let Some(audit) = crate::api_studio::studio_llm_audit_code_studio_turn(
                    &llm_router,
                    crate::api_studio::StudioLlmAuditParams {
                        user_excerpt: user_message.as_str(),
                        assistant_plain: &response_plain,
                        assigned_agent: assigned_agent.as_str(),
                        policy_allows_write,
                        tool_history_empty,
                        consider_prose: prose_heuristic,
                        consider_promise: promise_heuristic,
                    },
                )
                .await
                {
                    if prose_heuristic {
                        prose_fire = audit.prose_only_implementation;
                    }
                    // Promise-without-tools: keep heuristic result. The optional auditor often answers
                    // false on French intros (« état des lieux », « mise en place »), which would skip
                    // mandatory TOOL retries and leave Code Studio tasks « completed » with zero tools.
                }
            }

            if prose_fire {
                studio_prose_only_write_nags += 1;
                current_prompt = format!(
                    "User request: {}\n\nYour previous reply:\n{}\n\n[Code Studio — mandatory execution guard]\nThe user asked you to implement changes in the repository. Your previous reply was an audit/plan/refusal instead of executing available write tools. You DO have write tools in this task. Emit only executable TOOL lines now.\n\nRequired format examples:\nTOOL: write_file workspace:/path/to/file.ts\n<complete file content on following lines>\n\nTOOL: search_replace workspace:/path/to/file.ts old snippet | new snippet\n\nDo not say \"I cannot modify files\", \"No TOOL lines\", or ask where to start. Apply the requested changes on disk with `write_file`, `edit_file`, `search_replace`, or `apply_patch`.",
                    user_message,
                    response_plain
                );
                continue;
            }

            if promise_fire {
                studio_zero_tool_promise_nags += 1;
                let _ = bus.send(
                    EventEnvelope::new(
                        EventType::ProgressUpdate,
                        Some(serde_json::json!({
                            "task_id": task_id.to_string(),
                            "progress_pct": 48,
                            "message": "Relance Code Studio : la réponse ne contenait aucune ligne TOOL: — exécution d’outils requise (lecture, délégation ou écriture)."
                        })),
                    )
                    .with_correlation(timeline_correlation),
                );
                let pm_delegate = if assigned_agent.eq_ignore_ascii_case("studio_project_manager")
                {
                    " If you are the project manager, you may start with `TOOL: delegate_to_agent <agent> <message>` to a worker, or `TOOL: read_file` / `TOOL: run_command` yourself — but you must output at least one `TOOL:` line this turn, not only a plan in prose."
                } else {
                    ""
                };
                current_prompt = format!(
                    "User request: {}\n\nYour previous reply (rejected — no tools ran):\n{}\n\n[Code Studio — TOOL required this turn]\nYou started with a conversational plan but emitted no `TOOL:` line, so nothing ran on the repository. The task must not end here.\nReply THIS turn with one or more lines starting with `TOOL:` only (then optional brief prose after tool lines if needed). Examples:\n- `TOOL: read_file workspace:/package.json`\n- `TOOL: run_command --cwd workspace:/ npm run build`\n- `TOOL: grep_content workspace:/src pattern`\nDo not reply with only promises like \"I will examine…\" — execute at least one tool call now.{}",
                    user_message,
                    response_plain,
                    pm_delegate
                );
                continue;
            }

            // Kimi-scale dumps: prose + inlined `TOOL:` tokens that do not survive normalization → zero parseable tools.
            let unparsed_marker_base = code_studio_disk_task
                && tools_executor_snapshot.is_some()
                && no_parseable_tools_this_round
                && policy_allows_write
                && studio_unparsed_tool_marker_nags < MAX_STUDIO_UNPARSED_TOOL_MARKER_NAGS
                && crate::api_studio::looks_like_code_studio_tool_marker_but_unparsed(&response);
            if unparsed_marker_base {
                studio_unparsed_tool_marker_nags += 1;
                let _ = bus.send(
                    EventEnvelope::new(
                        EventType::ProgressUpdate,
                        Some(serde_json::json!({
                            "task_id": task_id.to_string(),
                            "progress_pct": 49,
                            "message": "Relance Code Studio : « TOOL: » détecté mais aucun appel exécutable — format strict requis."
                        })),
                    )
                    .with_correlation(timeline_correlation),
                );
                current_prompt = format!(
                    "{}\n\n[Code Studio — unparsed TOOL lines]\nYour last message mentioned TOOL: but nothing parsed as executable tool calls (often: multiline search_replace / write_file shape, or TOOL: embedded in prose). Emit **valid** calls only:\n\
                    - One tool per line starting with `TOOL:` at the beginning of the line (after optional list/markdown noise normalized by the runtime).\n\
                    - `TOOL: search_replace workspace:/path/to/file.ext` then on the **next lines** the full OLD block, a line with only ` | ` (space-pipe-space), then the NEW block — OR keep old|new on one line after the path.\n\
                    - `TOOL: write_file workspace:/path` then the **entire file body** on following lines until the next `TOOL:`.\n\
                    - Prefer `TOOL: run_command --cwd workspace:/ …` for npm/create-vite instead of pasting fake command transcripts.\n\
                    Reply now with working TOOL lines only (brief summary after is OK).",
                    current_prompt
                );
                continue;
            }

            if let (Some(exec), Some(calls)) = (tools_executor_snapshot.as_ref(), parsed_tool_calls)
            {
                round += 1;
                let mut tool_results = Vec::new();
                let scheduled_lanes = akasha_tools::schedule_tool_calls(&calls);
                for (lane, lane_calls) in scheduled_lanes {
                    let lane_name = match lane {
                        akasha_tools::ToolExecutionLane::ParallelSafe => "parallel_safe",
                        akasha_tools::ToolExecutionLane::SerialExclusive => "serial_exclusive",
                    };
                    let _ = bus.send(
                    EventEnvelope::new(
                        EventType::ProgressUpdate,
                        Some(serde_json::json!({
                            "task_id": task_id.to_string(),
                            "progress_pct": 50,
                            "message": format!("Tool lane execution: {} ({} call(s))", lane_name, lane_calls.len())
                        })),
                    )
                    .with_correlation(task_id),
                );
                    for (name, args) in &lane_calls {
                        // Phase D: resolve skill name to tool_ref (spec 33)
                        let actual_tool = match &skill_registry {
                            Some(reg) => reg
                                .get(name)
                                .await
                                .map(|s| s.tool_ref)
                                .unwrap_or_else(|| name.clone()),
                            None => name.clone(),
                        };
                        let actual_tool = canonicalize_tool_name(&actual_tool);
                        let tool_args: &[String] = args.as_slice();
                        // Hard guardrail: if read_file on this path was already truncated for this
                        // agent key, block repeated full-file reads and require chunked/windowed read.
                        if actual_tool.eq_ignore_ascii_case("read_file") {
                            let (path_tokens, explicit_window, want_full) =
                                parse_read_file_args(&tool_args);
                            let allows_escape_truncation_guard =
                                want_full || explicit_window.is_some();
                            if !allows_escape_truncation_guard {
                                let path_input = path_tokens.join(" ");
                                let path_str = normalize_tool_path_hint(&path_input);
                                let previously_truncated = truncated_read_file_paths_by_agent
                                    .get(&loop_agent_key)
                                    .map(|s| s.contains(&path_str))
                                    .unwrap_or(false);
                                if previously_truncated {
                                    let blocked = format!(
                                        "[read_file] blocked repeated default read after partial output for path={}. Use TOOL: read_file {} --full, or a line window (e.g. TOOL: read_file {} 1 200 then 201 200), or narrow with grep_content/search_files.",
                                        path_str,
                                        if path_str.is_empty() { "<path>" } else { &path_str },
                                        if path_str.is_empty() { "<path>" } else { &path_str }
                                    );
                                    let payload = serde_json::json!({
                                        "tool": "read_file",
                                        "args": tool_args,
                                        "result_preview": blocked,
                                        "success": false,
                                        "reason": "read_file_truncated_requires_chunking"
                                    });
                                    let _ = bus.send(
                                        EventEnvelope::new(EventType::ToolInvoked, Some(payload))
                                            .with_correlation(timeline_correlation),
                                    );
                                    tool_results.push(blocked);
                                    continue;
                                }
                            }
                        }
                        let args_str = tool_args.join(" ");
                        let history = tool_loop_history_by_agent
                            .entry(loop_agent_key.clone())
                            .or_default();
                        history.push((actual_tool.clone(), args_str.clone()));
                        // Phase 4: loop detection — same tool+args repeated 3 times
                        // for the same agent key (agent = sub-agent task_id).
                        if history.len() >= 3 {
                            let last = history.last().unwrap();
                            if history
                                .iter()
                                .rev()
                                .take(3)
                                .all(|e| e.0 == last.0 && e.1 == last.1)
                            {
                                reply_text =
                                    "Loop detected: same tool and arguments repeated. Stopping."
                                        .to_string();
                                break 'tool_rounds;
                            }
                        }
                        // User-friendly progress at key step: what we are doing right now (use skill name when actual_tool is empty, e.g. bankr skill).
                        let display_tool = if actual_tool.is_empty() {
                            name.as_str()
                        } else {
                            &actual_tool
                        };
                        let progress_msg = progress_message_for_tool(display_tool, tool_args);
                        if !first_meaningful_progress_sent {
                            first_meaningful_progress_sent = true;
                            meaningful_progress_flag
                                .store(true, std::sync::atomic::Ordering::Relaxed);
                            cancel_task_watchdogs(&mut watchdog_cancel, &mut stall_cancel);
                            if emit_timeline_once_for_task(
                                &bus,
                                Some(store_path.as_path()),
                                task_id,
                                "first_meaningful_progress",
                                Some(serde_json::json!({
                                    "source": "tool_progress",
                                    "tool": display_tool,
                                })),
                            ) {
                                log_latency_metric(store_path.as_path(), task_id, "ttfr_ms");
                            }
                        }
                        let _ = bus.send(
                            EventEnvelope::new(
                                EventType::ProgressUpdate,
                                Some(serde_json::json!({
                                    "task_id": task_id.to_string(),
                                    "progress_pct": 50,
                                    "message": progress_msg
                                })),
                            )
                            .with_correlation(task_id),
                        );
                        // Phase 3.1: tools in require_approval need user confirmation before execution.
                        if exec.policy.requires_approval(&actual_tool) {
                            let permission_mode = load_permission_mode(data_dir);
                            if permission_mode.mode == "allow_all" {
                                // Global session mode bypasses interactive approval prompts.
                            } else {
                            let scope_key = tool_scope_key(&actual_tool, &tool_args);
                            let state = crate::permissions_center::load(data_dir);
                            let mut already_granted = false;
                            if let Some(decision) =
                                crate::permissions_center::lookup(&actual_tool, &scope_key, &state)
                            {
                                match decision.mode {
                                    crate::permissions_center::DecisionMode::AllowPersistent => {
                                        already_granted = true;
                                        let payload = serde_json::json!({
                                            "tool": actual_tool,
                                            "scope": scope_key,
                                            "approved": true,
                                            "mode": "allow_persistent",
                                            "decision_id": decision.id
                                        });
                                        let _ = bus.send(
                                            EventEnvelope::new(EventType::ToolInvoked, Some(payload))
                                                .with_correlation(timeline_correlation),
                                        );
                                    }
                                    crate::permissions_center::DecisionMode::DenyPersistent => {
                                        tool_results.push(
                                            "Action refusée par la politique d'approbation persistante."
                                                .to_string(),
                                        );
                                        let payload = serde_json::json!({
                                            "tool": actual_tool,
                                            "scope": scope_key,
                                            "approved": false,
                                            "mode": "deny_persistent",
                                            "decision_id": decision.id
                                        });
                                        let _ = bus.send(
                                            EventEnvelope::new(EventType::ToolInvoked, Some(payload))
                                                .with_correlation(timeline_correlation),
                                        );
                                        continue;
                                    }
                                }
                            }
                            if already_granted {
                                // Persistent approval matched: skip interactive prompt.
                            } else {
                            match &human_input_store {
                                Some(store) => {
                                    const APPROVAL_TIMEOUT_SECS: u64 = 300;
                                    // Redact write-like tool args entirely; truncate others to avoid leaking secrets/blobs.
                                    const MAX_APPROVAL_ARG_LEN: usize = 80;
                                    let args_preview: String = if matches!(
                                        actual_tool.as_str(),
                                        "apply_patch" | "edit_file" | "write_file" | "write_code" | "delete_file"
                                    ) {
                                        "[redacted]".to_string()
                                    } else {
                                        let truncated: Vec<String> = tool_args
                                            .iter()
                                            .take(3)
                                            .map(|a| {
                                                if a.chars().count() > MAX_APPROVAL_ARG_LEN {
                                                    format!(
                                                        "{}…",
                                                        a.chars()
                                                            .take(MAX_APPROVAL_ARG_LEN)
                                                            .collect::<String>()
                                                    )
                                                } else {
                                                    a.clone()
                                                }
                                            })
                                            .collect();
                                        let suffix = if tool_args.len() > 3 {
                                            format!(" … ({} args)", tool_args.len())
                                        } else {
                                            String::new()
                                        };
                                        truncated.join(" ") + &suffix
                                    };
                                    let question = format!(
                                        "Approuver l'action : {} — {} ?",
                                        actual_tool, args_preview
                                    );
                                    let choices = vec![
                                        "Approuver".to_string(),
                                        "Toujours autoriser".to_string(),
                                        "Refuser".to_string(),
                                    ];
                                    let approval_request_id = Uuid::new_v4().to_string();
                                    let now = chrono::Utc::now();
                                    let queue_req = crate::permissions_queue::PermissionQueueRequest {
                                        id: approval_request_id.clone(),
                                        task_id: task_id.to_string(),
                                        tool: actual_tool.clone(),
                                        scope: scope_key.clone(),
                                        action: actual_tool.clone(),
                                        description: format!("{} {}", actual_tool, args_preview),
                                        rationale: format!(
                                            "Outil sensible (confirmation requise) pour la tâche {}",
                                            task_id
                                        ),
                                        urgency: "normal".to_string(),
                                        status: crate::permissions_queue::QueueStatus::Pending,
                                        created_at: now.to_rfc3339(),
                                        updated_at: now.to_rfc3339(),
                                        expires_at: Some(
                                            (now + chrono::Duration::seconds(APPROVAL_TIMEOUT_SECS as i64))
                                                .to_rfc3339(),
                                        ),
                                        decision_note: None,
                                        decision_source: None,
                                    };
                                    if let Err(err) = crate::permissions_queue::upsert_request(data_dir, queue_req) {
                                        eprintln!(
                                            "failed to persist permission queue request {} for task {} (tool {}): {}",
                                            approval_request_id, task_id, actual_tool, err
                                        );
                                    }
                                    let (tx, rx) = tokio::sync::oneshot::channel();
                                    let pending = PendingHumanInput {
                                        question: question.clone(),
                                        context: format!(
                                            "Outil sensible (nécessite confirmation) : {}",
                                            actual_tool
                                        ),
                                        choices: Some(choices.clone()),
                                        response_tx: tx,
                                    };
                                    {
                                        let mut g = store.write().await;
                                        g.insert(task_id, pending);
                                    }
                                    let payload = serde_json::json!({
                                        "task_id": task_id.to_string(),
                                        "request_id": approval_request_id.clone(),
                                        "question": question,
                                        "context": format!("Outil : {}", actual_tool),
                                        "choices": choices,
                                        "tool_approval": true
                                    });
                                    let _ = bus.send(
                                        EventEnvelope::new(
                                            EventType::TaskWaitingUserInput,
                                            Some(payload.clone()),
                                        )
                                        .with_correlation(task_id),
                                    );
                                    let approval_payload = serde_json::json!({
                                        "tool": actual_tool,
                                        "args_redacted": args_preview,
                                        "task_id": task_id.to_string(),
                                        "request_id": approval_request_id.clone()
                                    });
                                    let _ = bus.send(
                                        EventEnvelope::new(
                                            EventType::ToolApprovalRequest,
                                            Some(approval_payload),
                                        )
                                        .with_correlation(task_id),
                                    );
                                    let answer = match tokio::time::timeout(
                                        std::time::Duration::from_secs(APPROVAL_TIMEOUT_SECS),
                                        rx,
                                    )
                                    .await
                                    {
                                        Ok(Ok(reply)) => reply.trim().to_string(),
                                        _ => {
                                            // Timeout or channel error: remove stale pending entry to avoid it staying forever.
                                            {
                                                let mut g = store.write().await;
                                                g.remove(&task_id);
                                            }
                                            let expired_payload = serde_json::json!({
                                                "task_id": task_id.to_string(),
                                                "tool": actual_tool,
                                                "request_id": approval_request_id.clone(),
                                            });
                                            let _ = crate::permissions_queue::update_status(
                                                data_dir,
                                                &approval_request_id,
                                                crate::permissions_queue::QueueStatus::Expired,
                                                Some("timeout".to_string()),
                                                Some("timeout".to_string()),
                                            );
                                            let _ = bus.send(
                                                EventEnvelope::new(
                                                    EventType::ToolApprovalExpired,
                                                    Some(expired_payload),
                                                )
                                                .with_correlation(task_id),
                                            );
                                            "Refuser".to_string()
                                        }
                                    };
                                    let granted = answer.eq_ignore_ascii_case("Approuver")
                                        || answer.eq_ignore_ascii_case("Toujours autoriser");
                                    let queue_status = if granted {
                                        crate::permissions_queue::QueueStatus::Approved
                                    } else {
                                        crate::permissions_queue::QueueStatus::Denied
                                    };
                                    if let Err(err) = crate::permissions_queue::update_status(
                                        data_dir,
                                        &approval_request_id,
                                        queue_status,
                                        Some(answer.clone()),
                                        Some("chat_inline".to_string()),
                                    ) {
                                        eprintln!(
                                            "failed to update permission queue status for {} (task {}): {}",
                                            approval_request_id, task_id, err
                                        );
                                    }
                                    if answer.eq_ignore_ascii_case("Toujours autoriser") {
                                        let mut state = crate::permissions_center::load(data_dir);
                                        state.decisions.retain(|d| {
                                            !(d.tool == actual_tool && d.scope == scope_key)
                                        });
                                        state.decisions.push(crate::permissions_center::PermissionDecision {
                                            id: Uuid::new_v4().to_string(),
                                            tool: actual_tool.clone(),
                                            scope: scope_key.clone(),
                                            mode: crate::permissions_center::DecisionMode::AllowPersistent,
                                            created_at: chrono::Utc::now().to_rfc3339(),
                                            expires_at: None,
                                        });
                                        let _ = crate::permissions_center::save(data_dir, &state);
                                    }
                                    if !granted {
                                        tool_results.push("Action refusée par l'utilisateur (approbation requise).".to_string());
                                        let payload = serde_json::json!({
                                            "tool": actual_tool,
                                            "approved": false
                                        });
                                        let _ = bus.send(
                                            EventEnvelope::new(
                                                EventType::ToolInvoked,
                                                Some(payload),
                                            )
                                            .with_correlation(timeline_correlation),
                                        );
                                        continue;
                                    }
                                }
                                None => {
                                    tool_results.push("Action nécessitant approbation impossible (human_input_store indisponible).".to_string());
                                    continue;
                                }
                            }
                            }
                            }
                        }
                        let tool_t0 = std::time::Instant::now();
                        let call_id = Uuid::new_v4();
                        let args_preview_tc: String = {
                            const L: usize = 100;
                            args.iter()
                                .take(3)
                                .map(|a| {
                                    if a.len() > L {
                                        format!("{}…", &a[..a.floor_char_boundary(L)])
                                    } else {
                                        a.clone()
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join(" ")
                        };
                        let _ = bus.send(
                            EventEnvelope::new(
                                EventType::ToolCallStarted,
                                Some(serde_json::json!({
                                    "schema_version": 1,
                                    "task_id": task_id.to_string(),
                                    "call_id": call_id.to_string(),
                                    "tool": display_tool,
                                    "args_preview": args_preview_tc,
                                })),
                            )
                            .with_correlation(timeline_correlation),
                        );
                        let (success, res, captured_image): (bool, String, Option<String>) =
                            if actual_tool == "ask_user" {
                                // Human in the loop: register pending request, emit event, wait for user reply.
                                match &human_input_store {
                                    Some(store) => {
                                        let body_joined_string = args.join(" ");
                                        let body = body_joined_string.trim();
                                        let body = if body.is_empty() { "{}" } else { body };
                                        let v =
                                            serde_json::from_str::<serde_json::Value>(body).ok();
                                        let (question, context, choices) = match &v {
                                            Some(v) => (
                                                v.get("question")
                                                    .and_then(|q| q.as_str())
                                                    .unwrap_or("")
                                                    .to_string(),
                                                v.get("context")
                                                    .and_then(|c| c.as_str())
                                                    .unwrap_or("")
                                                    .to_string(),
                                                v.get("choices").and_then(|c| c.as_array()).map(
                                                    |a| {
                                                        a.iter()
                                                            .filter_map(|x| {
                                                                x.as_str().map(String::from)
                                                            })
                                                            .collect::<Vec<_>>()
                                                    },
                                                ),
                                            ),
                                            None => (body.to_string(), String::new(), None),
                                        };
                                        if question.is_empty() {
                                            (false, format!("[ask_user] invalid JSON: question required. Got: {}", body.chars().take(100).collect::<String>()), None)
                                        } else {
                                            let (tx, rx) = tokio::sync::oneshot::channel();
                                            let pending = PendingHumanInput {
                                                question: question.clone(),
                                                context: context.clone(),
                                                choices: choices.clone(),
                                                response_tx: tx,
                                            };
                                            {
                                                let mut g = store.write().await;
                                                g.insert(task_id, pending);
                                            }
                                            let payload = serde_json::json!({
                                                "task_id": task_id.to_string(),
                                                "question": question,
                                                "context": context,
                                                "choices": choices
                                            });
                                            let _ = bus.send(
                                                EventEnvelope::new(
                                                    EventType::TaskWaitingUserInput,
                                                    Some(payload),
                                                )
                                                .with_correlation(task_id),
                                            );
                                            const HUMAN_INPUT_TIMEOUT_SECS: u64 = 3600;
                                            match tokio::time::timeout(
                                                std::time::Duration::from_secs(
                                                    HUMAN_INPUT_TIMEOUT_SECS,
                                                ),
                                                rx,
                                            )
                                            .await
                                            {
                                                Ok(Ok(reply)) => (
                                                    true,
                                                    format!("[ask_user] User replied: {}", reply),
                                                    None,
                                                ),
                                                Ok(Err(_)) => {
                                                    let mut g = store.write().await;
                                                    g.remove(&task_id);
                                                    (
                                                        false,
                                                        "[ask_user] Channel closed.".to_string(),
                                                        None,
                                                    )
                                                }
                                                Err(_) => {
                                                    let mut g = store.write().await;
                                                    g.remove(&task_id);
                                                    (false, format!("[ask_user] Timeout after {}s; no user reply.", HUMAN_INPUT_TIMEOUT_SECS), None)
                                                }
                                            }
                                        }
                                    }
                                    None => (
                                        false,
                                        "[ask_user] Human-in-the-loop not available.".to_string(),
                                        None,
                                    ),
                                }
                            } else if actual_tool == "delegate_to_agent" {
                                if code_studio_disk_task
                                    && !assigned_agent.eq_ignore_ascii_case("studio_project_manager")
                                {
                                    (
                                        false,
                                        "[delegate_to_agent] réservé à l’agent `studio_project_manager` (chef de projet Code Studio). Les sous-agents implémentent directement avec read_file / write_file / …"
                                            .to_string(),
                                        None,
                                    )
                                } else {
                                    match &delegation_tx {
                                        Some(tx) => {
                                            let (reply_tx, reply_rx) = oneshot::channel();
                                            let agent_type = args
                                                .get(0)
                                                .cloned()
                                                .unwrap_or_else(|| "conversation".to_string());
                                            let message = args
                                                .get(1..)
                                                .map(|a| a.join(" "))
                                                .unwrap_or_else(|| args.get(0).cloned().unwrap_or_default());
                                            if tx
                                                .send(DelegationRequest {
                                                    requesting_task_id: task_id,
                                                    agent_type,
                                                    message,
                                                    reply_tx,
                                                })
                                                .await
                                                .is_ok()
                                            {
                                                match tokio::time::timeout(
                                                    std::time::Duration::from_secs(310),
                                                    reply_rx,
                                                )
                                                .await
                                                {
                                                    Ok(Ok(Ok(msg))) => (
                                                        true,
                                                        format!("[delegate_to_agent] {}", msg),
                                                        None,
                                                    ),
                                                    Ok(Ok(Err(e))) => (
                                                        false,
                                                        format!("[delegate_to_agent] {}", e),
                                                        None,
                                                    ),
                                                    _ => (
                                                        false,
                                                        "[delegate_to_agent] timeout or channel closed"
                                                            .to_string(),
                                                        None,
                                                    ),
                                                }
                                            } else {
                                                (
                                                    false,
                                                    "[delegate_to_agent] channel closed".to_string(),
                                                    None,
                                                )
                                            }
                                        }
                                        None => (
                                            false,
                                            "[delegate_to_agent] not available".to_string(),
                                            None,
                                        ),
                                    }
                                }
                            } else if actual_tool == "install_skill" {
                                let url = args.get(0).map(String::as_str).unwrap_or("").trim();
                                if url.is_empty() {
                                    (false, "[install_skill] usage: install_skill <url> (ex. https://github.com/BankrBot/skills/tree/main/bankr ou toute URL HTTPS autorisée dans tools_policy allowed_skill_install_hosts)".to_string(), None)
                                } else {
                                    let data_dir =
                                        store_path.parent().unwrap_or_else(|| store_path.as_ref());
                                    let allowed_hosts = exec.policy.skill_install_allowed_hosts();
                                    let tools_reload = tools_executor.as_ref().and_then(|arc| {
                                        tools_policy_path.as_ref().map(|p| (arc, p.as_path()))
                                    });
                                    match &skill_registry {
                                        Some(reg) => {
                                            let (s, r) = do_install_skill(
                                                url,
                                                data_dir,
                                                &spec_dir,
                                                reg,
                                                &allowed_hosts,
                                                tools_reload,
                                            )
                                            .await;
                                            (s, r, None)
                                        }
                                        None => (
                                            false,
                                            "[install_skill] skill registry not available"
                                                .to_string(),
                                            None,
                                        ),
                                    }
                                }
                            } else if actual_tool == "uninstall_skill" {
                                let skill_name =
                                    tool_args.get(0).map(String::as_str).unwrap_or("").trim();
                                let data_dir =
                                    store_path.parent().unwrap_or_else(|| store_path.as_ref());
                                let tools_reload = tools_executor.as_ref().and_then(|arc| {
                                    tools_policy_path.as_ref().map(|p| (arc, p.as_path()))
                                });
                                match &skill_registry {
                                    Some(reg) => {
                                        let (s, r) = do_uninstall_skill(
                                            skill_name,
                                            data_dir,
                                            &spec_dir,
                                            reg,
                                            tools_reload,
                                        )
                                        .await;
                                        (s, r, None)
                                    }
                                    None => (
                                        false,
                                        "[uninstall_skill] skill registry not available"
                                            .to_string(),
                                        None,
                                    ),
                                }
                            } else if actual_tool == "write_todos" {
                                let payload = args.join(" ").trim().to_string();
                                match TaskStore::open(&store_path) {
                                    Ok(store) => {
                                        let todos = parse_todos_from_payload(&payload);
                                        if let Err(e) = store.set_todos(task_id, &todos) {
                                            (false, format!("[write_todos] error: {}", e), None)
                                        } else {
                                            let payload_json = serde_json::json!({
                                                "task_id": task_id.to_string(),
                                                "todos": todos.iter().map(|t| serde_json::json!({ "id": t.id, "title": t.title, "status": t.status.as_str() })).collect::<Vec<_>>()
                                            });
                                            let _ = bus.send(
                                                EventEnvelope::new(
                                                    EventType::TodoListUpdated,
                                                    Some(payload_json),
                                                )
                                                .with_correlation(task_id),
                                            );
                                            (
                                                true,
                                                format!(
                                                    "[write_todos] {} step(s) saved.",
                                                    todos.len()
                                                ),
                                                None,
                                            )
                                        }
                                    }
                                    Err(e) => {
                                        (false, format!("[write_todos] store error: {}", e), None)
                                    }
                                }
                            } else if actual_tool == "merge_todos" {
                                let payload = args.join(" ").trim().to_string();
                                match TaskStore::open(&store_path) {
                                    Ok(store) => {
                                        match store.merge_todos_from_payload(task_id, &payload) {
                                            Ok(todos) => {
                                                let payload_json = serde_json::json!({
                                                    "task_id": task_id.to_string(),
                                                    "todos": todos.iter().map(|t| serde_json::json!({ "id": t.id, "title": t.title, "status": t.status.as_str() })).collect::<Vec<_>>()
                                                });
                                                let _ = bus.send(
                                                    EventEnvelope::new(
                                                        EventType::TodoListUpdated,
                                                        Some(payload_json),
                                                    )
                                                    .with_correlation(task_id),
                                                );
                                                (
                                                    true,
                                                    format!(
                                                        "[merge_todos] list now has {} step(s).",
                                                        todos.len()
                                                    ),
                                                    None,
                                                )
                                            }
                                            Err(e) => {
                                                (false, format!("[merge_todos] error: {}", e), None)
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        (false, format!("[merge_todos] store error: {}", e), None)
                                    }
                                }
                            } else if actual_tool == "read_todos" {
                                match TaskStore::open(&store_path) {
                                    Ok(store) => match store.get_todos(task_id) {
                                        Ok(todos) => {
                                            let summary: Vec<serde_json::Value> = todos.iter().enumerate().map(|(i, t)| {
                                    serde_json::json!({ "index": i + 1, "title": t.title, "status": t.status.as_str() })
                                }).collect();
                                            (
                                                true,
                                                format!(
                                                    "[read_todos] {} step(s): {}",
                                                    todos.len(),
                                                    serde_json::to_string(&summary)
                                                        .unwrap_or_default()
                                                ),
                                                None,
                                            )
                                        }
                                        Err(e) => {
                                            (false, format!("[read_todos] error: {}", e), None)
                                        }
                                    },
                                    Err(e) => {
                                        (false, format!("[read_todos] store error: {}", e), None)
                                    }
                                }
                            } else if actual_tool == "update_todo" {
                                let index_str =
                                    args.get(0).map(String::as_str).unwrap_or("").trim();
                                let status_str =
                                    args.get(1).map(String::as_str).unwrap_or("pending").trim();
                                let index: usize = index_str.parse().unwrap_or(0);
                                match TaskStore::open(&store_path) {
                                    Ok(store) => {
                                        match store.get_todos(task_id) {
                                            Ok(mut todos) => {
                                                if index == 0 || index > todos.len() {
                                                    (false, format!("[update_todo] invalid index (1..{}): {}", todos.len(), index_str), None)
                                                } else {
                                                    let status =
                                                        match status_str.to_lowercase().as_str() {
                                                            "done" => TodoStatus::Done,
                                                            "cancelled" => TodoStatus::Cancelled,
                                                            _ => TodoStatus::Pending,
                                                        };
                                                    todos[index - 1].status = status;
                                                    if let Err(e) = store.set_todos(task_id, &todos)
                                                    {
                                                        (
                                                            false,
                                                            format!("[update_todo] error: {}", e),
                                                            None,
                                                        )
                                                    } else {
                                                        let payload_json = serde_json::json!({
                                                            "task_id": task_id.to_string(),
                                                            "todos": todos.iter().map(|t| serde_json::json!({ "id": t.id, "title": t.title, "status": t.status.as_str() })).collect::<Vec<_>>()
                                                        });
                                                        let _ = bus.send(
                                                            EventEnvelope::new(
                                                                EventType::TodoListUpdated,
                                                                Some(payload_json),
                                                            )
                                                            .with_correlation(task_id),
                                                        );
                                                        (
                                                            true,
                                                            format!(
                                                                "[update_todo] step {} set to {}.",
                                                                index, status_str
                                                            ),
                                                            None,
                                                        )
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                (false, format!("[update_todo] error: {}", e), None)
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        (false, format!("[update_todo] store error: {}", e), None)
                                    }
                                }
                            } else if actual_tool == "list_skills" {
                                match &skill_registry {
                                    Some(reg) => {
                                        let list = reg.list().await;
                                        let summary: Vec<String> = list
                                            .iter()
                                            .map(|s| format!("{}: {}", s.name, s.description))
                                            .collect();
                                        (
                                            true,
                                            format!(
                                                "[list_skills] {} skill(s): {}",
                                                list.len(),
                                                summary.join(" ; ")
                                            ),
                                            None,
                                        )
                                    }
                                    None => (
                                        false,
                                        "[list_skills] skill registry not available.".to_string(),
                                        None,
                                    ),
                                }
                            } else if actual_tool == "read_skill" {
                                let skill_name =
                                    tool_args.get(0).map(String::as_str).unwrap_or("").trim();
                                if skill_name.is_empty() {
                                    (
                                        false,
                                        "[read_skill] usage: read_skill <name>".to_string(),
                                        None,
                                    )
                                } else {
                                    match &skill_registry {
                                        Some(reg) => {
                                            if let Some(body) = reg.get_body(skill_name).await {
                                                (
                                                    true,
                                                    format!(
                                                        "[read_skill {}] Instructions:\n{}",
                                                        skill_name, body
                                                    ),
                                                    None,
                                                )
                                            } else {
                                                (false, format!("[read_skill] skill '{}' not found or has no body.", skill_name), None)
                                            }
                                        }
                                        None => (
                                            false,
                                            "[read_skill] skill registry not available."
                                                .to_string(),
                                            None,
                                        ),
                                    }
                                }
                            } else if actual_tool.is_empty() {
                                // Skill with no tool_ref: if args provided, run as run_command(skill_name, ...args) (e.g. bankr whoami)
                                if !tool_args.is_empty() {
                                    let run_args: Vec<String> = std::iter::once(name.clone())
                                        .chain(tool_args.iter().cloned())
                                        .collect();
                                    let (s, r, _) = execute_tool_call(
                                        exec,
                                        "run_command",
                                        &run_args,
                                        process_registry.as_ref(),
                                        long_term_client.as_ref(),
                                        task_id,
                                        Some(store_path.as_path()),
                                        conv_tx.clone(),
                                        message_webhook_url.as_deref(),
                                        plugin_registry.as_ref(),
                                        device_bridge.as_ref(),
                                        workspace_store.as_ref(),
                                        browser_registry.as_ref(),
                                        Some(tool_disk_workspace_root.as_path()),
                                        Some(session_id.as_str()),
                                    )
                                    .await;
                                    (s, r, None)
                                } else {
                                    // No args: inject SKILL.md body as context for next round (doc-only)
                                    match &skill_registry {
                                        Some(reg) => {
                                            if let Some(body) = reg.get_body(name).await {
                                                (
                                                    true,
                                                    format!(
                                                        "[Skill: {}] Instructions:\n{}",
                                                        name, body
                                                    ),
                                                    None,
                                                )
                                            } else {
                                                (
                                                    false,
                                                    format!(
                                                        "[Skill: {}] No instructions body.",
                                                        name
                                                    ),
                                                    None,
                                                )
                                            }
                                        }
                                        None => (
                                            false,
                                            "Skill registry not available.".to_string(),
                                            None,
                                        ),
                                    }
                                }
                            } else {
                                execute_tool_call(
                                    exec,
                                    &actual_tool,
                                    tool_args,
                                    process_registry.as_ref(),
                                    long_term_client.as_ref(),
                                    task_id,
                                    Some(store_path.as_path()),
                                    conv_tx.clone(),
                                    message_webhook_url.as_deref(),
                                    plugin_registry.as_ref(),
                                    device_bridge.as_ref(),
                                    workspace_store.as_ref(),
                                    browser_registry.as_ref(),
                                    Some(tool_disk_workspace_root.as_path()),
                                    Some(session_id.as_str()),
                                )
                                .await
                            };
                        if let Some(img) = captured_image {
                            last_captured_image_base64 = Some(img.clone());
                            if captured_media_suitable_for_vision_injection(&img) {
                                let norm = normalize_captured_media_for_vision_turn(&img);
                                if vision_payload_within_cap(&norm) {
                                    pending_completion_image_urls
                                        .get_or_insert_with(Vec::new)
                                        .push(norm);
                                } else {
                                    tracing::debug!(
                                        len = norm.len(),
                                        cap = vision_inject_max_chars(),
                                        "vision inject skipped (payload cap)"
                                    );
                                }
                            }
                        }
                        // Phase F: emit ToolInvoked for Actions tab (spec 33)
                        // Redact or truncate args in the event to avoid leaking large blobs or secrets.
                        let redacted_args: Vec<String> = if matches!(
                            actual_tool.as_str(),
                            "apply_patch" | "edit_file" | "write_file" | "write_code" | "delete_file"
                        ) {
                            vec!["[redacted for write-like tool]".to_string()]
                        } else {
                            const MAX_ARG_PREVIEW_LEN: usize = 512;
                            tool_args
                                .iter()
                                .map(|arg| {
                                    if arg.len() > MAX_ARG_PREVIEW_LEN {
                                        format!(
                                            "{}...[truncated {} chars]",
                                            &arg[..arg.floor_char_boundary(MAX_ARG_PREVIEW_LEN)],
                                            arg.len().saturating_sub(MAX_ARG_PREVIEW_LEN)
                                        )
                                    } else {
                                        arg.clone()
                                    }
                                })
                                .collect()
                        };
                        let tool_display = if actual_tool.is_empty() {
                            name.as_str()
                        } else {
                            actual_tool.as_str()
                        };
                        let payload = serde_json::json!({
                            "tool": tool_display,
                            "skill": if &actual_tool != name { Some(name.as_str()) } else { None::<&str> },
                            "args": redacted_args,
                            "result_preview": if res.len() > 300 { format!("{}...", &res[..res.floor_char_boundary(300)]) } else { res.clone() },
                            "success": success,
                            "explanation": serde_json::Value::Null
                        });
                        let _ = bus.send(
                            EventEnvelope::new(EventType::ToolInvoked, Some(payload))
                                .with_correlation(timeline_correlation),
                        );
                        let _ = bus.send(
                            EventEnvelope::new(
                                EventType::ToolCallFinished,
                                Some(serde_json::json!({
                                    "schema_version": 1,
                                    "task_id": task_id.to_string(),
                                    "call_id": call_id.to_string(),
                                    "tool": tool_display,
                                    "success": success,
                                    "duration_ms": tool_t0.elapsed().as_millis() as u64,
                                })),
                            )
                            .with_correlation(timeline_correlation),
                        );
                        // Chat UI loads map / rich views from timeline_milestone + result_full.
                        if success && tool_display.starts_with("maps_") && res.len() <= 400_000usize
                        {
                            let result_preview = if res.chars().count() > 320 {
                                format!("{}…", res.chars().take(320).collect::<String>())
                            } else {
                                res.clone()
                            };
                            let milestone = serde_json::json!({
                                "name": "deterministic_preferred_tool_result",
                                "task_id": task_id.to_string(),
                                "round": round,
                                "tool": tool_display,
                                "success": true,
                                "result_preview": result_preview,
                                "result_full": res.clone(),
                            });
                            let _ = bus.send(
                                EventEnvelope::new(EventType::TimelineMilestone, Some(milestone))
                                    .with_correlation(timeline_correlation),
                            );
                        }
                        if success {
                            if code_studio_disk_task
                                && matches!(
                                    actual_tool.as_str(),
                                    "write_file"
                                        | "write_code"
                                        | "delete_file"
                                        | "rename_path"
                                        | "move_tree"
                                        | "search_replace"
                                        | "edit_file"
                                        | "apply_patch"
                                )
                            {
                                schedule_code_studio_index_for_root(
                                    store_path.as_path(),
                                    tool_disk_workspace_root.as_path(),
                                );
                            }
                            if actual_tool.eq_ignore_ascii_case("read_file") {
                                let (path_tokens, explicit_window, want_full) =
                                    parse_read_file_args(&tool_args);
                                if !want_full && explicit_window.is_none() {
                                    let path_input = path_tokens.join(" ");
                                    let path_str = normalize_tool_path_hint(&path_input);
                                    let was_truncated = res.contains("[read_file")
                                        && !path_str.is_empty()
                                        && (res.contains("(truncated,")
                                            || res.contains(READ_FILE_PARTIAL_DEFAULT_MARKER));
                                    if was_truncated {
                                        truncated_read_file_paths_by_agent
                                            .entry(loop_agent_key.clone())
                                            .or_default()
                                            .insert(path_str);
                                    }
                                }
                            }
                            log_tool_journal_if_write(&actual_tool, tool_args, &res).await;
                        }
                        tool_results.push(res);
                    }
                }
                let results_blob = tool_results.join("\n");
                let mut studio_readonly_nudge: Option<String> = None;
                if code_studio_disk_task && !calls.is_empty() {
                    let only_survey = calls.iter().all(|(name, _)| {
                        let c = canonicalize_tool_name(&name.trim().to_string());
                        crate::api_studio::studio_survey_tool(&c)
                    });
                    if only_survey {
                        studio_read_only_streak_rounds =
                            studio_read_only_streak_rounds.saturating_add(1);
                    } else {
                        studio_read_only_streak_rounds = 0;
                    }
                    let max_streak = std::env::var("AKASHA_STUDIO_READ_ONLY_STREAK_MAX")
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(4)
                        .max(2)
                        .min(20);
                    if studio_read_only_streak_rounds >= max_streak {
                        studio_read_only_streak_rounds = 0;
                        let _ = bus.send(
                            EventEnvelope::new(
                                EventType::ProgressUpdate,
                                Some(serde_json::json!({
                                    "task_id": task_id.to_string(),
                                    "progress_pct": 52,
                                    "message": "[Étape: garde-fou lecture] Plusieurs tours d’affilée n’ont utilisé que des outils d’exploration — appliquez une modification concrète (write_code / write_file / search_replace / edit_file) ou indiquez le blocage exact."
                                })),
                            )
                            .with_correlation(timeline_correlation),
                        );
                        studio_readonly_nudge = Some(format!(
                            "[STUDIO_READ_ONLY_STREAK]\nThe last {} model rounds only invoked read-only survey tools (read_file, list_dir, grep_content, search_files, file_diff, git status/log/diff). The user request requires repository changes. Emit at least one write-like TOOL line in this turn (write_code, write_file, search_replace, edit_file, apply_patch) OR answer in plain text with the precise blocking reason (e.g. tool policy forbids writes). Do not repeat another exploratory-only round.",
                            max_streak
                        ));
                    }
                }
                last_tool_results_blob = Some(results_blob.clone());
                if results_blob.contains("[browser] Snapshot") {
                    social_snapshot_seen = true;
                }
                let round_had_ask_user = calls.iter().any(|(name, _)| name == "ask_user");
                let msg_social = compute_message_intent_flags(&message).social_feed_fetch;
                let tool_loop_history = tool_loop_history_by_agent
                    .get(&loop_agent_key)
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]);
                let had_web_search = tool_loop_history.iter().any(|(t, _)| t == "web_search");
                let browser_ok = tools_executor_snapshot
                    .as_ref()
                    .map(|e| e.policy.can_use_tool("browser") && e.policy.browser_enabled)
                    .unwrap_or(false);
                let can_ws = tools_executor_snapshot
                    .as_ref()
                    .map(|e| e.policy.can_use_tool("web_search"))
                    .unwrap_or(false);
                let can_wf = tools_executor_snapshot
                    .as_ref()
                    .map(|e| e.policy.can_use_tool("web_fetch"))
                    .unwrap_or(false);
                let had_web_fetch = tool_loop_history.iter().any(|(t, _)| t == "web_fetch");
                let x_profile_url =
                    extract_x_profile_handle(&message).map(|h| format!("https://x.com/{}", h));
                let browser_line = results_blob.contains("[browser]");
                let navigated_ok = results_blob.contains("[browser] Navigated");
                // Social/X: after web_search (any prior round), chain browser navigate → snapshot; ws retry if browser fails or disabled.
                let social_pending = msg_social && had_web_search && !social_snapshot_seen;
                let ws_count = tool_loop_history
                    .iter()
                    .filter(|(t, _)| t == "web_search")
                    .count();
                // Re-inject the user's request so the model always knows what to answer (avoids treating another demand or losing context).
                current_prompt = if round_had_ask_user {
                    format!(
                    "User request (PRIMARY — you must still fulfill this): {}\n\nYour previous assistant reply:\n{}\n\nask_user step result (user's choice; may be unrelated to the primary request):\n{}\n\nContinue the task. If the PRIMARY request is not satisfied yet, you MUST emit TOOL: lines next (web_search, web_fetch, browser navigate + browser snapshot, etc.). Do not reply \"blocked\" or \"no information\" without trying web_search first. When the primary request is fully answered, reply in plain text only (no TOOL: lines).",
                    user_message, response, results_blob
                )
                } else if social_pending && browser_ok && navigated_ok {
                    format!(
                    "User request: {}\n\nYour previous reply:\n{}\n\nTool results (this round):\n{}\n\nThe profile page is open. Run TOOL: browser snapshot now, then answer in plain text with the latest posts visible in the snapshot.",
                    user_message, response, results_blob
                )
                } else if social_pending && browser_ok && !browser_line {
                    format!(
                    "User request: {}\n\nYour previous reply:\n{}\n\nTool results so far:\n{}\n\nSearch snippets may be the wrong account. Run TOOL: browser navigate https://x.com/<handle> (exact @handle from the user message) then TOOL: browser snapshot. Plain-text answer only after snapshot.",
                    user_message, response, results_blob
                )
                } else if social_pending && browser_ok && browser_line && !navigated_ok && can_ws {
                    format!(
                    "User request: {}\n\nYour previous reply:\n{}\n\nTool results:\n{}\n\nBrowser step failed or was blocked. Emit TOOL: web_search with the exact handle (e.g. site:x.com akasha_anthiam). If web_search already failed twice ({} calls), answer in plain text with limitations.",
                    user_message, response, results_blob, ws_count
                )
                } else if social_pending
                    && !browser_ok
                    && can_wf
                    && x_profile_url.is_some()
                    && !had_web_fetch
                {
                    format!(
                    "User request: {}\n\nYour previous reply:\n{}\n\nTool results so far:\n{}\n\nWeb search often returns the wrong X account. Fetch the exact profile HTML: TOOL: web_fetch {}\nThen, if the HTML is a login wall or has no post text, say so. Otherwise quote only text that appears in the fetch result. Emit TOOL: web_fetch now (one URL only).",
                    user_message,
                    response,
                    results_blob,
                    x_profile_url.as_deref().unwrap_or("https://x.com/")
                )
                } else if social_pending && !browser_ok && can_ws && ws_count < 2 {
                    format!(
                    "User request: {}\n\nYour previous reply:\n{}\n\nTool results:\n{}\n\nEmit TOOL: web_search including the EXACT @handle (e.g. site:x.com handle posts). Then answer from results or explain if impossible.",
                    user_message, response, results_blob
                )
                } else if social_pending && !browser_ok && ws_count >= 2 {
                    format!(
                    "User request: {}\n\nTool attempts (summary):\n{}\n\nSTOP: Do NOT invent or fabricate tweet/post text. Reply in plain text ONLY (no TOOL: lines):\n- State that automated retrieval did not reliably return the 3 latest posts for the handle the user asked for (X blocks many scrapers; search snippets often mismatch the account).\n- To get a real timeline in Akasha: set browser_enabled: true in tools_policy.yaml, install Playwright (npx playwright install chromium in scripts/playwright-runner), then ask again — the agent can use browser navigate + snapshot.\n- Optionally give the direct link https://x.com/{} for manual viewing.\n- You may list only URLs or titles that literally appeared in the tool output above — never make up post bodies.",
                    user_message,
                    results_blob,
                    extract_x_profile_handle(&message).as_deref().unwrap_or("handle")
                )
                } else {
                    format!(
                    "User request: {}\n\nYour previous reply:\n{}\n\nTool results:\n{}\n\nUsing ONLY the tool results above, answer the user's request now. Do NOT reply with a promise (e.g. \"I will fetch…\", \"Action in progress\"). The task ends after this message — give the actual answer (e.g. weather forecast, search summary). No TOOL: lines.",
                    user_message, response, results_blob
                )
                };
                if let Some(nudge) = studio_readonly_nudge {
                    current_prompt = format!("{nudge}\n\n{current_prompt}");
                }
                if round >= max_tool_rounds {
                    let response_for_user = response
                        .lines()
                        .filter(|l| !l.trim_start().starts_with("TOOL:"))
                        .collect::<Vec<_>>()
                        .join("\n")
                        .trim()
                        .to_string();
                    let limit_msg = if tool_results.iter().any(|r| {
                        r.contains("device_invoke")
                            && (r.contains("timeout") || r.contains("refused"))
                    }) {
                        "L'accès à l'appareil (caméra/micro) a expiré ou a été refusé. Vous pouvez réessayer en renvoyant votre demande."
                    } else if tool_results
                        .iter()
                        .any(|r| r.contains("generate_image") && r.contains("générée"))
                    {
                        "Image générée."
                    } else if tool_results
                        .iter()
                        .any(|r| r.contains("speech_synthesize") && r.contains("synthétisé"))
                    {
                        "Audio synthétisé."
                    } else if tool_results
                        .iter()
                        .any(|r| r.contains("device_invoke") && r.contains("success"))
                    {
                        "Photo reçue."
                    } else {
                        "Limite de tours d'outils atteinte."
                    };
                    let image_md = last_captured_image_base64
                        .as_ref()
                        .filter(|b| !b.is_empty())
                        .map(|b| {
                            let url = if b.starts_with("data:") {
                                b.clone()
                            } else {
                                format!("data:image/jpeg;base64,{}", b)
                            };
                            let label = if url.starts_with("data:audio/") {
                                "Audio synthétisé"
                            } else if b.starts_with("data:") {
                                "Image générée"
                            } else {
                                "Photo capturée"
                            };
                            build_image_markdown(&label, &url)
                        })
                        .unwrap_or_default();
                    let response_clean = ensure_no_open_code_block(&response_for_user);
                    let default_reply = if response_for_user.is_empty() {
                        format!("{}{}", limit_msg, image_md)
                    } else {
                        format!("{}\n\n[{}]{}", response_clean, limit_msg, image_md)
                    };

                    // When human_input_store is available, ask user whether to continue (+10 rounds), reset and continue, or stop.
                    let should_stop = match &human_input_store {
                        Some(store) => {
                            const TOOL_ROUND_LIMIT_TIMEOUT_SECS: u64 = 300;
                            let question = "Limite de tours d'outils atteinte. Souhaitez-vous continuer la tâche ?".to_string();
                            let context = format!(
                            "La tâche a utilisé {} tours d'outils (max {}). Vous pouvez ajouter 10 tours, réinitialiser le compteur et ajouter 10 tours, ou arrêter.",
                            round, max_tool_rounds
                        );
                            let choices = vec![
                                "Continuer (+10 tours)".to_string(),
                                "Réinitialiser et continuer (+10 tours)".to_string(),
                                "Arrêter".to_string(),
                            ];
                            let (tx, rx) = tokio::sync::oneshot::channel();
                            let pending = PendingHumanInput {
                                question: question.clone(),
                                context: context.clone(),
                                choices: Some(choices.clone()),
                                response_tx: tx,
                            };
                            {
                                let mut g = store.write().await;
                                g.insert(task_id, pending);
                            }
                            let payload = serde_json::json!({
                                "task_id": task_id.to_string(),
                                "question": question,
                                "context": context,
                                "choices": choices,
                                "tool_round_limit": true,
                                "current_round": round,
                                "max_tool_rounds": max_tool_rounds
                            });
                            let _ = bus.send(
                                EventEnvelope::new(EventType::TaskWaitingUserInput, Some(payload))
                                    .with_correlation(task_id),
                            );
                            let reply = tokio::time::timeout(
                                std::time::Duration::from_secs(TOOL_ROUND_LIMIT_TIMEOUT_SECS),
                                rx,
                            )
                            .await;
                            {
                                let mut g = store.write().await;
                                g.remove(&task_id);
                            }
                            match reply {
                                Ok(Ok(user_choice)) => {
                                    let choice = user_choice.trim();
                                    if choice == "Continuer (+10 tours)" {
                                        max_tool_rounds += 10;
                                        false
                                    } else if choice == "Réinitialiser et continuer (+10 tours)" {
                                        round = 0;
                                        max_tool_rounds += 10;
                                        false
                                    } else {
                                        true
                                    }
                                }
                                _ => true,
                            }
                        }
                        None => true,
                    };
                    if should_stop {
                        reply_text = default_reply;
                        break;
                    }
                    continue;
                }
                continue;
            }

            // When the model returns prose / JSON only, this loop would exit with zero tool rounds — UI shows an
            // answer but nothing is written. For orchestrated disk deliverables, nudge additional LLM rounds until
            // a write-like tool appears in history. Only nag when the active policy actually permits write tools;
            // if the profile blocks them the nudge would just churn through "tool not allowed" errors.
            const MAX_ORCH_DISK_WRITE_NAGS: u32 = 8;
            if orch_disk_deliverables
                && tools_executor_snapshot.is_some()
                && no_parseable_tools_this_round
            {
                let policy_allows_write = tools_executor_snapshot
                    .as_ref()
                    .map(|e| policy_allows_primary_disk_write(&e.policy))
                    .unwrap_or(false);
                if policy_allows_write {
                    let tool_loop_history = tool_loop_history_by_agent
                        .get(&loop_agent_key)
                        .map(|v| v.as_slice())
                        .unwrap_or(&[]);
                    let disk_write_attempted = tool_loop_history.iter().any(|(t, _)| {
                        matches!(
                            t.as_str(),
                            "write_file" | "write_code" | "edit_file" | "search_replace" | "apply_patch"
                        )
                    });
                    if !disk_write_attempted && orch_disk_write_nags < MAX_ORCH_DISK_WRITE_NAGS {
                        orch_disk_write_nags += 1;
                        current_prompt = format!(
                        "{}\n\n[Orchestrator — disk deliverables] Your last assistant message did not include any executable TOOL: lines (or they were not parsed). This step MUST call tools: use TOOL: read_file on the shared plan trace if needed, then TOOL: write_code / write_file / edit_file / search_replace for every mandatory workspace path and update the plan sections **Fait (agent)** / **Reste (agent)**. Do not finish with prose-only or ```json``` — emit TOOL lines now.",
                        current_prompt
                    );
                        continue;
                    }
                }
            }

            let response_for_user = response
                .lines()
                .filter(|l| !l.trim_start().starts_with("TOOL:"))
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string();
            const MAX_STUDIO_MANUAL_PATCH_NAGS: u32 = 3;
            if code_studio_disk_task
                && looks_like_manual_file_patch_reply(&response_for_user)
                && tools_executor_snapshot.is_some()
            {
                let (policy_allows_write, write_tool_seen) = tools_executor_snapshot
                    .as_ref()
                    .map(|e| {
                        let tool_loop_history = tool_loop_history_by_agent
                            .get(&loop_agent_key)
                            .map(|v| v.as_slice())
                            .unwrap_or(&[]);
                        let allows = policy_allows_primary_disk_write(&e.policy);
                        let seen = tool_loop_history.iter().any(|(t, _)| {
                            matches!(
                                t.as_str(),
                                "write_file" | "write_code" | "edit_file" | "search_replace" | "apply_patch"
                            )
                        });
                        (allows, seen)
                    })
                    .unwrap_or((false, false));
                if policy_allows_write
                    && !write_tool_seen
                    && studio_manual_patch_nags < MAX_STUDIO_MANUAL_PATCH_NAGS
                {
                    studio_manual_patch_nags += 1;
                    current_prompt = format!(
                        "{}\n\n[Code Studio — mandatory runtime guard]\nYour previous reply asked the user to manually paste file changes. This is not acceptable here while write tools are available.\nEmit TOOL lines now and apply the fix directly on disk using `search_replace`, `edit_file`, `write_file`, or `apply_patch` (workspace:/ paths). Do NOT output manual replacement blocks.\nIf a write tool fails, include the tool error and explain the blocker briefly.",
                        current_prompt
                    );
                    continue;
                }
            }
            // If we already ran tools but the model returned a placeholder ("Je vais… Une seconde."), force one more round to get the actual answer.
            let tool_loop_history = tool_loop_history_by_agent
                .get(&loop_agent_key)
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            if !tool_loop_history.is_empty()
                && last_tool_results_blob
                    .as_ref()
                    .map_or(false, |b| !b.is_empty())
                && looks_like_placeholder_after_tools(&response_for_user)
                && !force_synthesis_attempted
            {
                force_synthesis_attempted = true;
                current_prompt = format!(
                "User request: {}\n\nTool results:\n{}\n\nThe user is waiting for the actual answer. Your previous message was a promise — the task is about to close, so you must answer NOW. Using the tool results above, write ONLY the final answer to the user's request. No TOOL: lines, no \"action in progress\".",
                user_message,
                last_tool_results_blob.as_deref().unwrap_or("")
            );
                continue;
            }
            // Guardrail: when tools already produced results, reject generic greeting responses
            // and force one synthesis round from tool outputs.
            if !tool_loop_history.is_empty()
                && last_tool_results_blob
                    .as_ref()
                    .map_or(false, |b| !b.is_empty())
                && looks_like_off_topic_greeting(&response_for_user)
                && !force_synthesis_attempted
            {
                force_synthesis_attempted = true;
                current_prompt = format!(
                "User request: {}\n\nTool results:\n{}\n\nYour previous reply was off-topic greeting text. Answer the user's request NOW using the tool results above. Return only the concrete final answer (distance/itinerary if available). No greeting, no TOOL: lines.",
                user_message,
                last_tool_results_blob.as_deref().unwrap_or("")
            );
                continue;
            }
            let image_md = last_captured_image_base64
                .as_ref()
                .filter(|b| !b.is_empty())
                .map(|b| {
                    let url = if b.starts_with("data:") {
                        b.clone()
                    } else {
                        format!("data:image/jpeg;base64,{}", b)
                    };
                    let label = if url.starts_with("data:audio/") {
                        "Audio synthétisé"
                    } else if b.starts_with("data:") {
                        "Image générée"
                    } else {
                        "Photo capturée"
                    };
                    build_image_markdown(&label, &url)
                })
                .unwrap_or_default();
            let response_clean = ensure_no_open_code_block(&response_for_user);
            let enforce_pm_zero_tool_warning = code_studio_disk_task
                || assigned_agent.eq_ignore_ascii_case("studio_project_manager");
            let warn_zero_tools_heuristic = enforce_pm_zero_tool_warning
                && tool_loop_history.is_empty()
                && !crate::api_studio::code_studio_skip_zero_tool_mandatory_retry(
                    assigned_agent.as_str(),
                )
                && crate::api_studio::looks_like_code_studio_promise_before_any_tools(
                    &response_for_user,
                );
            let mut warn_zero_tools_fire = warn_zero_tools_heuristic;
            if warn_zero_tools_heuristic && crate::api_studio::studio_llm_response_auditor_enabled()
            {
                let user_ex: String = user_message.chars().take(2200).collect();
                let assist_ex: String = response_for_user.chars().take(3200).collect();
                if let Some(audit) = crate::api_studio::studio_llm_audit_code_studio_turn(
                    &llm_router,
                    crate::api_studio::StudioLlmAuditParams {
                        user_excerpt: user_ex.as_str(),
                        assistant_plain: assist_ex.as_str(),
                        assigned_agent: assigned_agent.as_str(),
                        policy_allows_write: false,
                        tool_history_empty: true,
                        consider_prose: false,
                        consider_promise: true,
                    },
                )
                .await
                {
                    warn_zero_tools_fire = audit.promise_without_tools;
                }
            }
            let studio_no_tool_warning = if warn_zero_tools_fire {
                "\n\n— *Akasha (Code Studio)* : aucune ligne `TOOL:` n’a été exécutée ; le dépôt n’a probablement pas été modifié. Relancez la tâche ou vérifiez le fournisseur LLM."
            } else {
                ""
            };
            if let Some(ref sq) = steering_queue {
                let follow_items = sq.drain_follow_up(task_id).await;
                if !follow_items.is_empty() && !is_subagent {
                    for item in follow_items {
                        let fu = format!(
                            "[Follow-up — poursuivre après le travail en cours]\n{}",
                            item.text
                        );
                        if let Some(st) = short_term.as_ref() {
                            st.append(&session_id, "user", fu.clone()).await;
                        }
                        user_message.push_str("\n\n");
                        user_message.push_str(&fu);
                        let _ = store.insert_event(
                            task_id,
                            "user_follow_up_applied",
                            Some(&serde_json::json!({
                                "queue_id": item.id,
                                "preview": item.text.chars().take(200).collect::<String>(),
                                "schema_version": 1
                            })),
                            &chrono::Utc::now().to_rfc3339(),
                        );
                    }
                    current_prompt = format!(
                        "{guardrail_reminder_block}{write_reminder}{web_search_reminder}{web_search_followup_reminder}{transport_reminder}{geolocation_distance_reminder}{plugin_catalog_reminder}{social_feed_reminder}{device_camera_reminder}{image_generation_reminder}{github_vault_reminder}{code_dev_sandbox_reminder}{user_prefix}User:\n{user_message}"
                    );
                    round = 0;
                    continue;
                }
            }
            reply_text = if response_for_user.is_empty() {
                format!("{}{}", response, image_md)
            } else {
                format!("{}{}{}", response_clean, studio_no_tool_warning, image_md)
            };
            break;
        }
    }

    let mut reply_text = if reply_text.is_empty() {
        tracing::warn!("LLM returned empty text");
        "No response from the model. Check Ollama or your LLM provider.".to_string()
    } else {
        if std::env::var("AKASHA_LOG_LLM_RESPONSE").as_deref() == Ok("1") {
            tracing::info!(response = %reply_text, "LLM full response");
        } else {
            tracing::debug!(response = %reply_text, "LLM full response (set AKASHA_LOG_LLM_RESPONSE=1 to log at info)");
        }
        reply_text
    };
    let task_status_snapshot = store.get(task_id).ok().flatten().map(|t| t.status);
    let is_paused = matches!(task_status_snapshot, Some(TaskStatus::Paused));
    let halted_user = matches!(
        task_status_snapshot,
        Some(
            TaskStatus::Paused
                | TaskStatus::Cancelled
                | TaskStatus::Interrupted
                | TaskStatus::Failed
        )
    );
    let mut studio_verify_error: Option<String> = None;
    let mut studio_autofix_applied = false;
    if !halted_user && code_studio_disk_task {
        let max_passes: u32 = if is_session_recall {
            1
        } else {
            std::env::var("AKASHA_STUDIO_VERIFY_MAX_PASSES")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(3)
                .max(1)
                .min(8)
        };
        for pass in 0..max_passes {
            match crate::api_studio::studio_verify_after_agent_task(&tool_disk_workspace_root).await {
                Ok(()) => {
                    studio_verify_error = None;
                    break;
                }
                Err(e) => {
                    studio_verify_error = Some(e.clone());
                    if pass + 1 >= max_passes {
                        break;
                    }
                    if let Some(ref exec_arc) = tools_executor {
                        let denom = max_passes.saturating_sub(1).max(1);
                        let _ = bus.send(
                            EventEnvelope::new(
                                EventType::ProgressUpdate,
                                Some(serde_json::json!({
                                    "task_id": task_id.to_string(),
                                    "progress_pct": 55,
                                    "message": format!(
                                        "Compilation du projet en échec — correction automatique (tentative {}/{}).",
                                        pass + 1,
                                        denom
                                    )
                                })),
                            )
                            .with_correlation(timeline_correlation),
                        );
                        let llm_rounds = std::env::var("AKASHA_STUDIO_VERIFY_AUTOFIX_LLM_ROUNDS")
                            .ok()
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(6)
                            .max(1)
                            .min(16);
                        let data_dir = store_path
                            .parent()
                            .unwrap_or_else(|| store_path.as_path());
                        let did_write = studio_verify_run_llm_autofix_rounds(
                            &bus,
                            &llm_router,
                            exec_arc,
                            skill_registry.as_ref(),
                            plugin_registry.as_ref(),
                            process_registry.as_ref(),
                            conv_tx.clone(),
                            long_term_client.as_ref(),
                            workspace_store.as_ref(),
                            browser_registry.as_ref(),
                            device_bridge.as_ref(),
                            task_id,
                            store_path.as_path(),
                            data_dir,
                            tool_disk_workspace_root.as_path(),
                            &e,
                            llm_rounds,
                        )
                        .await;
                        if did_write {
                            studio_autofix_applied = true;
                        }
                    } else {
                        break;
                    }
                }
            }
        }
        if studio_verify_error.is_none() && studio_autofix_applied {
            reply_text.push_str("\n\n_(Compilation du projet Code Studio corrigée automatiquement après échec du build.)_");
        }
        if studio_verify_error.is_none() {
            if let Some(ref pay) = embedded_studio_acceptance {
                let _ = bus.send(
                    EventEnvelope::new(
                        EventType::ProgressUpdate,
                        Some(serde_json::json!({
                            "task_id": task_id.to_string(),
                            "progress_pct": 57,
                            "message": "[Étape: critères d'acceptation] Vérification fichiers / commandes configurées…"
                        })),
                    )
                    .with_correlation(timeline_correlation),
                );
                let tmo = crate::api_studio::studio_project_verify_timeout_sec(
                    tool_disk_workspace_root.as_path(),
                );
                let mech = crate::api_studio::run_mechanical_acceptance_checks(
                    tool_disk_workspace_root.as_path(),
                    pay,
                    tmo,
                )
                .await;
                if !mech.is_empty() {
                    studio_verify_error = Some(format!(
                        "Échec critères d'acceptation (vérification automatique) :\n{}",
                        mech.join("\n")
                    ));
                } else {
                    let manual_lines: Vec<String> = pay
                        .criteria
                        .iter()
                        .filter(|c| matches!(c.kind, crate::api_studio::StudioCriterionKind::Manual))
                        .map(|c| {
                            let id = if c.id.is_empty() { "-" } else { c.id.as_str() };
                            format!("{id}: {}", c.text)
                        })
                        .collect();
                    if !manual_lines.is_empty() {
                        let _ = bus.send(
                            EventEnvelope::new(
                                EventType::ProgressUpdate,
                                Some(serde_json::json!({
                                    "task_id": task_id.to_string(),
                                    "progress_pct": 59,
                                    "message": "[Étape: contrôle critères] Analyse des critères manuels (après build)…"
                                })),
                            )
                            .with_correlation(timeline_correlation),
                        );
                        let data_dir = store_path.parent().unwrap_or_else(|| store_path.as_path());
                        let diff_summary: String = {
                            let snap_path = data_dir
                                .join("studio-task-snapshots")
                                .join(format!("{task_id}.json"));
                            let snap_json = std::fs::read_to_string(&snap_path).unwrap_or_default();
                            if snap_json.is_empty() {
                                String::new()
                            } else if let Ok(snap) = serde_json::from_str::<
                                crate::studio_task_snapshot::StudioTaskSnapshot,
                            >(&snap_json)
                            {
                                match tokio::task::spawn_blocking(move || {
                                    crate::studio_task_snapshot::compute_studio_task_diff_from_snapshot(
                                        snap,
                                    )
                                })
                                .await
                                {
                                    Ok(Ok(entries)) => {
                                        let mut parts = Vec::new();
                                        for e in entries.iter().take(24) {
                                            parts.push(format!(
                                                "{} [{}]: {}",
                                                e.path,
                                                e.status,
                                                e.diff.chars().take(900).collect::<String>()
                                            ));
                                        }
                                        parts.join("\n")
                                    }
                                    _ => String::new(),
                                }
                            } else {
                                String::new()
                            }
                        };
                        if let Some(missing) = studio_semantic_acceptance_review(
                            &llm_router,
                            &manual_lines,
                            &diff_summary,
                            clean_message,
                            &reply_text,
                        )
                        .await
                        {
                            if !missing.is_empty() {
                                reply_text.push_str(
                                    "\n\n---\n**Revue critères (non bloquant)** — à vérifier manuellement :\n- ",
                                );
                                reply_text.push_str(&missing.join("\n- "));
                                if let Ok(store_ev) = TaskStore::open(&store_path) {
                                    let _ = store_ev.insert_event(
                                        task_id,
                                        "studio_acceptance_review",
                                        Some(&serde_json::json!({ "missing": missing })),
                                        &chrono::Utc::now().to_rfc3339(),
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    if !first_meaningful_progress_sent && !reply_text.trim().is_empty() {
        meaningful_progress_flag.store(true, std::sync::atomic::Ordering::Relaxed);
        cancel_task_watchdogs(&mut watchdog_cancel, &mut stall_cancel);
        if emit_timeline_once_for_task(
            &bus,
            Some(store_path.as_path()),
            task_id,
            "first_meaningful_progress",
            Some(serde_json::json!({ "source": "final_reply" })),
        ) {
            log_latency_metric(store_path.as_path(), task_id, "ttfr_ms");
        }
    } else {
        cancel_task_watchdogs(&mut watchdog_cancel, &mut stall_cancel);
    }

    // Persist this exchange in short-term memory (spec 06)
    // Skip orchestrated task messages ([Task]\n prefix): they are internal planner artefacts,
    // not real user/assistant turns. Storing them pollutes future context with unrelated content.
    if !incognito && !is_small_talk_fast_lane && !is_orchestrated_task_msg {
        if let Some(ref st) = short_term {
            // Store the clean user message (without any guardrail prefix) so history is human-readable.
            st.append(&session_id, "user", clean_message.to_string())
                .await;
            st.append(&session_id, "assistant", reply_text.clone())
                .await;
        }
    }

    // Extract and promote personal facts to long-term memory (spec 06: nom, préférences, décisions).
    if !incognito && !is_small_talk_fast_lane {
        if let Some(ref long_term) = long_term_client {
            // Heuristic: capture obvious name/intro from user message. Promote these *immediately* so they appear in Memory tab right away.
            // Case-insensitive matching on lowercased text, but extract from original message to preserve casing.
            let msg_lower = message.to_lowercase();
            let mut heuristic_facts = Vec::new();
            for (pattern, prefix) in [
                ("je m'appelle ", "L'utilisateur s'appelle "),
                ("mon nom est ", "L'utilisateur s'appelle "),
                ("mon prénom est ", "L'utilisateur s'appelle "),
                ("mon prénom c'est ", "L'utilisateur s'appelle "),
                ("je suis ", "L'utilisateur est "),
                ("tu peux m'appeler ", "L'utilisateur veut être appelé "),
                ("appelle-moi ", "L'utilisateur veut être appelé "),
                ("i'm ", "The user is "),
                ("my name is ", "The user's name is "),
                ("call me ", "The user wants to be called "),
            ] {
                if let Some(start_idx) = msg_lower.find(pattern) {
                    let value_start = start_idx + pattern.len();
                    // Extract value from the *original* message at the same position to preserve casing.
                    let original_rest = &message[value_start..];
                    let name = original_rest
                        .trim()
                        .split(|c: char| c == ',' || c == '.' || c == '\n' || c == '!')
                        .next()
                        .unwrap_or(original_rest)
                        .trim();
                    let name = name.chars().take(80).collect::<String>();
                    if !name.is_empty() {
                        heuristic_facts.push(format!("{}{}", prefix, name));
                        break;
                    }
                }
            }
            // Promote heuristic facts synchronously so they are stored before the user opens the Memory tab.
            for fact in &heuristic_facts {
                let client = long_term.clone();
                let fact = fact.clone();
                match tokio::task::spawn_blocking(move || {
                    let res = client.promote(
                        fact.clone(),
                        "user_fact".to_string(),
                        None,
                        None,
                        None,
                        Some(2),
                        Some("global_user".to_string()),
                        None,
                        None,
                    );
                    if res.is_ok() {
                        let _ = client.emit_event(
                            "user_preference".to_string(),
                            fact,
                            None,
                            None,
                            None,
                            None,
                            Some(2),
                            Some("global_user".to_string()),
                            Some("user_fact".to_string()),
                        );
                    }
                    res
                })
                .await
                {
                    Ok(Ok(_)) => {
                        tracing::info!("Personal fact stored in long-term memory (heuristic)")
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(error = %e, "Long-term promote failed — check that embeddings/tract model loads (see daemon logs)")
                    }
                    Err(e) => tracing::debug!(error = %e, "Promote task join error"),
                }
            }

            // Then spawn LLM extraction for projects, interests, important info, personal facts, and agent profile (async).
            let msg = message.clone();
            let reply = reply_text.clone();
            let client = long_term.clone();
            let router = llm_router.clone();
            let heuristic_set: std::collections::HashSet<String> =
                heuristic_facts.iter().cloned().collect();
            let sem = extract_semaphore();
            let data_dir_for_extract = data_dir.to_path_buf();
            let agent_profile_cache_for_extract = agent_profile_cache.clone();
            tokio::spawn(async move {
                let _permit = match sem.try_acquire() {
                    Ok(p) => p,
                    Err(_) => {
                        tracing::debug!("Background fact extraction skipped: semaphore full");
                        return;
                    }
                };
                let extract_prompt = format!(
                "Extract items to remember. One line per item, each line starts with exactly one of these prefixes:\n\
FACT: personal facts (name, preferences, decisions)\n\
PROJECT: projects created or mentioned\n\
INTEREST: interests\n\
IMPORTANT: important information to remember\n\
AGENT_NAME: the name the user gives the agent (e.g. You are called X)\n\
AGENT_PERSONALITY: personality or tone requested for the agent\n\
AGENT_RULE: a rule the agent must follow\n\
AGENT_CAN: what the agent can do (allowed)\n\
AGENT_CANNOT: what the agent must not do (forbidden)\n\
Write only lines with these prefixes, or NOTHING if none. No other text.\n\
Extract only facts explicitly mentioned (by the user or the assistant). Do not invent anything.\n\nUser: {}\n\nAssistant: {}",
                crate::llm_prompt_cap::truncate_utf8_bytes(
                    msg.trim(),
                    crate::llm_prompt_cap::SYSTEM_PROMPT_FIELD_MAX_BYTES,
                ),
                crate::llm_prompt_cap::truncate_utf8_bytes(
                    reply.trim(),
                    crate::llm_prompt_cap::SYSTEM_PROMPT_FIELD_MAX_BYTES,
                )
            );
                let extract_max_tokens = std::env::var("AKASHA_SYSTEM_TASK_MAX_TOKENS")
                    .ok()
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(2048);
                let req = CompletionRequest {
                    prompt: extract_prompt,
                    max_tokens: Some(extract_max_tokens),
                    temperature: Some(0.1),
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
                let mut to_promote: Vec<(String, String)> = Vec::new();
                let mut agent_updates: Vec<(String, String)> = Vec::new();
                if let Ok(Ok(resp)) =
                    tokio::time::timeout(std::time::Duration::from_secs(30), router.complete(&req))
                        .await
                {
                    for line in resp.text.lines() {
                        let line = line.trim();
                        if let Some(rest) = line.strip_prefix("AGENT_NAME:") {
                            agent_updates.push(("AGENT_NAME".to_string(), rest.trim().to_string()));
                        } else if let Some(rest) = line.strip_prefix("AGENT_PERSONALITY:") {
                            agent_updates
                                .push(("AGENT_PERSONALITY".to_string(), rest.trim().to_string()));
                        } else if let Some(rest) = line.strip_prefix("AGENT_RULE:") {
                            agent_updates.push(("AGENT_RULE".to_string(), rest.trim().to_string()));
                        } else if let Some(rest) = line.strip_prefix("AGENT_CAN:") {
                            agent_updates.push(("AGENT_CAN".to_string(), rest.trim().to_string()));
                        } else if let Some(rest) = line.strip_prefix("AGENT_CANNOT:") {
                            agent_updates
                                .push(("AGENT_CANNOT".to_string(), rest.trim().to_string()));
                        } else {
                            let (content, source) = if let Some(rest) = line.strip_prefix("FACT:") {
                                (rest.trim().to_string(), "user_fact".to_string())
                            } else if let Some(rest) = line.strip_prefix("PROJECT:") {
                                (rest.trim().to_string(), "project".to_string())
                            } else if let Some(rest) = line.strip_prefix("INTEREST:") {
                                (rest.trim().to_string(), "interest".to_string())
                            } else if let Some(rest) = line.strip_prefix("IMPORTANT:") {
                                (rest.trim().to_string(), "important".to_string())
                            } else {
                                continue;
                            };
                            if !content.is_empty()
                                && !heuristic_set.contains(&content)
                                && !should_skip_capture_content(&content)
                            {
                                let content_trimmed = if content.len() > CAPTURE_MAX_CHARS {
                                    content.chars().take(CAPTURE_MAX_CHARS).collect::<String>()
                                } else {
                                    content
                                };
                                to_promote.push((content_trimmed, source));
                            }
                        }
                    }
                }
                // Cap number of items promoted per turn (plan court terme 7).
                if to_promote.len() > CAPTURE_MAX_PER_TURN {
                    to_promote.truncate(CAPTURE_MAX_PER_TURN);
                }
                if !agent_updates.is_empty() {
                    let data_dir_extract = data_dir_for_extract.clone();
                    let cache = agent_profile_cache_for_extract.clone();
                    let mut profile = match &cache {
                        Some(c) => get_or_load_agent_profile(&data_dir_extract, c).await,
                        None => AgentProfile::load(&data_dir_for_extract),
                    };
                    for (kind, value) in agent_updates {
                        profile.apply_extracted(&kind, value);
                    }
                    if let Err(e) = profile.save(&data_dir_for_extract) {
                        tracing::warn!(error = %e, "Failed to save agent profile");
                    } else {
                        if let Some(c) = &cache {
                            set_agent_profile_cache(c, profile).await;
                        }
                        tracing::info!("Agent profile updated from conversation");
                    }
                }
                if !to_promote.is_empty() {
                    tracing::debug!(
                        count = to_promote.len(),
                        "Promoting extracted items to long-term memory"
                    );
                }
                for (content, source) in to_promote {
                    let client = client.clone();
                    let c = content.clone();
                    let s = source.clone();
                    match tokio::task::spawn_blocking(move || {
                        client.promote(c, s, None, None, None, None, None, None, None)
                    })
                    .await
                    {
                        Ok(Ok(_)) => {}
                        Ok(Err(e)) => tracing::warn!(error = %e, "Long-term promote failed"),
                        Err(e) => tracing::debug!(error = %e, "Promote task join error"),
                    }
                }
            });
        }
    }

    let _ = bus.send(
        EventEnvelope::new(
            EventType::ProgressUpdate,
            Some(serde_json::json!({
                "task_id": task_id.to_string(),
                "progress_pct": 100,
                "message": reply_text
            })),
        )
        .with_correlation(task_id),
    );
    // Phase 3.3: when task ends with an error-like outcome, emit TaskEscalatedToHuman for UI banner / retry.
    let reply_lower = reply_text.to_lowercase();
    let is_error_outcome = reply_lower.contains("sorry, i couldn't")
        || reply_lower.contains("timed out")
        || reply_lower.contains("timeout")
        || reply_lower.contains("budget dépassé")
        || reply_lower.contains("quota de tokens")
        || reply_lower.contains("loop detected")
        || reply_lower.contains("refusée par l'utilisateur")
        || reply_lower.contains("action refusée")
        || reply_lower.contains("limite de tours d'outils atteinte");
    if is_error_outcome {
        let _ = bus.send(
            EventEnvelope::new(
                EventType::TaskEscalatedToHuman,
                Some(serde_json::json!({
                    "task_id": task_id.to_string(),
                    "reason": reply_text.chars().take(500).collect::<String>()
                })),
            )
            .with_correlation(task_id),
        );
    }
    if let Some(reg) = &browser_registry {
        crate::browser::close_task(reg, task_id).await;
    }
    // Release per-task workspace memory immediately — no longer needed once the task finishes.
    if let Some(ws) = &workspace_store {
        ws.write().await.remove(&task_id);
    }

    let verify_user_lang = studio_verify_detect_user_language(clean_message);
    let studio_verify_display_message: Option<String> = if let Some(ref err) = studio_verify_error {
        let explain_enabled = code_studio_disk_task
            && std::env::var("AKASHA_STUDIO_VERIFY_EXPLAIN_FAILURE")
                .ok()
                .map(|v| v != "0")
                .unwrap_or(true);
        let explain = if explain_enabled {
            let _ = bus.send(
                EventEnvelope::new(
                    EventType::ProgressUpdate,
                    Some(serde_json::json!({
                        "task_id": task_id.to_string(),
                        "progress_pct": 99,
                        "message": studio_verify_analyzing_progress_line(verify_user_lang)
                    })),
                )
                .with_correlation(task_id),
            );
            studio_verify_explain_failure_to_user(
                &llm_router,
                clean_message,
                &reply_text,
                err,
                verify_user_lang,
            )
            .await
        } else {
            None
        };
        let summary = explain.filter(|s| !s.trim().is_empty());
        let banner = studio_verify_failure_banner(verify_user_lang);
        let excerpt_lbl = studio_verify_build_excerpt_label(verify_user_lang);
        Some(if let Some(ref s) = summary {
            let head = studio_verify_summary_heading_markdown(verify_user_lang);
            if head.is_empty() {
                format!(
                    "{banner}\n\n{}\n\n{excerpt_lbl}\n{}",
                    s.trim(),
                    err.chars().take(1_400).collect::<String>()
                )
            } else {
                format!(
                    "{banner}\n\n{head}{}\n\n{excerpt_lbl}\n{}",
                    s.trim(),
                    err.chars().take(1_400).collect::<String>()
                )
            }
        } else {
            format!(
                "{banner}\n{}",
                err.chars().take(1_800).collect::<String>()
            )
        })
    } else {
        None
    };

    if let Some(ref pm) = studio_verify_display_message {
        let _ = bus.send(
            EventEnvelope::new(
                EventType::ProgressUpdate,
                Some(serde_json::json!({
                    "task_id": task_id.to_string(),
                    "progress_pct": 100,
                    "message": pm.chars().take(6_000).collect::<String>()
                })),
            )
            .with_correlation(task_id),
        );
    }

    let final_event_type = if matches!(task_status_snapshot, Some(TaskStatus::Cancelled)) {
        EventType::TaskCancelled
    } else if is_paused {
        EventType::TaskPaused
    } else if matches!(task_status_snapshot, Some(TaskStatus::Interrupted)) {
        EventType::TaskPaused
    } else if matches!(task_status_snapshot, Some(TaskStatus::Failed)) {
        EventType::TaskFailed
    } else if studio_verify_error.is_some() {
        EventType::TaskFailed
    } else {
        EventType::TaskCompleted
    };
    let final_status_str = if matches!(task_status_snapshot, Some(TaskStatus::Cancelled)) {
        "cancelled"
    } else if is_paused {
        "paused"
    } else if matches!(task_status_snapshot, Some(TaskStatus::Interrupted)) {
        "interrupted"
    } else if matches!(task_status_snapshot, Some(TaskStatus::Failed)) {
        "failed"
    } else if studio_verify_error.is_some() {
        "failed"
    } else {
        "completed"
    };

    let mut final_payload = serde_json::json!({
        "task_id": task_id.to_string(),
        "status": final_status_str,
        "model_used": last_llm_model_used
    });
    if let Some(ref store) = task_usage_store {
        if let Some(u) = store.get_last_turn(task_id).await {
            if let Some(obj) = final_payload.as_object_mut() {
                obj.insert("prompt_tokens".to_string(), serde_json::json!(u.prompt_tokens));
                obj.insert(
                    "completion_tokens".to_string(),
                    serde_json::json!(u.completion_tokens),
                );
                obj.insert("cost_usd".to_string(), serde_json::json!(u.cost_usd));
                obj.insert("latency_ms".to_string(), serde_json::json!(u.latency_ms));
            }
        }
    }
    if studio_verify_error.is_some() {
        if let Some(obj) = final_payload.as_object_mut() {
            let reason_text: String = studio_verify_display_message
                .as_ref()
                .map(|m| m.chars().take(2_000).collect::<String>())
                .unwrap_or_else(|| {
                    studio_verify_error
                        .as_deref()
                        .unwrap_or("")
                        .chars()
                        .take(2_000)
                        .collect::<String>()
                });
            obj.insert("reason".to_string(), serde_json::Value::String(reason_text));
        }
    }
    let failure_reason = final_payload.get("reason").cloned();
    let _ = bus.send(
        EventEnvelope::new(final_event_type, Some(final_payload)).with_correlation(task_id),
    );
    emit_timeline_once_for_task(
        &bus,
        Some(store_path.as_path()),
        task_id,
        "task_completed",
        Some(serde_json::json!({ "status": final_status_str })),
    );
    if final_status_str == "completed" || final_status_str == "failed" {
        let hook_event = if final_status_str == "completed" {
            "task_completed"
        } else {
            "task_failed"
        };
        let mut hook_payload = serde_json::json!({
            "task_id": task_id.to_string(),
            "session_id": session_id,
            "status": final_status_str,
            "model_used": last_llm_model_used,
            "reply_preview": reply_text.chars().take(800).collect::<String>(),
        });
        if final_status_str == "failed" {
            if let (Some(obj), Some(reason)) = (hook_payload.as_object_mut(), failure_reason.as_ref()) {
                obj.insert("reason".to_string(), reason.clone());
            }
        }
        let hook_data_dir = store_path.parent().unwrap_or_else(|| store_path.as_path());
        crate::plugin_hook_bus::dispatch_hook_event(
            hook_data_dir,
            hook_event,
            &hook_payload.to_string(),
        );
    }
    clear_task_milestones(task_id, Some(store_path.as_path()));

    if let Some(data_dir) = store_path.parent().map(|p| p.to_path_buf()) {
        let sp = store_path.to_path_buf();
        let tid = task_id;
        let _ = tokio::task::spawn_blocking(move || {
            let _ = crate::session_transcript::write_task_transcript(&data_dir, &sp, tid);
        })
        .await;
    }

    // Phase 2 AI OS: do not overwrite Paused / Cancelled / Interrupted with Completed.
    if !halted_user {
        if studio_verify_error.is_some() {
            let _ = store.update_status(task_id, TaskStatus::Failed);
        } else {
            let _ = store.update_status(task_id, TaskStatus::Completed);
            if let Ok(events) = store.get_events(task_id) {
                if let Some(ticket_id) = events
                    .iter()
                    .rev()
                    .find(|e| e.event_type == "studio_ticket_link")
                    .and_then(|e| e.payload.as_ref())
                    .and_then(|p| p.get("ticket_id").and_then(|x| x.as_str()))
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                {
                    let _ = crate::api_studio::studio_mark_ticket_ready_for_review(
                        &tool_disk_workspace_root,
                        &ticket_id,
                        &task_id.to_string(),
                        "system",
                    );
                }
            }
        }
        notify_task_completion(&task_completion_registry, task_id).await;
        if let Some(ref sq) = steering_queue {
            sq.unregister_active(task_id).await;
        }
        let data_dir_sess = store_path.parent().unwrap_or_else(|| store_path.as_ref());
        let is_root_task = store
            .get(task_id)
            .ok()
            .flatten()
            .map(|t| t.parent_task_id.is_none())
            .unwrap_or(true);
        if is_root_task
            && !is_small_talk_fast_lane
            && !is_orchestrated_task_msg
            && studio_verify_error.is_none()
        {
            if let Ok(st) = crate::session_state::merge(data_dir_sess, &session_id, |s| {
                let fact = reply_text.chars().take(240).collect::<String>();
                // Skip storing agent confusion/redirects as facts — they poison future context.
                let fact_lower = fact.to_lowercase();
                let is_confused_redirect = fact_lower.contains("chemin complet")
                    || fact_lower.contains("quel chemin")
                    || fact_lower.contains("pouvez-vous me préciser")
                    || fact_lower.contains("pouvez-vous préciser")
                    || fact_lower.contains("plan du projet")
                    || fact_lower.contains("quel texte voulez")
                    || (fact_lower.contains("fichier") && fact_lower.contains("chemin") && fact_lower.ends_with('?'))
                    || (fact_lower.contains("dossier") && fact_lower.contains("préciser"))
                    // Generic LLM "here is…" answers are one-off responses, not session facts.
                    // Storing them causes context contamination on future unrelated requests.
                    || fact_lower.starts_with("voici ")
                    || fact_lower.starts_with("here is ")
                    || fact_lower.starts_with("here's ")
                    || fact_lower.starts_with("voilà ")
                    || fact_lower.contains("```")
                    || fact_lower.contains("## ")
                    || fact_lower.contains("| étape ")
                    || fact_lower.contains("| step ")
                    || fact_lower.contains("souhaitez-vous")
                    || fact.starts_with("[Task]\n")
                    // Confused assistant responses: clarifying questions, enthusiastic capability
                    // listings, or structured documents that are not user-relevant session facts.
                    || fact_lower.starts_with("could you ")
                    || fact_lower.starts_with("sure! ")
                    || fact_lower.starts_with("sure, ")
                    || fact_lower.starts_with("bien sûr !")
                    || fact_lower.starts_with("bien sur !")
                    || fact_lower.starts_with("bien sûr,")
                    || fact_lower.starts_with("bien sur,")
                    || fact_lower.starts_with("vous pouvez me ")
                    || fact_lower.starts_with("vous pouvez m'")
                    || fact_lower.starts_with("certainly! ")
                    || fact_lower.starts_with("certainly, ")
                    || fact_lower.starts_with("of course! ")
                    || fact_lower.starts_with("of course, ");
                if !fact.trim().is_empty() && !is_confused_redirect {
                    s.facts.push(fact);
                }
            }) {
                let _ = bus.send(
                    EventEnvelope::new(
                        EventType::SessionStateSnapshot,
                        Some(serde_json::json!({
                            "schema_version": 1,
                            "session_id": session_id,
                            "state": st,
                        })),
                    )
                    .with_correlation(task_id),
                );
            }
        }
        if !is_small_talk_fast_lane {
            let outcome_label = if studio_verify_error.is_some() {
                "failed"
            } else {
                "completed"
            };
            let summary_preview: String = if let Some(ref m) = studio_verify_display_message {
                m.chars().take(300).collect()
            } else if let Some(ref e) = studio_verify_error {
                e.chars().take(300).collect()
            } else {
                reply_text.chars().take(300).collect()
            };
            learn_from_task_outcome_async(
                long_term_client.clone(),
                task_id,
                message.clone(),
                outcome_label.to_string(),
                summary_preview,
                Some(session_id.clone()),
                structured.intent_slug.clone(),
            )
            .await;
        }
    }
}
