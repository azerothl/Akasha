//! Main LLM + tools loop (`run_message_via_llm`) extracted from `api.rs` (P3 / v0.11).

use crate::agents::{interpret_message, EventBus, OrchestratorTask};
use crate::api::{
    agent_role_system_prompt, available_tools_instruction,
    build_image_markdown, build_session_recap_reply, cancel_task_watchdogs,
    canonicalize_tool_name, captured_media_suitable_for_vision_injection,
    classify_small_talk_message, compact_short_term_if_needed,
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
    MemoryProfile, PendingHumanInput, ProcessRegistry,
    SessionRecallRange, SmallTalkLanguage, SteeringQueueStore,
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

fn write_file_reminder_for_policy(
    save_file: bool,
    code_studio_disk_task: bool,
    write_file_allowed: bool,
) -> &'static str {
    if save_file && !code_studio_disk_task && write_file_allowed {
        WRITE_FILE_REMINDER
    } else {
        ""
    }
}

async fn studio_round_limit_choice(store: &Option<HumanInputStore>, bus: &EventBus, task: Uuid, round: u32, max: u32) -> Option<String> {
    let store = store.as_ref()?;
    let question = "Limite de tours d'outils atteinte. Souhaitez-vous continuer la tâche ?".to_string();
    let context = format!("La tâche a utilisé {round} tours d'outils (max {max}). Une reprise exige une nouvelle décision ; aucune ancienne approbation n'est réutilisée.");
    let choices = vec!["Continuer (+10 tours)".to_string(), "Réinitialiser et continuer (+10 tours)".to_string(), "Arrêter".to_string()];
    let (tx, rx) = oneshot::channel();
    let request_id=Uuid::new_v4().to_string();
    store.write().await.insert(task, PendingHumanInput { request_id:Some(request_id.clone()), question:question.clone(), context:context.clone(), choices:Some(choices.clone()), response_tx:tx });
    let _ = bus.send(EventEnvelope::new(EventType::TaskWaitingUserInput, Some(serde_json::json!({"task_id":task,"request_id":request_id.clone(),"question":question,"context":context,"choices":choices,"tool_round_limit":true,"current_round":round,"max_tool_rounds":max}))).with_correlation(task));
    let answer = tokio::time::timeout(std::time::Duration::from_secs(300), rx).await.ok().and_then(Result::ok);
    let mut waiting=store.write().await;if waiting.get(&task).is_some_and(|p|p.request_id.as_deref()==Some(request_id.as_str())){waiting.remove(&task);}
    answer
}

fn auto_extend_studio_round_budget(
    store_path: &std::path::Path,
    task_id: Uuid,
    round: u32,
    max_tool_rounds: &mut u32,
    prompt: &str,
    outcome: &crate::task_outcome::RunOutcome,
) -> Result<bool, String> {
    const STUDIO_TOOL_ROUND_HARD_CAP: u32 = 96;
    if *max_tool_rounds >= STUDIO_TOOL_ROUND_HARD_CAP {
        return Ok(false);
    }
    let previous_limit = *max_tool_rounds;
    *max_tool_rounds = round
        .max(*max_tool_rounds)
        .saturating_add(10)
        .min(STUDIO_TOOL_ROUND_HARD_CAP);
    if let Some(mut checkpoint) = crate::studio_recovery::load(store_path, task_id, false)? {
        checkpoint.phase = "auto_continuing_tool_rounds".into();
        checkpoint.waiting = None;
        checkpoint.prompt = prompt.to_owned();
        checkpoint.round = round;
        checkpoint.max_tool_rounds = Some(*max_tool_rounds);
        checkpoint.outcome = outcome.clone();
        crate::studio_recovery::save(store_path, &checkpoint)?;
    }
    insert_task_tracking_event(
        store_path,
        task_id,
        "studio_auto_tool_round_extension",
        serde_json::json!({
            "round": round,
            "previous_limit": previous_limit,
            "new_limit": *max_tool_rounds,
            "hard_cap": STUDIO_TOOL_ROUND_HARD_CAP,
        }),
    );
    Ok(true)
}

fn should_recover_duplicate_read(
    tool: &str,
    repeated_three_times: bool,
    recovery_count: u8,
    recovery_limit: u8,
) -> bool {
    repeated_three_times
        && tool.eq_ignore_ascii_case("read_file")
        && recovery_count < recovery_limit
}

fn should_suppress_repeated_goal_read(
    tool: &str,
    goal_failure_seen: bool,
    successful_reads_for_request: u8,
) -> bool {
    goal_failure_seen
        && tool.eq_ignore_ascii_case("read_file")
        && successful_reads_for_request >= 2
}

fn is_provider_tool_call_parse_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("error parsing tool call")
        || (lower.contains("tool call")
            && lower.contains("invalid character")
            && lower.contains("string literal"))
}

fn sanitized_tool_transport_error(error: &str, provider_parse_error: bool) -> String {
    if provider_parse_error {
        "Provider rejected malformed tool-call JSON before execution".to_string()
    } else {
        error.chars().take(800).collect()
    }
}

fn goal_read_request_key(path: &str, args: &[String]) -> String {
    let (_, window, full) = parse_read_file_args(args);
    let read_shape = if full {
        "full".to_string()
    } else if let Some((offset, limit)) = window {
        format!("window:{offset}:{limit}")
    } else {
        "default".to_string()
    };
    format!("{}|{read_shape}", path.to_ascii_lowercase())
}

fn is_retryable_failed_mutation(tool: &str, result: &str) -> bool {
    let tool = tool.to_ascii_lowercase();
    let result = result.to_ascii_lowercase();
    matches!(
        tool.as_str(),
        "search_replace" | "edit_file" | "apply_patch" | "write_file" | "write_code"
    ) && (result.contains("edit_no_change") || result.contains("stale_file_version"))
}

#[cfg(test)]
mod duplicate_read_loop_tests {
    use super::{
        goal_read_request_key, is_retryable_failed_mutation, should_recover_duplicate_read,
        should_suppress_repeated_goal_read,
    };

    #[test]
    fn gives_bounded_recoveries_for_a_repeated_file_read() {
        assert!(should_recover_duplicate_read("read_file", true, 0, 4));
        assert!(should_recover_duplicate_read("read_file", true, 3, 4));
        assert!(!should_recover_duplicate_read("read_file", true, 4, 4));
        assert!(!should_recover_duplicate_read("read_file", false, 0, 4));
    }

    #[test]
    fn non_read_tool_loops_remain_terminal() {
        assert!(!should_recover_duplicate_read("write_file", true, 0, 4));
        assert!(!should_recover_duplicate_read("run_command", true, 0, 4));
    }

    #[test]
    fn recognizes_failed_edit_retries_even_when_other_tools_intervene() {
        assert!(is_retryable_failed_mutation(
            "search_replace",
            "edit_no_change: the replacement did not change file content"
        ));
        assert!(is_retryable_failed_mutation(
            "edit_file",
            "stale_file_version: expected version is outdated"
        ));
        assert!(!is_retryable_failed_mutation(
            "run_command",
            "edit_no_change"
        ));
        assert!(!is_retryable_failed_mutation("search_replace", "success"));
    }

    #[test]
    fn suppresses_repeated_goal_reads_only_after_two_successful_reads_and_a_failure() {
        assert!(!should_suppress_repeated_goal_read("read_file", false, 3));
        assert!(!should_suppress_repeated_goal_read("read_file", true, 1));
        assert!(should_suppress_repeated_goal_read("read_file", true, 2));
        assert!(!should_suppress_repeated_goal_read("search_files", true, 5));
    }

    #[test]
    fn different_goal_read_windows_are_not_treated_as_duplicate_requests() {
        let first = goal_read_request_key(
            "workspace:/src/game.ts",
            &[
                "workspace:/src/game.ts".to_string(),
                "200".to_string(),
                "40".to_string(),
            ],
        );
        let second = goal_read_request_key(
            "workspace:/src/game.ts",
            &[
                "workspace:/src/game.ts".to_string(),
                "240".to_string(),
                "40".to_string(),
            ],
        );
        let full = goal_read_request_key(
            "workspace:/src/game.ts",
            &["workspace:/src/game.ts".to_string(), "--full".to_string()],
        );

        assert_ne!(first, second);
        assert_ne!(first, full);
        assert_eq!(
            first,
            goal_read_request_key(
                "WORKSPACE:/SRC/GAME.TS",
                &[
                    "workspace:/src/game.ts".to_string(),
                    "200".to_string(),
                    "40".to_string(),
                ],
            )
        );
    }
}

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
    let _active_execution=match crate::studio_recovery::ActiveRun::enter(task_id){Ok(guard)=>guard,Err(error)=>{tracing::warn!(%task_id,%error,"Duplicate task execution refused");return;}};
    let started=match TaskStore::open(&store_path).and_then(|store|store.transition_if(task_id,&[TaskStatus::Pending,TaskStatus::Queued,TaskStatus::Running],TaskStatus::Running)) {
        Ok(started)=>started,
        Err(error)=>{
            let reason=format!("Cannot start task execution: {error}");
            let outcome=serde_json::json!({"schema_version":1,"termination_reason":"context_unavailable","result":"failed","reason":reason,"status":"failed","verification":[],"acceptance_verified":false});
            let persisted=TaskStore::open(&store_path).and_then(|store|store.finalize_outcome(task_id,TaskStatus::Failed,outcome.clone())).is_ok();
            tracing::error!(%task_id,%error,persisted,"Task start persistence failed");
            let _=bus.send(EventEnvelope::new(EventType::TaskFailed,Some(serde_json::json!({"task_id":task_id,"reason":reason,"outcome":outcome,"persisted":persisted}))).with_correlation(task_id));
            notify_task_completion(&task_completion_registry,task_id).await;return;
        }
    };
    if !started {notify_task_completion(&task_completion_registry,task_id).await;return;}

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
    macro_rules! abort_recovery {($reason:expr)=>{{
        let reason=$reason.to_string();
        let proposed=if crate::studio_recovery::load(&store_path,task_id,false).ok().flatten().is_some(){TaskStatus::Interrupted}else{TaskStatus::Failed};
        let actual=TaskStore::open(&store_path).and_then(|store|store.finalize_outcome(task_id,proposed.clone(),serde_json::json!({"schema_version":1,"termination_reason":"context_unavailable","result":if proposed==TaskStatus::Interrupted{"stopped"}else{"failed"},"reason":reason,"verification":[],"acceptance_verified":false}))).map(|(status,_)|status).unwrap_or(proposed);
        let event=match actual {TaskStatus::Cancelled=>EventType::TaskCancelled,TaskStatus::Paused|TaskStatus::Interrupted|TaskStatus::WaitingUserInput=>EventType::TaskPaused,_=>EventType::TaskFailed};
        let _=bus.send(EventEnvelope::new(event,Some(serde_json::json!({"task_id":task_id,"status":actual.as_str(),"reason":reason}))).with_correlation(task_id));
        cancel_task_watchdogs(&mut watchdog_cancel,&mut stall_cancel);notify_task_completion(&task_completion_registry,task_id).await;return;
    }}}
    let studio_execution=match crate::studio_execution::load(&store_path,task_id){Ok(value)=>value,Err(error)=>{abort_recovery!(error);}};
    let selected_model_route=match studio_execution.model.as_ref().map(|model|crate::studio_execution::validate_model(&llm_router,model)).transpose(){Ok(value)=>value,Err(error)=>{abort_recovery!(error);}};
    let mut goal_tracker=match studio_execution.goal.clone().map(|goal|crate::studio_execution::GoalTracker::new(&store_path,task_id,goal)).transpose(){Ok(value)=>value,Err(error)=>{abort_recovery!(error);}};
    let recovery_checkpoint=match crate::studio_recovery::load(&store_path,task_id,true){Ok(value)=>value,Err(error)=>{abort_recovery!(error);}};
    if let Some(checkpoint)=recovery_checkpoint.as_ref(){
        let root=match crate::studio::resolve_studio_project_dir(data_dir_for_studio_flags,&checkpoint.project_id).and_then(|path|path.canonicalize().map_err(|e|e.to_string())){Ok(path)=>path,Err(error)=>{abort_recovery!(error);}};
        if !crate::studio::is_strictly_under_studio_root(&root,&crate::studio::studio_projects_base(data_dir_for_studio_flags)){abort_recovery!("Recorded project escapes Studio project storage");}
        let expected=match checkpoint.workspace.canonicalize(){Ok(path)=>path,Err(error)=>{abort_recovery!(error);}};
        if expected!=root {
            match crate::studio_worktree::restore_for_resume(data_dir_for_studio_flags,&studio_worktree_registry,task_id,&checkpoint.project_id).await {
                Ok(Some(state)) if state.worktree_path.canonicalize().ok().as_ref()==Some(&expected)=>{},
                _=>{abort_recovery!("Cannot restore the recorded task worktree; project-root fallback refused");}
            }
        }
    }
    let mut tool_disk_workspace_root: std::path::PathBuf = {
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
    if let Some(project)=TaskStore::open(&store_path).ok().and_then(|s|s.lineage_root_studio_project_id(task_id).ok().flatten()) {
        tool_disk_workspace_root=match crate::studio_project_context::manifest_workspace(data_dir_for_studio_flags,&project,Some(task_id)) {
            Ok(root)=>root,Err(error)=>{abort_recovery!(error);}
        };
    }
    // A path shape is not a project identity. Match the dispatcher/ledger gate.
    let code_studio_disk_task = TaskStore::open(&store_path).ok()
            .and_then(|store| store.lineage_root_studio_project_id(task_id).ok().flatten()).is_some();
    let mut studio_disk_system_append = {
        if code_studio_disk_task {
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
    let studio_initial_kanban_planning = code_studio_disk_task
        && assigned_agent.eq_ignore_ascii_case("studio_project_manager")
        && (crate::api_studio::studio_is_initial_backlog_planning_message(&message)
            || task_snapshot
                .as_ref()
                .and_then(|task| task.initial_message.as_deref())
                .is_some_and(crate::api_studio::studio_is_initial_backlog_planning_message));
    if studio_initial_kanban_planning {
        // This first turn only refines the seeded backlog. Drop generic
        // implementation reminders so small local models receive the request
        // and the bounded Kanban tool set.
        studio_disk_system_append.clear();
        studio_disk_system_append.push_str(
            "\n\n[Planification Kanban initiale — portée limitée]\n\
             Lis les tickets existants, adapte les gabarits et crée seulement les tickets manquants.\
             Couvre socle, fonctionnalités, intégration et validation avec des critères observables.\
             Utilise parent_ticket_id pour le suivi, depends_on_ticket_ids uniquement pour les vrais prérequis,\
             et une creation_key stable. Relis le Kanban après chaque mise à jour; ne modifie aucun fichier source\
             et ne délègue pas avant la fin de ce backlog.\n",
        );
    } else if code_studio_disk_task && assigned_agent.eq_ignore_ascii_case("studio_project_manager") {
        studio_disk_system_append.push_str(
            "\n\n[Kanban workflow — required]\n\
            Before implementation delegation, read the whole Kanban. When the linked ticket has the initial-kanban-backlog criterion, refine the seeded work tickets before creating missing ones. Ensure at least three focused work tickets with non-empty, verifiable acceptance criteria cover the foundation, requested features, integration, and validation, with at least one real dependency between work tickets. Each work ticket uses parent_ticket_id for the global tracker and a stable creation_key; parent-child tracking does not block dispatch. Use depends_on_ticket_ids only for real execution prerequisites, never for the global tracker. Before creating work, reread the Kanban and reuse the existing canonical ticket ID; repeated requests must not create duplicates. Do not delegate implementation until this backlog exists. Every user evolution has an automatically linked global ticket; do not duplicate it. Before editing source, create or reuse at least one actionable child ticket under that tracker: exactly one for a small change, multiple for independent or sequential work. Give each child observable acceptance criteria, an owner, and a reviewer. A queued follow-up has its own linked tracker and follows the same rule. After every worker result and on every scheduled Kanban pass, compare open tickets with task states, update blockers and dispatch one ready ticket per pass in dependency order. If verification finds a confirmed bug outside the current ticket, create a distinct bug ticket with expected result, observed result, and a reproducible check; do not duplicate a bug already covered by the current ticket. Never mark a ticket done without observed validation evidence."
        );
    } else if code_studio_disk_task
        && matches!(
            assigned_agent.to_ascii_lowercase().as_str(),
            "studio_fullstack" | "studio_frontend" | "studio_backend" | "studio_scaffold" | "code"
        )
    {
        studio_disk_system_append.push_str(
            "\n\n[Kanban bug handoff]\n\
            Stay within the assigned ticket. If inspection or verification confirms a separate out-of-scope bug, report its expected result, observed result, and reproducible check to the project manager so it can be recorded as a separate Kanban ticket. Do not silently fold unrelated work into this ticket or claim the bug is tracked until the manager confirms the ticket."
        );
    }
    let context_message=recovery_checkpoint.as_ref().map(|checkpoint|super::studio_conversation::initial_user_context(&checkpoint.conversation,
        if checkpoint.prompt.contains("[Current user request — task scope]"){&checkpoint.prompt}else{&message})).unwrap_or(&message);
    let focused_file_task=code_studio_disk_task && assigned_agent=="studio_project_manager"
        && !context_message.contains("[Délégation obligatoire (Code Studio")
        && context_message.split("[Current user request — task scope]").next().unwrap_or("").contains(super::DIRECT_FILE_TASK_MARKER)
        && super::scoped_file_request(context_message);
    if focused_file_task {
        // The global quality contract remains, except maintenance templates for
        // plan/design files which are explicitly outside this direct-file mode.
        if let Some((before,rest))=STUDIO_AGENT_QUALITY_REMINDER.split_once("- Fichier `CODE_STUDIO_PLAN.md`") {
            if let Some((_,after))=rest.split_once("- **Corrections sur le disque") {
                studio_disk_system_append=format!("{STUDIO_DISK_REMINDER}{before}- **Corrections sur le disque{after}");
            }
        }
    }
    if let Some(checkpoint)=recovery_checkpoint.as_ref(){
        let project=TaskStore::open(&store_path).ok().and_then(|store|store.lineage_root_studio_project_id(task_id).ok().flatten());
        if checkpoint.agent!=assigned_agent || project.as_deref()!=Some(&checkpoint.project_id) || checkpoint.workspace.canonicalize().ok()!=tool_disk_workspace_root.canonicalize().ok(){abort_recovery!("Checkpoint context no longer matches the task workspace, project or role");}
    }
    let skill_registry = if code_studio_disk_task {
        match &skill_registry {
            Some(registry) => match registry.for_project(&tool_disk_workspace_root).await {
                Ok(scoped) => Some(Arc::new(scoped)),
                Err(error) => {
                    let reason = format!("Cannot load project skills: {error}");
                    if let Ok(store) = TaskStore::open(&store_path) {
                        let _ = store.finalize_outcome(task_id, TaskStatus::Failed, serde_json::json!({"schema_version":1,"termination_reason":"context_unavailable","result":"failed","reason":reason,"verification":[],"acceptance_verified":false}));
                    }
                    let _ = bus.send(EventEnvelope::new(EventType::TaskFailed, Some(serde_json::json!({"task_id":task_id,"reason":reason}))).with_correlation(task_id));
                    cancel_task_watchdogs(&mut watchdog_cancel, &mut stall_cancel);
                    notify_task_completion(&task_completion_registry, task_id).await;
                    return;
                }
            },
            None => None,
        }
    } else { skill_registry };
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
    let router_task_type_early = if code_studio_disk_task && llm_router.primary_route_for_task_type("studio").is_some() {
        "studio".to_string()
    } else if preferred_task_type_override.as_deref()
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
    let message_intent_flags_clean = super::studio_conversation::conversational_intents(clean_message, code_studio_disk_task);
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

    let studio_policy_root = code_studio_disk_task.then_some(tool_disk_workspace_root.as_path());
    let tools_executor_snapshot = match &tools_executor {
        Some(r) => Some(crate::studio_policy::executor((*r.read().await).clone(), studio_policy_root)),
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
        .unwrap_or(if code_studio_disk_task { 8192 } else { 4096 });
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
        let studio_tool_list = if code_studio_disk_task || crate::studio_role_policy::restricted_role(&assigned_agent) {
            tools_executor_snapshot.as_ref().map(|executor| crate::studio_capabilities::announced_names(&executor.policy, &assigned_agent))
        } else {
            None
        };
        let base = if studio_initial_kanban_planning {
            "Kanban uniquement : studio_list_tickets, studio_create_ticket, studio_update_ticket".to_string()
        } else if studio_tool_list.is_some() {
            tools_executor_snapshot.as_ref().map(|executor| crate::studio_capabilities::prompt(&executor.policy, &assigned_agent)).unwrap_or_default()
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
            let native_studio_tools =
                std::env::var("AKASHA_STUDIO_NATIVE_TOOLS").ok().as_deref() != Some("0");
            let skills_part_studio = if studio_initial_kanban_planning {
                String::new()
            } else { match &skill_registry {
                Some(reg) => {
                    let list = reg.for_agent(&assigned_agent).await;
                    if list.is_empty() {
                        String::new()
                    } else {
                        let names: Vec<String> = list.iter().map(|s| format!("{}: {}", s.name, s.description)).collect();
                        format!(
                            " ; Skills (n’utiliser que si pertinent pour ce dépôt ; sinon ignorer) : {}",
                            names.join(", ")
                        )
                    }
                }
                None => String::new(),
            }};
            code_studio_system_tools_block = format!(
                "[Code Studio — outils]\n\
                 Une ligne par invocation : `TOOL: nom_outil arg1 …`.\n\
                 Disponibles : {}{}.\n\
                 Règles :\n\
                 - Outils strictement nécessaires à la demande sur ce dépôt ; pas d’exemples hors sujet.\n\
                 - read_file : par défaut **500 premières lignes** seulement. Fichier entier : `TOOL: read_file <chemin> --full` (plafond octets si très gros). Fenêtre : `TOOL: read_file <chemin> <ligne_début> <nombre_de_lignes>`.\n\
                 - write_file : uniquement si le fichier demandé reste à écrire ; conserver exactement le chemin et le contenu demandés. Première ligne seule `TOOL: write_file <chemin>`, puis le contenu sur les lignes suivantes. Après confirmation de la sauvegarde par le résultat outil, ne pas répéter cette écriture : effectuer les contrôles encore demandés puis conclure en texte.\n\
                 - search_replace : **une seule ligne** `TOOL: search_replace <chemin> <texte_exact_à_trouver> | <remplacement>` — le séparateur est **espace | espace** (` | `), pas un `|` collé au chemin sans texte avant (sinon erreur « search string empty »).\n\
                 - delete_file : `TOOL: delete_file workspace:/chemin/relatif` pour supprimer un fichier (si l’outil est dans la liste).\n\
                 - ask_user : JSON question/context/choices pour continuer la même tâche.\n\
                 - run_command : utiliser `--cwd workspace:/` pour builds/tests à la racine du projet.\n\
                 - write_todos / merge_todos si exposés par la politique.\n\
                 {}\n\
                 Si aucun outil n’est nécessaire, répondre en texte.",
                base, skills_part_studio, run_command_os_rule
            );
            if native_studio_tools {
                code_studio_system_tools_block = if studio_initial_kanban_planning {
                    "[Code Studio — planification Kanban]\nUtilise les fonctions natives annoncées pour lire, adapter ou créer les tickets. N'utilise aucun outil de fichier, shell ou délégation pendant cette phase. Le schéma de chaque fonction décrit ses arguments. Relis le Kanban après chaque modification et conserve les IDs retournés.\n".to_string()
                } else {
                    tools_executor_snapshot.as_ref()
                        .map(|executor| crate::studio_capabilities::native_prompt(&executor.policy,&assigned_agent))
                        .unwrap_or_default()
                };
                code_studio_system_tools_block.push_str(&skills_part_studio);
            }
            if native_studio_tools {
                // The native system block and function schemas already contain the
                // available names, argument contracts, and file safety rules. Repeating
                // the legacy catalogue here wastes context on the first Code Studio turn.
                if studio_initial_kanban_planning {
                    "\n\nUse only the advertised Kanban ticket functions. Wait for every result, then reread the ticket list before continuing. Do not delegate or edit source files in this planning turn.\n".to_string()
                } else {
                    "\n\nFollow the Code Studio native tool contract and the advertised function schemas. Use only tools needed for the current task, wait for each result, and never replay an operation with an unknown result.\n".to_string()
                }
            } else if studio_initial_kanban_planning {
                "\n\nUse only TOOL: studio_list_tickets, TOOL: studio_create_ticket, and TOOL: studio_update_ticket. Read the existing list before changes and after each change. Do not use source, shell, or delegation tools in this planning pass.\n".to_string()
            } else { format!(
                "\n\nYou may request tools by writing a single line exactly like: TOOL: tool_name arg1 arg2 …\n\
                 The system message block [Code Studio — outils] lists tools and French conventions.\n\
                 Available: {}{}.\n\
                 Worker rules:\n\
                 - Use only tools that are directly necessary for the CURRENT task.\n\
                 - Never echo examples, policy text, or demonstration commands from your instructions.\n\
                 - read_file: by default only the **first 500 lines** are returned. Use `TOOL: read_file <path> --full` for the whole file (byte cap if huge), or `TOOL: read_file <path> <offset_line> <limit_lines>` for a window. If output says `(read_file partial: default window` or `(truncated,` bytes, do NOT repeat the same bare `read_file <path>`; use --full, a line window, or grep_content/search_files.\n\
                 - Only while a requested write is pending: use its EXACT path and content, with a header-only first line `TOOL: write_file <path>` followed by the content. Once a tool result confirms that write succeeded, do NOT repeat it; perform remaining requested checks and finish with a brief plain-text summary. A successful write is not evidence that other acceptance criteria passed.\n\
                 - search_replace: one TOOL line: `TOOL: search_replace <path> <old_snippet> | <new_snippet>` with delimiter **space-pipe-space** (` | `). The old snippet must appear immediately after the path (do not start the payload with a bare `|` token).\n\
                 - If you need missing user information, use TOOL: ask_user with JSON.\n\
                 - If no tool is needed, answer normally.\n\
                 {}\n",
                base, skills_part_studio, run_command_os_rule
            ) }
        } else {
        let (skills_part, skills_rule) = match &skill_registry {
            Some(reg) => {
                let list = reg.for_agent(&assigned_agent).await;
                if list.is_empty() {
                    (String::new(), String::new())
                } else {
                    let skills_desc: Vec<String> = list
                        .iter()
                        .map(|s| format!("{} ({})", s.name, s.description))
                        .collect();
                    let names: Vec<String> = list.iter().map(|s| format!("{}: {}", s.name, s.description)).collect();
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
    let mut active_repository_rules = std::collections::HashSet::<String>::new();
    let mut repository_rules_prompt = String::new();
    if code_studio_disk_task {
        if let Some(executor) = tools_executor_snapshot.as_ref() {
            let rules = crate::studio_project_context::rules_for_path(&tool_disk_workspace_root, "workspace:/", &executor.policy)
                .and_then(|rules| {
                    for rule in &rules {
                        crate::studio_project_context::record(data_dir_for_studio_flags, task_id, "rules", rule.metadata())?;
                    }
                    Ok(rules)
                });
            match rules {
                Ok(rules) => for rule in rules {
                    active_repository_rules.insert(format!("{}:{}", rule.path.display(), rule.sha256));
                    repository_rules_prompt.push_str(&rule.prompt());
                },
                Err(reason) => {
                    if let Ok(store) = TaskStore::open(&store_path) {
                        let _ = store.finalize_outcome(task_id, TaskStatus::Failed, serde_json::json!({"schema_version":1,"termination_reason":"context_unavailable","result":"failed","reason":reason,"verification":[],"acceptance_verified":false}));
                    }
                    let _ = bus.send(EventEnvelope::new(EventType::TaskFailed, Some(serde_json::json!({"task_id":task_id,"reason":reason}))).with_correlation(task_id));
                    cancel_task_watchdogs(&mut watchdog_cancel, &mut stall_cancel);
                    notify_task_completion(&task_completion_registry, task_id).await;
                    return;
                }
            }
        }
    }
    let mut system_prompt = String::with_capacity(8192);
    if studio_initial_kanban_planning {
        system_prompt.push_str(
            "[Code Studio — planification Kanban initiale]\n\
             Transforme la première demande utilisateur en tickets de travail concrets.\
             Respecte sa demande et les consignes du dépôt; garde `workspace:/` comme racine.\
             Utilise uniquement les outils Kanban annoncés, réponds en français et ne modifie aucun fichier source.\n\n",
        );
    } else if code_studio_disk_task {
        system_prompt.push_str(CODE_STUDIO_APP_CONTEXT);
    } else if !code_studio_disk_task && embedded_compact {
        system_prompt.push_str(EMBEDDED_APP_CONTEXT);
    } else {
        system_prompt.push_str(APP_CONTEXT);
    }
    if !embedded_compact && !studio_initial_kanban_planning {
        system_prompt.push_str(os_env_block);
    }
    let role_prompt=if focused_file_task {
        Some("You are the Code Studio project manager in direct file execution mode. Execute the user's explicitly named file operations yourself, using the real tools and current permissions. Follow repository instructions and preserved project constraints. Include every explicitly requested read in its requested order: a failed read or a read before creation does not count as a post-write verification. Create a new file with expected_version=missing, then perform the requested read successfully before proceeding to its next step. Preserve exact file bytes: no headings, dates, project templates or explanations unless requested. Do not add unrelated plans, tickets, designs or delegation outside the user's request. Finish only after all requested operations actually ran; do not repeat confirmed writes.")
    } else if studio_initial_kanban_planning {
        Some("You are doing only the initial Kanban planning pass for a Code Studio project. Read the existing tickets, refine generic starters, and create only missing work tickets. Cover foundation, requested features, integration, and verification with observable acceptance criteria. Use parent_ticket_id for grouping, stable creation_key values, and depends_on_ticket_ids only for genuine execution prerequisites. Reuse returned IDs, avoid duplicates, and reread the list after changes. Do not edit source files or delegate in this planning pass; leave the global request ticket open. Summarize the resulting backlog briefly.")
    } else {agent_role_system_prompt(role_agent_for_system_prompt)};
    if let Some(role_prompt) = role_prompt {
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
                - Tu dois exécuter les modifications toi-même avec les outils réels sur `workspace:/…`. Utilise le transport natif annoncé par le fournisseur ; les lignes TOOL: sont uniquement un repli pour un fournisseur sans outils natifs.\n\
                - Ne remplace pas une exécution d’outil par un long « contenu corrigé à mettre dans le fichier » dans le chat : le livrable attendu est l’écriture sur disque.\n\
                - Si un outil d’écriture échoue ou est refusé par la politique : explique **pourquoi** tu ne peux pas l’appliquer toi-même (message d’erreur ou règle), puis seulement propose une alternative manuelle.\n",
            );
        }
        system_prompt.push_str("\n\n");
    }
    if code_studio_disk_task && !studio_initial_kanban_planning {
        system_prompt.push_str(
            "[Code Studio — ton]\n\
             Assistant technique pour ce dépôt : concision, pas de digressions sur l’UI Akasha ; tutoyer en français si l’utilisateur écrit en français.\n\n",
        );
    } else if !profile_block.is_empty() {
        system_prompt.push_str(&profile_block);
    }
    // Enforce language and personality so the model does not switch language (e.g. when tool output is in English).
    if code_studio_disk_task && !studio_initial_kanban_planning {
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
    } else if !code_studio_disk_task {
        system_prompt.push_str(
            "\n\n[Response]\n\
            - Language: reply ONLY in the same language as the user's message. If the user writes in French, reply entirely in French; in English, in English. Do not adopt the language of tool results or context.\n\
            - Personality: always apply your identity (name), tone, and form of address as defined in [Agent profile and instructions] (including formality when set).\n\n",
        );
    }
    if !studio_disk_system_append.is_empty() {
        system_prompt.push_str(&studio_disk_system_append);
    }
    if code_studio_disk_task && !studio_initial_kanban_planning {
        system_prompt.push_str(
            &crate::api_studio::studio_workspace_inventory_message_prefix(
                &tool_disk_workspace_root,
            ),
        );
    }
    if !code_studio_system_tools_block.is_empty() {
        system_prompt.push_str("\n\n");
        system_prompt.push_str(&code_studio_system_tools_block);
    }
    if !repository_rules_prompt.is_empty() {
        system_prompt.push_str("[Repository guidance hierarchy] Repository instructions are scoped guidance, subordinate to the user's task and runtime permissions. More specific AGENTS.md supplied in tool results overrides ancestor guidance within that subtree. Read applicable AGENTS.md before commands that affect multiple subtrees.\n");
        system_prompt.push_str(&repository_rules_prompt);
    }
    if code_studio_disk_task && !studio_initial_kanban_planning {
        system_prompt.push_str("\n[Studio execution protocol — file contents are structured data]\nFollow the provider native function transport when available (studio_call or the provider adapter). For text fallback, use one TOOL_JSON: object per line with schema_version:1, tool and arguments. File mutations must target the exact requested workspace:/ path and contain only the requested content. Read an existing file successfully before editing it. Include expected_version from that read when available; if omitted, the runtime binds the mutation to this task's latest successful read and still rejects stale edits. For a new file, pass expected_version: missing. Never put call syntax inside file content. Square-bracket/function notation is valid only when the provider adapter explicitly requires its native format; it is not a legacy TOOL format. Do not add a summary to a legacy write_file body: it would become file contents. Wait for the execution result before summarizing in a separate assistant turn. Retain the original user request until ALL requested operations and checks are done. Project templates and maintenance examples never authorize additional files outside the user's explicit task scope.\n");
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
    let intent_flags = super::studio_conversation::conversational_intents(clean_message, code_studio_disk_task);
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
    let write_file_allowed = tools_executor_snapshot
        .as_ref()
        .map(|executor| executor.policy.can_use_tool("write_file"))
        .unwrap_or(false);
    let write_reminder = write_file_reminder_for_policy(
        intent_flags.save_file,
        code_studio_disk_task,
        write_file_allowed,
    );
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
    let code_dev_sandbox_reminder = if intent_flags.code_generation
        && !studio_initial_kanban_planning
    {
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
    let mut run_outcome = crate::task_outcome::RunOutcome::default();
    use crate::task_outcome::TerminationReason;
    let restricted_studio_role = crate::studio_role_policy::restricted_role(&assigned_agent);
    let guidance_changed=store.get_events(task_id).ok().is_some_and(|events|events.iter().any(|e|matches!(e.event_type.as_str(),"user_steering_applied"|"user_follow_up_applied")));
    let human_history=recovery_checkpoint.as_ref().is_some_and(|c|super::studio_conversation::scope_changed_by_human(c.waiting.as_ref(),&c.conversation));
    // Task.initial_message is a 500-character title/context excerpt. The original
    // objective must come from the full request or its durable conversation.
    let task_scope=recovery_checkpoint.as_ref().and_then(|c| {
        c.conversation.iter().find(|m|matches!(&m.role,akasha_llm::MessageRole::User) && m.content.contains("[Current user request — task scope]"))
            .map(|m|m.content.as_str()).or_else(||c.prompt.contains("[Current user request — task scope]").then_some(c.prompt.as_str()))
    }).unwrap_or(&user_message).to_string();
    let mut scope_unchanged=!guidance_changed && !human_history;
    let mut explicit_file_goals=if code_studio_disk_task && !restricted_studio_role && !message.contains("[Délégation obligatoire (Code Studio") && !guidance_changed && !human_history {
        super::studio_completion::file_goals(&task_scope)
    } else {vec![]};
    let explicit_file_literals=if !explicit_file_goals.is_empty() {super::studio_completion::exact_file_literals(&task_scope)}else{vec![]};

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
        // Code Studio implementation tasks need enough bounded iterations to inspect,
        // change, and verify a real project instead of stopping at the chat default.
        if code_studio_disk_task {
            let studio_budget = std::env::var("AKASHA_MAX_TOOL_ROUNDS_STUDIO")
                .ok()
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(64)
                .clamp(32, 96);
            max_tool_rounds = max_tool_rounds.max(studio_budget).min(96);
        }
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
        // An explicitly budgeted goal already authorizes bounded continuation.
        // Do not interrupt it at the unrelated conversational ten-round default.
        if let Some(goal) = goal_tracker.as_ref() {
            max_tool_rounds = crate::studio_execution::goal_tool_round_budget(max_tool_rounds, &goal.goal);
        }
        let mut conversation = recovery_checkpoint.as_ref().map(|c|c.conversation.clone()).unwrap_or_default();
        let mut resume_overlay=String::new();
        if let Some(checkpoint)=recovery_checkpoint.as_ref(){
            let ledger=match crate::studio_tool_contract::recovery_summary(&store_path,task_id){Ok(value)=>crate::studio_tool_contract::recovery_prompt_summary(value),Err(error)=>{abort_recovery!(error);}};
            resume_overlay=format!("[Resumed execution — apply CURRENT system/rules/permissions; do not replay operations]\n[Recorded operation results; unknown effects require operator resolution]\n{}\n[Pending human input at interruption: {}. Ask again; no old approval is restored.]\n[CURRENT task instructions]\n{}\n[Saved execution state]\n",ledger,checkpoint.waiting.as_ref().map(|v|v.to_string()).unwrap_or_else(||"none".into()),current_prompt);
            current_prompt=checkpoint.prompt.clone();
            run_outcome=checkpoint.outcome.clone();run_outcome.termination=TerminationReason::Success;

        }
        if let Some(saved)=recovery_checkpoint.as_ref().and_then(|checkpoint|checkpoint.max_tool_rounds) {max_tool_rounds=saved;}
        let mut round = recovery_checkpoint.as_ref().map(|checkpoint|checkpoint.round).unwrap_or(0);
        let mut recovered_model_route=run_outcome.checks.iter().rev().find(|check|check["kind"]=="provider_route_recovered")
            .and_then(|check|Some(akasha_llm::config::RouteEntry{provider:check["provider"].as_str()?.into(),model:check["model"].as_str()?.into(),config:None}));
        // Tours d'affilée avec uniquement des outils d'exploration (Code Studio).
        let mut studio_read_only_streak_rounds: u32 = 0;
        let mut studio_goal_failure_seen = goal_tracker.as_ref().is_some_and(|goal|goal.state.cycles>0);
        let mut studio_repair_focus_next_turn = recovery_checkpoint.is_some() && studio_goal_failure_seen;
        let mut social_snapshot_seen = false;
        // Loop detection history is tracked per "agent key".
        // For now, agent key = current task_id (sub-agent tasks each have their own task_id).
        let mut tool_loop_history_by_agent: std::collections::HashMap<
            String,
            Vec<(String, String)>,
        > = std::collections::HashMap::new();
        let mut duplicate_read_recovery_by_agent =
            std::collections::HashMap::<(String, String), u8>::new();
        // A failed goal may need a second source read, but repeated successful
        // reads of the same unchanged path must eventually yield to a repair.
        let mut successful_goal_reads_by_path = std::collections::HashMap::<String, u8>::new();
        // Models sometimes reread between retries, which defeats a consecutive
        // tool-call loop detector. Remember exact file mutations rejected as
        // unchanged/stale and suppress the same retry before executing it.
        let mut failed_mutation_retries_by_agent =
            std::collections::HashMap::<(String, String), (u8, String)>::new();
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
        let mut physical_completion_retries=0u32;
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
                } else if code_studio_disk_task {
                    // Native function arguments may be buffered until the call
                    // is complete; honor the configured overall model deadline.
                    llm_timeout_secs
                } else {
                    llm_timeout_secs.min(300)
                }
            });

        let mut resumed_limit_wait = recovery_checkpoint.as_ref().and_then(|checkpoint| {
            checkpoint.waiting.as_ref().filter(|v|v["tool_round_limit"]==true).cloned()
                .or_else(||(checkpoint.round>=max_tool_rounds).then(||serde_json::json!({"tool_round_limit":true,"max_tool_rounds":max_tool_rounds})))
        });
        'tool_rounds: loop {
            if let Some(goal)=goal_tracker.as_mut() {
                if goal.exhausted() { run_outcome.termination=TerminationReason::BudgetExhausted;reply_text="Limite de cycles, de durée ou de tokens de l’objectif atteinte ; réussite non confirmée.".into();let _=goal.save(&store_path,task_id,"budget_exhausted",&reply_text);break 'tool_rounds; }
                if let Err(error)=goal.save(&store_path,task_id,"active","Exécution ou vérification en cours") {run_outcome.termination=TerminationReason::ContextUnavailable;reply_text=error;break 'tool_rounds;}
            }
            if code_studio_disk_task {if let Some(exec)=tools_executor.as_ref(){let policy=crate::studio_policy::for_workspace(&exec.read().await.policy, &tool_disk_workspace_root);if let Err(error)=crate::studio_file_effects::refresh(&store_path,task_id,&tool_disk_workspace_root,super::studio_conversation::user_request(&task_scope),&assigned_agent,&policy,&mut run_outcome){run_outcome.termination=TerminationReason::ContextUnavailable;reply_text=error;break 'tool_rounds;}}}

            if let Some(waiting) = resumed_limit_wait.take() {
                max_tool_rounds = waiting["max_tool_rounds"].as_u64().map(|v|v as u32).unwrap_or(max_tool_rounds);
                if code_studio_disk_task {
                    match auto_extend_studio_round_budget(
                        &store_path,
                        task_id,
                        round,
                        &mut max_tool_rounds,
                        &current_prompt,
                        &run_outcome,
                    ) {
                        Ok(true) => continue 'tool_rounds,
                        Ok(false) => {}
                        Err(error) => {
                            run_outcome.termination = TerminationReason::ContextUnavailable;
                            reply_text = error;
                            break 'tool_rounds;
                        }
                    }
                }
                if let Err(error)={crate::studio_recovery::record_waiting(&store_path,task_id,"waiting_tool_round_limit",waiting,(&current_prompt,round,max_tool_rounds,&run_outcome))} {run_outcome.termination=TerminationReason::ContextUnavailable;reply_text=error;break 'tool_rounds;}
                match studio_round_limit_choice(&human_input_store, &bus, task_id, round, max_tool_rounds).await.as_deref() {
                    Some("Continuer (+10 tours)") => max_tool_rounds = round.max(max_tool_rounds).saturating_add(10),
                    Some("Réinitialiser et continuer (+10 tours)") => { round=0; max_tool_rounds=max_tool_rounds.saturating_add(10); }
                    _ => { run_outcome.termination=TerminationReason::ToolRoundLimit; reply_text="Tool round continuation declined or timed out.".into(); break 'tool_rounds; }
                }
            }
            match store.get(task_id).ok().flatten().map(|t| t.status) {
                Some(TaskStatus::Paused)
                | Some(TaskStatus::Cancelled)
                | Some(TaskStatus::Interrupted)
                | Some(TaskStatus::WaitingUserInput)
                | Some(TaskStatus::Failed) => {
                    break 'tool_rounds;
                }
                _ => {}
            }
            if let Some(ref sq) = steering_queue {
                let steering_items = sq.drain_steering(task_id).await;
                for item in steering_items {
                    explicit_file_goals.clear();scope_unchanged=false;
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
                        run_outcome.termination = TerminationReason::BudgetExhausted;
                        reply_text = "Budget dépassé pour cette session (AKASHA_MAX_COST_PER_SESSION_USD). Démarrez une nouvelle session ou augmentez le plafond.".to_string();
                        break 'tool_rounds;
                    }
                }
                if let Some(max_tokens) = std::env::var("AKASHA_MAX_TOKENS_PER_SESSION")
                    .ok()
                    .and_then(|s| s.parse::<u64>().ok())
                {
                    if max_tokens > 0 && session_tokens >= max_tokens {
                        run_outcome.termination = TerminationReason::BudgetExhausted;
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
            if let Some((provider, model)) = selected_model_route.as_ref()
                .or(recovered_model_route.as_ref())
                .map(|route|(route.provider.clone(),route.model.clone()))
                .or_else(||preferred_task_type.as_deref().and_then(|tt|llm_router.primary_route_for_task_type(tt)))
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
            if code_studio_disk_task {
                let project=TaskStore::open(&store_path).ok().and_then(|store|store.lineage_root_studio_project_id(task_id).ok().flatten());
                let Some(project_id)=project else {run_outcome.termination=TerminationReason::ContextUnavailable;reply_text="Studio project identity unavailable at checkpoint".into();break 'tool_rounds;};
                let checkpoint=crate::studio_recovery::Checkpoint{schema_version:1,task_id,project_id,workspace:tool_disk_workspace_root.clone(),agent:assigned_agent.clone(),session_id:session_id.clone(),preferred_task_type:preferred_task_type_override.clone(),incognito,round,max_tool_rounds:Some(max_tool_rounds),prompt:current_prompt.clone(),conversation:conversation.clone(),phase:"before_model".into(),waiting:None,outcome:run_outcome.clone()};
                if let Err(error)=crate::studio_recovery::save(&store_path,&checkpoint){run_outcome.termination=TerminationReason::ContextUnavailable;reply_text=error;break 'tool_rounds;}
            }
            if round>=max_tool_rounds {run_outcome.termination=TerminationReason::ToolRoundLimit;reply_text="Tool round budget already exhausted at checkpoint; start a new scoped task to continue.".into();break 'tool_rounds;}
            let literal_only=focused_file_task && scope_unchanged && active_repository_rules.is_empty() && super::studio_completion::literal_file_operations_only(&task_scope);
            let repair_focus_this_turn = studio_repair_focus_next_turn;
            if repair_focus_this_turn {
                current_prompt.push_str("\n[Stalled goal repair phase] Repeated surveys or failed checks have not changed the project. This turn prioritizes already permitted repair functions and retains source reads and verification commands needed for the repair. Read an implicated file when needed, then prefer one small targeted correction with search_replace, edit_file or apply_patch using the inspected current version. Preserve unrelated code and avoid a whole-file rewrite when a patch suffices, unless the original request explicitly requires a full rewrite. Do not repeat the failed command. No completed write is implied by a plan. If the recorded blocker is truly irreparable with these permissions, report that precise blocker.\n");
            }
            let native_tools=if code_studio_disk_task && std::env::var("AKASHA_STUDIO_NATIVE_TOOLS").ok().as_deref()!=Some("0") {
                tools_executor_snapshot.as_ref().and_then(|exec| {
                    let mut names:Vec<String>=crate::studio_capabilities::catalogue(&exec.policy,&assigned_agent).into_iter().filter(|row|row.announced && crate::studio_tool_contract::supports_typed(&row.name)).map(|row|row.name).collect();
                    names=if studio_initial_kanban_planning {
                        crate::studio_capabilities::select_initial_kanban_names(names)
                    } else {
                        crate::studio_capabilities::select_native_names(names,
                            run_outcome.checks.iter().any(|check|check["kind"]=="native_catalogue_discovery"))
                    };
                    if repair_focus_this_turn && !literal_only {
                        names=crate::studio_capabilities::select_repair_names(names);
                        insert_task_tracking_event(&store_path,task_id,"studio_native_tool_catalog",serde_json::json!({"selection":"stalled_goal_repair","tools":names}));
                    }
                    if literal_only {
                        names.retain(|name|matches!(name.as_str(),"read_file"|"write_file"|"write_code"));
                        insert_task_tracking_event(&store_path,task_id,"studio_native_tool_catalog",serde_json::json!({"selection":"literal_file_only","literal_values_bound":true,"tools":names}));
                    }
                    (!names.is_empty()).then(||vec![if literal_only {crate::studio_tool_contract::literal_native_definition(names,&explicit_file_literals)}else{crate::studio_tool_contract::native_definition(names)}])
                })
            }else{None};
            let mut turn_tool_names: Option<Vec<String>> = native_tools.as_ref().and_then(|tools|tools.first()?.pointer("/function/parameters/properties/tool/enum")?.as_array().map(|names|names.iter().filter_map(|name|name.as_str().map(str::to_owned)).collect()));
            // Workflow tools use the text protocol. Include their effective
            // announced names for ordinary turns, without widening literal-only turns.
            if !literal_only && !studio_initial_kanban_planning {
                if let (Some(names),Some(exec))=(turn_tool_names.as_mut(),tools_executor_snapshot.as_ref()) {
                    names.extend(crate::studio_capabilities::catalogue(&exec.policy,&assigned_agent).into_iter().filter(|row|row.announced && !crate::studio_tool_contract::supports_typed(&row.name)).map(|row|row.name));
                }
            }
            let request = CompletionRequest {
                require_tool: code_studio_disk_task && (goal_tracker.is_some() || (run_outcome.requires_file_effect() && !run_outcome.confirmed_file_mutation())
                    || (!explicit_file_goals.is_empty() && super::studio_completion::requires_tool_retry(&current_prompt,&run_outcome.checks)))
                    && (matches!(assigned_agent.as_str(),"studio_scaffold"|"studio_frontend"|"studio_backend"|"studio_fullstack"|"code")
                        || (assigned_agent == "studio_project_manager" && !message.contains("[Délégation obligatoire (Code Studio")))
                    && native_tools.as_ref().is_some_and(|tools|tools[0].pointer("/function/parameters/properties/tool/enum").and_then(|v|v.as_array()).is_some_and(|names|names.iter().any(|name|matches!(name.as_str(),Some("write_file"|"write_code"|"edit_file"|"search_replace"|"apply_patch"))))),
                conversation: code_studio_disk_task.then(||conversation.clone()),
                prompt: if code_studio_disk_task {
                    format!("{}{}", resume_overlay, current_prompt)
                } else {
                    format!("{}{}{}", resume_overlay, current_prompt, tool_instruction)
                },
                max_tokens: Some(goal_tracker.as_ref().map(|goal|completion_max_tokens.min(goal.goal.max_tokens.saturating_sub(goal.state.tokens_used) as u32)).unwrap_or(completion_max_tokens)),
                temperature: Some(completion_temperature),
                preferred_task_type,
                // Studio's system block already contains the effective catalogue.
                // Repeating it doubles the context and its executable examples.
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
                tools: native_tools,
            };
            // Streaming path: single forwarder thread → tokio channel (avoids spawn_blocking per chunk).
            // Overall deadline bounds the full generation; idle timeout bounds inter-chunk wait.
            // Native providers may buffer a whole file-write argument before
            // yielding a chunk. The request has its own configured deadline;
            // handing off to that bounded wait is real worker activity, not a
            // claim that files changed or the goal succeeded.
            if code_studio_disk_task && request.tools.as_ref().is_some_and(|tools|!tools.is_empty()) {
                crate::api::cancel_progress_watchdog(&mut watchdog_cancel);
                crate::studio_execution::mark_bounded_native_wait(&meaningful_progress_flag,llm_timeout_secs);
                insert_task_tracking_event(&store_path,task_id,"bounded_native_model_wait",serde_json::json!({"round":round+1,"timeout_secs":llm_timeout_secs,"files_changed":false}));
            }
            let delivered_prompt = request.prompt.clone();
            let estimated_request_tokens=serde_json::to_vec(&request).map(|bytes|bytes.len() as u64).unwrap_or(request.prompt.len() as u64);
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
            let selected_route = selected_model_route.clone();
            let recovered_route = recovered_model_route.clone();
            let turn_budget = std::time::Duration::from_secs(goal_tracker.as_ref().map(|goal|llm_timeout_secs.min(goal.goal.max_seconds.saturating_sub(goal.state.elapsed_ms/1000).max(1))).unwrap_or(llm_timeout_secs));
            let attempt_state = std::sync::Arc::new(std::sync::Mutex::new((serde_json::Value::Null, std::time::Instant::now())));
            let observer_state = attempt_state.clone();
            let observer_store = store_path.clone();
            let observer_bus = bus.clone();
            let observer: akasha_llm::AttemptObserver = std::sync::Arc::new(move |event| {
                if let Ok(mut state) = observer_state.lock() {
                    if matches!(event["phase"].as_str(), Some("qualifying"|"started"|"retry"|"fallback")) {state.1=std::time::Instant::now();}
                    state.0=event.clone();
                }
                insert_task_tracking_event(&observer_store, task_id, "llm_provider_attempt", event.clone());
                if matches!(event["phase"].as_str(), Some("qualifying"|"started"|"retry"|"fallback")) {
                    let action=match event["phase"].as_str() {Some("qualifying")=>"Qualification des outils (sans opération projet)",Some("retry")=>"Nouvelle tentative",Some("fallback")=>"Repli automatique",_=>"Appel au modèle"};
                    let message=format!("{action} : {} / {} · tentative {}/{}",event["provider"].as_str().unwrap_or("?"),event["model"].as_str().unwrap_or("?"),event["attempt"],event["max_attempts"]);
                    let _=observer_bus.send(EventEnvelope::new(EventType::ProgressUpdate,Some(serde_json::json!({"task_id":task_id,"progress_pct":24,"message":message}))).with_correlation(task_id));
                }
            });
            let mut stream_join = tokio::spawn(async move {
                if code_studio_disk_task {
                    router.complete_stream_bounded(&request,selected_route.as_ref(),recovered_route.as_ref(),stream_tx,turn_budget.saturating_sub(std::time::Duration::from_millis(100)),observer).await
                } else if let Some(route) = selected_route { router.complete_stream_for_entry(&request, &route, stream_tx).await }
                else { router.complete_stream(&request, stream_tx).await }
            });
            let mut accumulated = String::new();
            let mut first_wait = true;
            let overall_deadline = tokio::time::Instant::now() + turn_budget;
            let timeout_message=|| {
                if code_studio_disk_task {
                    let state=attempt_state.lock().ok();
                    let route=state.as_ref().map(|state|format!("{} / {}",state.0["provider"].as_str().unwrap_or("inconnu"),state.0["model"].as_str().unwrap_or("inconnu"))).unwrap_or_else(||"fournisseur inconnu".into());
                    format!("provider_generation_failed: {route} : délai de génération dépassé après {} s. Vérifiez que le service répond et que la mémoire disponible suffit, puis reprenez cette tâche depuis son checkpoint. Les événements conservent les tentatives et les modèles appelés.",turn_budget.as_secs())
                } else {format!("LLM response timed out after {} seconds.",llm_timeout_secs)}
            };
            let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(15));
            heartbeat.tick().await;
            loop {
                // Check the overall deadline before waiting for a chunk to avoid spurious zero-duration timeouts.
                if tokio::time::Instant::now() >= overall_deadline {
                    tracing::warn!(
                        timeout_secs = llm_timeout_secs,
                        "Overall LLM timeout exceeded; aborting task"
                    );
                    stream_join.abort();
                    run_outcome.termination = TerminationReason::Timeout;
                    reply_text = if code_studio_disk_task || accumulated.is_empty() {
                        timeout_message()
                    } else {
                        accumulated
                    };
                    break 'tool_rounds;
                }
                let idle = if code_studio_disk_task {
                    overall_deadline.saturating_duration_since(tokio::time::Instant::now())
                } else if first_wait {
                    first_wait = false;
                    std::time::Duration::from_secs(first_chunk_timeout_secs)
                } else {
                    std::time::Duration::from_secs(idle_timeout_secs)
                };
                let receive = tokio::time::timeout(idle.min(overall_deadline.saturating_duration_since(tokio::time::Instant::now())), tok_rx.recv());
                tokio::pin!(receive);
                let received = loop {
                    tokio::select! {
                        result = &mut receive => break result,
                        _ = heartbeat.tick(), if code_studio_disk_task => {
                            if let Ok(state)=attempt_state.lock() {
                                let event=&state.0;
                                if event.is_null() {continue;}
                                let elapsed=state.1.elapsed().as_secs();
                                let activity=if event["phase"]=="first_token" {"génération en cours"}else{"attente de réponse"};
                                let message=format!("{} / {} : {activity} depuis {elapsed} s · tentative {}/{}.",event["provider"].as_str().unwrap_or("?"),event["model"].as_str().unwrap_or("?"),event["attempt"],event["max_attempts"]);
                                insert_task_tracking_event(&store_path,task_id,"llm_wait_heartbeat",serde_json::json!({"provider":event["provider"],"model":event["model"],"attempt":event["attempt"],"elapsed_secs":elapsed,"phase":event["phase"],"files_changed":false}));
                                let _=bus.send(EventEnvelope::new(EventType::ProgressUpdate,Some(serde_json::json!({"task_id":task_id,"progress_pct":24,"message":message}))).with_correlation(task_id));
                            }
                        }
                    }
                };
                match received {
                    Ok(Some(chunk)) => {
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
                stream_join.abort();
                run_outcome.termination = TerminationReason::Timeout;
                tracing::warn!(
                    timeout_secs = llm_timeout_secs,
                    "Overall LLM timeout on stream completion"
                );
                reply_text = if code_studio_disk_task || accumulated.is_empty() {
                    timeout_message()
                } else {
                    accumulated
                };
                break;
            } else {
                match tokio::time::timeout(remaining, &mut stream_join).await {
                    Ok(Ok(Ok(resp))) => {
                        if let Some(goal)=goal_tracker.as_mut() {
                            let used=resp.usage.as_ref().map(|usage|usage.prompt_tokens.saturating_add(usage.completion_tokens)).filter(|count|*count>0).unwrap_or(estimated_request_tokens.saturating_add(resp.text.len() as u64));
                            goal.state.tokens_used=goal.state.tokens_used.saturating_add(used);
                            if let Err(error)=goal.save(&store_path,task_id,"active","Réponse modèle reçue; consommation enregistrée") {run_outcome.termination=TerminationReason::ContextUnavailable;reply_text=error;break 'tool_rounds;}
                        }
                        last_llm_model_used = Some(resp.model_used.clone());
                        if code_studio_disk_task && selected_model_route.is_none() {
                            if let Some(provider)=resp.provider_used.as_ref() {
                                let route=akasha_llm::config::RouteEntry{provider:provider.clone(),model:resp.model_used.clone(),config:None};
                                if recovered_model_route.as_ref().is_none_or(|old|old.provider!=route.provider || old.model!=route.model) {
                                    run_outcome.checks.push(serde_json::json!({"kind":"provider_route_recovered","provider":route.provider,"model":route.model,"status":"passed","detail":"Le fournisseur a retourné une génération complète; cette route configurée est préférée pour les tours suivants."}));
                                }
                                recovered_model_route=Some(route);
                            }
                        }
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
                        let error_text = e.to_string();
                        let provider_parse_error = is_provider_tool_call_parse_error(&error_text);
                        let safe_error = sanitized_tool_transport_error(&error_text, provider_parse_error);
                        tracing::warn!(error = %safe_error, "LLM completion failed");
                        insert_task_tracking_event(
                            store_path.as_path(),
                            task_id,
                            "llm_call_finished",
                            serde_json::json!({
                                "round": round + 1,
                                "success": false,
                                "error": safe_error.clone(),
                            }),
                        );
                        let local_protocol_error = error_text.contains("tool_protocol_error:");
                        let protocol_retry_count = run_outcome.checks.iter()
                            .filter(|check| check["kind"] == "tool_protocol_retry")
                            .count();
                        let provider_parse_retry_count = run_outcome.checks.iter()
                            .filter(|check| check["kind"] == "tool_protocol_retry"
                                && check["failure_kind"] == "provider_json_parse")
                            .count();
                        let retry_allowed = if provider_parse_error {
                            provider_parse_retry_count < 1
                        } else {
                            local_protocol_error && protocol_retry_count < 3
                        };
                        if code_studio_disk_task && retry_allowed {
                            let failure_kind = if provider_parse_error { "provider_json_parse" } else { "native_tool_protocol" };
                            let retry_detail = if provider_parse_error {
                                "Le fournisseur a rejeté le JSON d'appel d'outil avant son exécution."
                            } else {
                                "Le protocole natif a rejeté l'appel avant son exécution."
                            };
                            run_outcome.checks.push(serde_json::json!({
                                "kind":"tool_protocol_retry",
                                "failure_kind":failure_kind,
                                "status":"retried",
                                "detail":retry_detail
                            }));
                            if let Some(goal)=goal_tracker.as_mut() {
                                // Failed transport has no usage receipt; conservatively
                                // charge the configured completion allowance as estimated.
                                goal.state.tokens_used=goal.state.tokens_used.saturating_add(completion_max_tokens as u64);
                                if let Err(error)=goal.save(&store_path,task_id,"active","Malformed tool transport rejected; bounded corrective retry") {run_outcome.termination=TerminationReason::ContextUnavailable;reply_text=error;break 'tool_rounds;}
                            }
                            current_prompt.push_str(&format!("\n[Current advertised functions] {}. Choose only from these names.\n",turn_tool_names.as_deref().unwrap_or(&[]).join(", ")));
                            current_prompt.push_str(&format!("\n[Transport rejection — no tool call from this response was executed] {retry_detail} Regenerate one complete operation using an advertised function and its literal keyword arguments. Do not mix formats or use legacy TOOL lines. Keep the arguments small and valid JSON. Re-read current ticket and file state before making any assumptions about earlier operations. Continue the requested implementation only after the repaired call succeeds.\n"));
                            continue 'tool_rounds;
                        }
                        run_outcome.termination = if error_text.to_ascii_lowercase().contains("timeout") {TerminationReason::Timeout}else{TerminationReason::ProviderError};
                        reply_text = if provider_parse_error {
                            "Le fournisseur LLM a rejeté le JSON d’un appel d’outil. La tâche s’est arrêtée et cet appel n’a pas été exécuté. Des opérations précédentes peuvent avoir abouti : vérifiez l’état du projet avant de relancer ou choisissez un autre modèle.".to_string()
                        } else if error_text.starts_with("provider_generation_failed:") {
                            error_text.chars().take(2000).collect()
                        } else {
                            format!("Sorry, I couldn't get a response (error: {}).", safe_error)
                        };
                        break;
                    }
                    Ok(Err(join_err)) => {
                        tracing::warn!(error = %join_err, "Stream task join failed");
                        run_outcome.termination = TerminationReason::ProviderError;
                        reply_text = if accumulated.is_empty() {
                            format!("LLM task error: {}", join_err)
                        } else {
                            accumulated
                        };
                        break;
                    }
                    Err(_timeout) => {
                        stream_join.abort();
                        run_outcome.termination = TerminationReason::Timeout;
                        tracing::warn!(
                            timeout_secs = llm_timeout_secs,
                            "Overall LLM timeout on stream completion"
                        );
                        reply_text = if code_studio_disk_task || accumulated.is_empty() {
                            timeout_message()
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

            // One observable retry is allowed for an explicit file creation before
            // any effect, with the physical/tool guards still enforced. Persist its
            // marker in checks so a restart cannot reset this retry allowance.
            if response.trim().is_empty() {
                if code_studio_disk_task && scope_unchanged && super::studio_completion::can_retry_empty(&explicit_file_goals,run_outcome.successful_tool_calls,run_outcome.failed_tool_calls,&run_outcome.checks) {
                    run_outcome.checks.push(serde_json::json!({"kind":"empty_generation_retry","status":"retried","attempt":1,"detail":"Empty response before any tool effect; a real operation is still required. A second empty generation remains model_empty."}));
                    current_prompt=format!("{delivered_prompt}\n[Empty generation retry — no tool operation ran]\nYour previous generation was empty. Execute the explicitly requested file operations with real tools, in their requested order and exact contents. A final claim cannot replace an operation. After the confirmed operations, finish normally.");
                    continue;
                }
                run_outcome.termination = TerminationReason::ModelEmpty;
                reply_text = String::new();
                break 'tool_rounds;
            }

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
                let calls = if code_studio_disk_task {super::studio_conversation::parse_calls_for_turn(&response,turn_tool_names.as_deref())} else {parse_tool_calls(&response)};
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
            // Targeted missing-file recovery follows verified progress; a zero-effect claim still fails as a tool error.
            if code_studio_disk_task && no_parseable_tools_this_round && !explicit_file_goals.is_empty() && run_outcome.has_file_effect_progress() {
                let current=if let Some(executor)=tools_executor.as_ref(){Some(crate::studio_policy::executor(executor.read().await.clone(), studio_policy_root))}else{tools_executor_snapshot.clone()};
                if let Some(executor)=current {
                    let checks={
                        let _guard=match crate::studio_tool_contract::workspace_guard(&tool_disk_workspace_root).await {Ok(guard)=>guard,Err(error)=>{run_outcome.termination=TerminationReason::ContextUnavailable;reply_text=error;break 'tool_rounds;}};
                        super::studio_completion::check_deliverables(&tool_disk_workspace_root,&executor.policy,&explicit_file_goals,if scope_unchanged {&explicit_file_literals}else{&[]})
                    };
                    let pending=checks.iter().filter(|c|c["status"]=="failed").filter_map(|c|c["path"].as_str()).map(str::to_owned).collect::<Vec<_>>();
                    run_outcome.checks.retain(|c|c["kind"]!="requested_file");run_outcome.checks.extend(checks.clone());
                    insert_task_tracking_event(&store_path,task_id,"studio_explicit_files_checked",serde_json::json!({"checks":checks}));
                    if !pending.is_empty() {
                        if physical_completion_retries>=3 {run_outcome.termination=TerminationReason::VerificationFailed;reply_text=format!("Explicitly requested files are still missing: {}",pending.join(", "));break 'tool_rounds;}
                        physical_completion_retries+=1;
                        if !delivered_prompt.is_empty(){conversation.push(if !conversation.is_empty() && (delivered_prompt.trim_start().starts_with("[Active goal") || delivered_prompt.trim_start().starts_with("[Repair the FIRST") || delivered_prompt.trim_start().starts_with("[STUDIO_READ_ONLY_STREAK]")) {akasha_llm::ConversationMessage::runtime_control(delivered_prompt.clone())} else {akasha_llm::ConversationMessage::user(delivered_prompt.clone())});}
                        conversation.push(akasha_llm::ConversationMessage::assistant(response.clone()));
                        current_prompt=format!("[Pending physical deliverables] The final response is premature: these explicitly requested files are missing or do not match their required literal content: {}. Complete the ORIGINAL user request with the available tools and current permissions. Do not repeat confirmed operations on other files. Presence alone does not validate file contents or other requested checks.\nOriginal user request:\n{}",pending.join(", "),super::studio_conversation::user_request(&task_scope));
                        continue;
                    }
                }
            }
            if code_studio_disk_task && no_parseable_tools_this_round
                && !run_outcome.confirmed_file_mutation()
                && run_outcome.requires_file_effect()
                && tools_executor_snapshot.as_ref().is_some_and(|e|policy_allows_primary_disk_write(&e.policy))
            {
                if studio_zero_tool_promise_nags >= 3 {
                    run_outcome.termination=TerminationReason::ToolError;
                    reply_text="No file mutation was confirmed for the explicit file request; reads and failed edits do not validate the requested change.".into();
                    break 'tool_rounds;
                }
                studio_zero_tool_promise_nags += 1;
                current_prompt.push_str("\n[Execution still missing] No successful file mutation has been confirmed. Reads and failed edits do not implement the requested change. Execute the original requested file operation now with native studio_call or the structured JSON tool branch (schema_version, tool, arguments including the exact path and content). Read existing files first; the runtime uses this task's latest successful read when expected_version is omitted and rejects stale edits. Use expected_version: missing for file creation. Do not return a final response until the actual tool result confirms the requested effects.\n");
                continue;
            }
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
                let mut goal_boundary_attempt = false;
                let mut round_non_survey_progress = false;
                let mut tool_result_starts = Vec::new();
                // Validate explicit byte constraints before any dependent batch effect.
                // Rejected calls and deferred siblings remain visible, without fake results.
                let literal_errors:Vec<Option<String>>=calls.iter().map(|(name,args)| {
                    let decoded=if name=="studio_call" {match crate::studio_tool_contract::decode(&args.join(" ")) {Ok(call)=>Some(call),Err(error)=>return Some(serde_json::json!({"schema_version":1,"success":false,"error_code":"invalid_tool_call","error":error}).to_string())}}else{None};
                    if !scope_unchanged {return None;}
                    let tool=decoded.as_ref().map(|call|call.envelope.tool.as_str()).unwrap_or(name);
                    let argv=decoded.as_ref().map(|call|call.argv.as_slice()).unwrap_or(args);
                    super::studio_completion::literal_write_error(&tool_disk_workspace_root,&explicit_file_literals,tool,argv)
                }).collect();
                let literal_batch_rejected=literal_errors.iter().any(Option::is_some);
                // Retain the execution order without duplicating large file bodies
                // in every rejected sibling result. Literal contents remain in the
                // original user request; this is evidence of non-execution only.
                let rejected_calls:Vec<_>=calls.iter().take(16).map(|(name,args)| {
                    let value=if name=="studio_call" {serde_json::from_str::<serde_json::Value>(&args.join(" ")).ok()}else{None};
                    let tool=value.as_ref().and_then(|v|v["tool"].as_str()).unwrap_or(name);
                    let path=value.as_ref().and_then(|v|v["arguments"]["path"].as_str()).or_else(||args.first().filter(|_|name!="studio_call").map(String::as_str)).unwrap_or("");
                    serde_json::json!({"tool":&tool[..tool.floor_char_boundary(64.min(tool.len()))],"path":&path[..path.floor_char_boundary(256.min(path.len()))],"status":"not_executed"})
                }).collect();
                let rejected_batch = serde_json::json!({"calls":rejected_calls,"omitted_calls":calls.len().saturating_sub(16),"retry_hint":"No call in this batch was executed. Resubmit from the FIRST unexecuted operation, not just the last call. Read existing files before mutation; expected_version may be omitted when this task has a successful read, in which case the runtime binds to that snapshot and rejects stale edits. Use expected_version: missing for creation. A deferred creation did NOT create its file."});
                let mut previous_batch_call_succeeded = true;
                let mut repository_rules_deferred = false;
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
                        tool_result_starts.push(tool_results.len());
                        if literal_batch_rejected {
                            let index=tool_result_starts.len()-1;
                            let error=literal_errors[index].clone().unwrap_or_else(||serde_json::json!({"schema_version":1,"success":false,"error_code":"batch_deferred","error":"No operation executed: this batch contains an invalid tool call or a file content that violates the explicit user literal. Correct it and request the required operations again in order."}).to_string());
                            let error=serde_json::json!({"schema_version":1,"success":false,"error":serde_json::from_str::<serde_json::Value>(&error).unwrap_or_else(|_|serde_json::json!(error)),"rejected_batch":rejected_batch}).to_string();
                            run_outcome.failed_tool_calls+=1;tool_results.push(error);continue;
                        }
                        if code_studio_disk_task && !previous_batch_call_succeeded {
                            run_outcome.failed_tool_calls += 1;
                            tool_results.push(serde_json::json!({"schema_version":1,"success":false,"error_code":"batch_deferred","error":"An earlier operation in this batch failed or requires replanning. Repair it and request the remaining operations again in order."}).to_string());
                            continue;
                        }
                        // Every early return/continue leaves the batch blocked. Only a
                        // confirmed successful invocation permits its next sibling.
                        previous_batch_call_succeeded = false;
                        let typed_call=if name=="studio_call" {
                            match crate::studio_tool_contract::decode(&args.join(" ")) {
                                Ok(call)=>Some(call),Err(error)=>{run_outcome.failed_tool_calls+=1;tool_results.push(serde_json::json!({"schema_version":1,"success":false,"error":error}).to_string());continue;}
                            }
                        }else{None};
                        let native_envelope_args=args;
                        let name=typed_call.as_ref().map(|call|&call.envelope.tool).unwrap_or(name);
                        let args=typed_call.as_ref().map(|call|&call.argv).unwrap_or(args);
                        if name=="list_tools" && !run_outcome.checks.iter().any(|check|check["kind"]=="native_catalogue_discovery") {
                            run_outcome.checks.push(serde_json::json!({"kind":"native_catalogue_discovery","status":"expanded","detail":"Subsequent turns advertise the full currently permitted native catalogue; permissions are still checked at invocation."}));
                        }


                        // Hot-reloaded permissions must constrain the next invocation of an active task.
                        let current_executor = if let Some(registry) = tools_executor.as_ref() {
                            crate::studio_policy::executor(registry.read().await.clone(), studio_policy_root)
                        } else { exec.clone() };
                        let exec = &current_executor;
                        if matches!(store.get(task_id).ok().flatten().map(|t| t.status),
                            Some(TaskStatus::Paused | TaskStatus::Cancelled | TaskStatus::Interrupted | TaskStatus::WaitingUserInput | TaskStatus::Failed)) {
                            break 'tool_rounds;
                        }
                        // Phase D: resolve skill name to tool_ref (spec 33)
                        let invocation_role = store.get(task_id).ok().flatten().map(|t| t.assigned_agent);
                        // Builtin tool names are authoritative; a registry entry cannot shadow them.
                        let skill_definition = if crate::api::AVAILABLE_TOOLS.iter().any(|(tool,_)| *tool == name) { None } else {
                            match &skill_registry { Some(reg) => reg.get(name).await, None => None }
                        };
                        if skill_definition.as_ref().is_some_and(|skill| !skill.agents.is_empty() && !skill.agents.iter().any(|agent| agent.eq_ignore_ascii_case(invocation_role.as_deref().unwrap_or(&assigned_agent)))) {
                            run_outcome.failed_tool_calls += 1;
                            tool_results.push(format!("[skill] {name} is not available for {assigned_agent}"));
                            continue;
                        }
                        let actual_tool = skill_definition.as_ref().map(|s| s.tool_ref.clone()).unwrap_or_else(|| name.clone());
                        let actual_tool = canonicalize_tool_name(&actual_tool);
                        let tool_args: &[String] = args.as_slice();
                        let effective_role_tool = if actual_tool.is_empty() {
                            if tool_args.is_empty() { "read_skill" } else { "run_command" }
                        } else { actual_tool.as_str() };
                        let role_check = invocation_role.as_deref().ok_or_else(|| "[studio] cannot resolve task role".to_string())
                            .and_then(|role| crate::studio_role_policy::check(role, effective_role_tool, tool_args, Some(&tool_disk_workspace_root)));
                        if let Err(reason) = role_check {
                            run_outcome.failed_tool_calls += 1;
                            let _ = bus.send(EventEnvelope::new(EventType::ToolInvoked, Some(serde_json::json!({
                                "tool":effective_role_tool, "success":false, "reason":"role_policy_denied", "result_preview":reason
                            }))).with_correlation(timeline_correlation));
                            tool_results.push(reason);
                            continue;
                        }
                        if code_studio_disk_task {
                            if let Err(reason) = crate::studio_capabilities::check(&exec.policy, invocation_role.as_deref().unwrap_or(&assigned_agent), effective_role_tool) {
                                run_outcome.failed_tool_calls += 1;
                                let _ = bus.send(EventEnvelope::new(EventType::ToolInvoked, Some(serde_json::json!({"tool":effective_role_tool,"success":false,"reason":"capability_unavailable","result_preview":reason}))).with_correlation(timeline_correlation));
                                tool_results.push(reason);
                                continue;
                            }
                        }
                        if code_studio_disk_task {
                            let targets: Vec<String> = if actual_tool=="lsp_apply_rename" {
                                let targets=tool_args.first().and_then(|id|Uuid::parse_str(id).ok()).ok_or_else(||"LSP plan UUID required".to_string()).and_then(|id|crate::studio_lsp::plan_paths(data_dir_for_studio_flags,&tool_disk_workspace_root,task_id,id));
                                match targets {Ok(paths)=>paths,Err(error)=>{run_outcome.failed_tool_calls+=1;tool_results.push(error);continue;}}
                            } else if actual_tool=="git_commit" {
                                serde_json::from_str::<crate::studio_delivery::Commit>(&tool_args.join(" ")).map(|request|request.paths).unwrap_or_default()
                            } else if matches!(actual_tool.as_str(), "write_file" | "write_code") {
                                crate::api::parse_write_file_request(tool_args).map(|(path,_)| path).into_iter().collect()
                            } else if actual_tool == "read_file" {
                                let (parts, _, _) = parse_read_file_args(tool_args);
                                vec![parts.join(" ")]
                            } else if matches!(actual_tool.as_str(), "rename_path" | "move_tree") {
                                let mut paths: Vec<String> = tool_args.first().cloned().into_iter().collect();
                                if tool_args.len() > 1 { paths.push(tool_args[1..].join(" ")); }
                                paths
                            } else if matches!(actual_tool.as_str(), "file_diff" | "diff_unified" | "dir_compare") {
                                tool_args.iter().take(2).cloned().collect()
                            } else if matches!(actual_tool.as_str(), "search_files" | "grep_content" | "edit_file" | "apply_patch" | "search_replace" | "delete_file") {
                                tool_args.first().cloned().into_iter().collect()
                            } else { Vec::new() };
                            for path in targets {
                                match crate::studio_project_context::rules_for_path(&tool_disk_workspace_root, &path, &exec.policy) {
                                    Ok(rules) => for rule in rules {
                                        let key = format!("{}:{}", rule.path.display(), rule.sha256);
                                        if !active_repository_rules.contains(&key) {
                                            match crate::studio_project_context::record(data_dir_for_studio_flags, task_id, "rules", rule.metadata()) {
                                                Ok(()) => {
                                                    active_repository_rules.insert(key);
                                                    tool_results.push(rule.prompt());
                                                    repository_rules_deferred = true;
                                                }
                                                Err(reason) => {
                                                    run_outcome.termination = TerminationReason::ContextUnavailable;
                                                    reply_text = reason;
                                                    break 'tool_rounds;
                                                }
                                            }
                                        }
                                    },
                                    Err(reason) => {
                                        run_outcome.termination = TerminationReason::ContextUnavailable;
                                        reply_text = reason;
                                        break 'tool_rounds;
                                    }
                                }
                            }
                            if repository_rules_deferred && !matches!(effective_role_tool, "read_file" | "grep_content" | "search_files" | "read_skill" | "list_skills" | "read_todos") {
                                tool_results.push(format!("[{actual_tool}] deferred: read and apply the repository instructions supplied in this round before retrying the action"));
                                continue;
                            }
                        }
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
                                        "[read_file] The default read of {path_str} was partial. This is not an access denial: retry the native read_file function with full=true, or use offset_line and limit_lines for successive windows. Do not repeat the default read or ask the user for another path."
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
                        let goal_read_key = if code_studio_disk_task
                            && goal_tracker.is_some()
                            && actual_tool.eq_ignore_ascii_case("read_file")
                        {
                            let (path_tokens, _, _) = parse_read_file_args(&tool_args);
                            let path = normalize_tool_path_hint(&path_tokens.join(" "));
                            (!path.trim().is_empty())
                                .then(|| goal_read_request_key(&path, &tool_args))
                        } else {
                            None
                        };
                        if let Some(key) = goal_read_key.as_ref() {
                            let successful_reads = successful_goal_reads_by_path
                                .get(key)
                                .copied()
                                .unwrap_or_default();
                            if should_suppress_repeated_goal_read(
                                &actual_tool,
                                studio_goal_failure_seen,
                                successful_reads,
                            ) {
                                let suppressed = format!(
                                    "[read_file] Suppressed this exact repeated request for {key}: it already succeeded twice during a failed goal. A different line window or a full read is still allowed. Use the recorded content and diagnostic to edit, or request the specific missing window. No file access failed and no change is implied."
                                );
                                let payload = serde_json::json!({
                                    "tool": "read_file",
                                    "args": tool_args,
                                    "result_preview": suppressed,
                                    "success": false,
                                    "reason": "repeated_goal_read_suppressed"
                                });
                                let _ = bus.send(
                                    EventEnvelope::new(EventType::ToolInvoked, Some(payload))
                                        .with_correlation(timeline_correlation),
                                );
                                tool_results.push(suppressed);
                                continue;
                            }
                        }
                        let args_str = tool_args.join(" ");
                        let history = tool_loop_history_by_agent
                            .entry(loop_agent_key.clone())
                            .or_default();
                        history.push((actual_tool.clone(), args_str.clone()));
                        if code_studio_disk_task {
                            let retry_key = (actual_tool.to_ascii_lowercase(), args_str.clone());
                            if let Some((retry_count, previous_error)) =
                                failed_mutation_retries_by_agent.get_mut(&retry_key)
                            {
                                if *retry_count >= 3 {
                                    run_outcome.termination = TerminationReason::ToolLoop;
                                    reply_text = format!(
                                        "Repeated unchanged or stale edit blocked after three retries: {}. Choose a different edit with current file content.",
                                        actual_tool
                                    );
                                    break 'tool_rounds;
                                }
                                *retry_count = retry_count.saturating_add(1);
                                history.pop();
                                let error = previous_error.chars().take(320).collect::<String>();
                                tool_results.push(format!(
                                    "[{actual_tool}] This exact edit was not executed again because the previous attempt failed: {error}. Do not repeat these arguments. Use the current file contents to choose a different edit, then rerun the failed check. Suppressed retry {retry_count}/3."
                                ));
                                continue;
                            }
                        }
                        // Phase 4: loop detection — same tool+args repeated 3 times
                        // for the same agent key (agent = sub-agent task_id). A repeated
                        // read is recoverable once: the model may have ignored a successful
                        // file result, so feed it a direct instruction to use that result
                        // before treating the loop as terminal.
                        let repeated_three_times = if history.len() >= 3 {
                            let last = history.last().unwrap();
                            history
                                .iter()
                                .rev()
                                .take(3)
                                .all(|entry| entry.0 == last.0 && entry.1 == last.1)
                        } else {
                            false
                        };
                        if repeated_three_times {
                            let recovery_key = (loop_agent_key.clone(), args_str.clone());
                            let recovery_count = duplicate_read_recovery_by_agent
                                .get(&recovery_key)
                                .copied()
                                .unwrap_or_default();
                            let recovery_limit = if code_studio_disk_task && goal_tracker.is_some() {
                                4
                            } else {
                                1
                            };
                            if should_recover_duplicate_read(
                                &actual_tool,
                                repeated_three_times,
                                recovery_count,
                                recovery_limit,
                            ) {
                                duplicate_read_recovery_by_agent
                                    .insert(recovery_key, recovery_count.saturating_add(1));
                                history.pop();
                                if code_studio_disk_task {
                                    studio_read_only_streak_rounds = 0;
                                    studio_repair_focus_next_turn =
                                        goal_tracker.is_some() && studio_goal_failure_seen;
                                }
                                let recovery = format!(
                                    "[read_file] Suppressed repeated identical read #{}/{}. The latest successful contents and tool results are already in this task. Do not call read_file again with these arguments. Use the recorded build/acceptance diagnostic and make a focused source edit with search_replace, edit_file, or apply_patch; then rerun only the failed check. If more context is truly needed, use a different narrow line window. No file change is implied by this message.",
                                    recovery_count + 1,
                                    recovery_limit
                                );
                                tool_results.push(recovery);
                                continue;
                            } else {
                                run_outcome.termination = TerminationReason::ToolLoop;
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
                        // Documentary CLI skills inherit command approval and scope, including their executable.
                        let mut approval_args: Vec<String> = if actual_tool.is_empty() && !tool_args.is_empty() {
                            std::iter::once(name.clone()).chain(tool_args.iter().cloned()).collect()
                        } else {tool_args.to_vec()};
                        let delivery_approval_identity = if matches!(effective_role_tool,"git_commit"|"git_push"|"github_create_pr") {
                            match crate::studio_delivery::approval_identity(&tool_disk_workspace_root,effective_role_tool,&approval_args).await {
                                Ok(identity)=>{approval_args.push(identity.clone());Some(identity)},
                                Err(error)=>{tool_results.push(format!("Delivery approval unavailable: {error}"));continue;}
                            }
                        }else{None};
                        if effective_role_tool=="terminal_session" && exec.policy.requires_approval(effective_role_tool) && serde_json::to_vec(&approval_args).map_or(true,|bytes|bytes.len()>4096) {
                            tool_results.push("Terminal approval payload exceeds the 4 KiB review limit; split the request into smaller inspectable operations.".into());continue;
                        }
                        if exec.policy.requires_approval(effective_role_tool) {
                            let permission_mode = load_permission_mode(data_dir);
                            if permission_mode.mode == "allow_all" {
                                // Global session mode bypasses interactive approval prompts.
                            } else {
                            let scope_key = tool_scope_key(effective_role_tool, &approval_args);
                            let state = crate::permissions_center::load(data_dir);
                            let mut already_granted = false;
                            if let Some(decision) =
                                crate::permissions_center::lookup(effective_role_tool, &scope_key, &state)
                            {
                                match decision.mode {
                                    crate::permissions_center::DecisionMode::AllowPersistent => {
                                        already_granted = true;
                                        let payload = serde_json::json!({
                                            "tool": effective_role_tool,
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
                                            "tool": effective_role_tool,
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
                                    let args_preview: String = if matches!(effective_role_tool,"git_commit"|"git_push"|"github_create_pr") {
                                        crate::studio_delivery::approval_preview(delivery_approval_identity.as_deref().unwrap_or_default())
                                    } else if effective_role_tool=="terminal_session" {
                                        serde_json::to_string(&approval_args).unwrap_or_default()
                                    } else if matches!(
                                        effective_role_tool,
                                        "apply_patch" | "edit_file" | "write_file" | "write_code" | "delete_file" | "terminal_session" | "git_commit" | "github_create_pr"
                                    ) {
                                        "[redacted]".to_string()
                                    } else {
                                        let truncated: Vec<String> = approval_args
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
                                        let suffix = if approval_args.len() > 3 {
                                            format!(" … ({} args)", approval_args.len())
                                        } else {
                                            String::new()
                                        };
                                        truncated.join(" ") + &suffix
                                    };
                                    let question = format!(
                                        "Approuver l'action : {} — {} ?",
                                        effective_role_tool, args_preview
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
                                        tool: effective_role_tool.to_string(),
                                        scope: scope_key.clone(),
                                        action: effective_role_tool.to_string(),
                                        description: format!("{} {}", effective_role_tool, args_preview),
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
                                            approval_request_id, task_id, effective_role_tool, err
                                        );
                                    }
                                    if code_studio_disk_task {if let Err(error)={crate::studio_recovery::record_waiting(&store_path,task_id,"waiting_human",serde_json::json!({"tool":actual_tool,"question":question,"choices":choices}),(&current_prompt,round,max_tool_rounds,&run_outcome))}{run_outcome.termination=TerminationReason::ContextUnavailable;reply_text=error;break 'tool_rounds;}}
                                    let (tx, rx) = tokio::sync::oneshot::channel();
                                    let pending = PendingHumanInput {
                                        request_id:Some(approval_request_id.clone()),
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
                                        "tool": effective_role_tool,
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
                                                "tool": effective_role_tool,
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
                                    if matches!(TaskStore::open(&store_path).ok().and_then(|s|s.get(task_id).ok().flatten()).map(|t|t.status),Some(TaskStatus::Paused|TaskStatus::Cancelled|TaskStatus::Interrupted|TaskStatus::Failed)) {break 'tool_rounds;}
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
                                            tool: effective_role_tool.to_string(),
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
                                            "tool": effective_role_tool,
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
                        let args_preview_tc: String = if let Some(identity)=delivery_approval_identity.as_deref() { crate::studio_delivery::approval_preview(identity) } else {
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
                        if let Some(definition) = skill_definition.as_ref().filter(|_| name != &actual_tool && !(actual_tool.is_empty() && tool_args.is_empty())) {
                            let metadata = serde_json::json!({"name":definition.name,"source_root":definition.source_root,"sha256":definition.sha256,"version":definition.version,"activation":"tool_alias"});
                            if let Err(reason) = crate::studio_project_context::record(data_dir_for_studio_flags, task_id, "skills", metadata.clone()) {
                                run_outcome.termination = TerminationReason::ContextUnavailable;
                                reply_text = reason;
                                break 'tool_rounds;
                            }
                            let _ = store.insert_event(task_id, "skill_activated", Some(&metadata), &chrono::Utc::now().to_rfc3339());
                        }
                        let workflow_operation=if code_studio_disk_task && matches!(actual_tool.as_str(),"delegate_to_agent"|"install_skill"|"uninstall_skill"|"write_todos"|"merge_todos"|"update_todo") {
                            match crate::studio_tool_contract::WorkflowOperation::begin(&store_path,task_id,&actual_tool,tool_args){Ok(operation)=>Some(operation),Err(error)=>{run_outcome.failed_tool_calls+=1;tool_results.push(error);continue;}}
                        }else{None};
                        if let Some(expected)=delivery_approval_identity.as_ref() {
                            if crate::studio_delivery::approval_identity(&tool_disk_workspace_root,effective_role_tool,tool_args).await.as_ref().ok()!=Some(expected) {
                                tool_results.push("Delivery destination changed after approval; request fresh approval.".into());continue;
                            }
                        }
                        let approved_delivery_envelope=delivery_approval_identity.as_ref().map(|identity| {
                            vec![serde_json::json!({"schema_version":1,"operation_id":typed_call.as_ref().map(|call|call.envelope.operation_id).unwrap_or_else(Uuid::new_v4),"tool":actual_tool,"arguments":serde_json::from_str::<serde_json::Value>(&tool_args.join(" ")).unwrap_or_default(),"approval_identity":identity}).to_string()]
                        });
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
                                            if code_studio_disk_task {if let Err(error)={explicit_file_goals.clear();scope_unchanged=false;crate::studio_recovery::record_waiting(&store_path,task_id,"waiting_human",serde_json::json!({"tool":"ask_user","question":question,"choices":choices}),(&current_prompt,round,max_tool_rounds,&run_outcome))}{run_outcome.termination=TerminationReason::ContextUnavailable;reply_text=error;break 'tool_rounds;}}
                                            let (tx, rx) = tokio::sync::oneshot::channel();
                                            let request_id=Uuid::new_v4().to_string();
                                            let pending = PendingHumanInput {
                                                request_id:Some(request_id.clone()),
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
                                                "request_id": request_id.clone(),
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
                                                Ok(Ok(reply)) => {
                                                    if let Some(exec)=tools_executor.as_ref(){let policy=crate::studio_policy::for_workspace(&exec.read().await.policy, &tool_disk_workspace_root);if let Err(error)=crate::studio_file_effects::human_reply(&store_path,task_id,&tool_disk_workspace_root,super::studio_conversation::user_request(&task_scope),&question,&reply,&assigned_agent,&policy,&mut run_outcome){run_outcome.termination=TerminationReason::ContextUnavailable;reply_text=error;break 'tool_rounds;}}
                                                    (true,format!("[ask_user] User replied: {}",reply),None)
                                                },
                                                Ok(Err(_)) => {
                                                    let mut g = store.write().await;
                                                    if g.get(&task_id).is_some_and(|p|p.request_id.as_deref()==Some(request_id.as_str())){g.remove(&task_id);}
                                                    if matches!(TaskStore::open(&store_path).ok().and_then(|s|s.get(task_id).ok().flatten()).map(|t|t.status),Some(TaskStatus::Paused|TaskStatus::Cancelled|TaskStatus::Interrupted|TaskStatus::Failed)) {break 'tool_rounds;}
                                                    (
                                                        false,
                                                        "[ask_user] Channel closed.".to_string(),
                                                        None,
                                                    )
                                                }
                                                Err(_) => {
                                                    let mut g = store.write().await;
                                                    if g.get(&task_id).is_some_and(|p|p.request_id.as_deref()==Some(request_id.as_str())){g.remove(&task_id);}
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
                                    let initial_backlog_preflight = if code_studio_disk_task
                                        && assigned_agent
                                            .eq_ignore_ascii_case("studio_project_manager")
                                    {
                                        let task_message = task_snapshot
                                            .as_ref()
                                            .and_then(|task| task.initial_message.as_deref())
                                            .unwrap_or("");
                                        crate::api_studio::studio_initial_backlog_preflight(
                                            &tool_disk_workspace_root,
                                            &task_id.to_string(),
                                            task_message,
                                        )
                                    } else {
                                        Ok(())
                                    };
                                    if let Err(error) = initial_backlog_preflight {
                                        (
                                            false,
                                            format!("[delegate_to_agent] {error}"),
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
                                                    Ok(Ok(Err(e))) if e.starts_with("child_task_terminal:") => (
                                                        // The child is confirmed terminal, so this is a
                                                        // known delegation result rather than an unknown
                                                        // side effect. Let the project manager inspect the
                                                        // child and continue recovery in this task.
                                                        true,
                                                        format!("[delegate_to_agent] Le sous-agent a terminé sans valider son travail ({e}). Le résultat de cette délégation est connu. Inspecte la tâche et le Kanban, conserve les modifications déjà confirmées puis poursuis avec une reprise ciblée; ne demande pas un chemin de dépôt déjà fourni par Code Studio."),
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
                                        let list = reg.for_agent(invocation_role.as_deref().unwrap_or(&assigned_agent)).await;
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
                                let skill_name = tool_args.first().map(String::as_str).unwrap_or("");
                                let resource = tool_args.get(1).map(String::as_str);
                                match &skill_registry {
                                    Some(registry) => match crate::studio_project_context::activate_skill(registry, skill_name, invocation_role.as_deref().unwrap_or(&assigned_agent), resource, &store_path, task_id).await {
                                        Ok(body) => (true, body, None),
                                        Err(reason) => (false, reason, None),
                                    },
                                    None => (false, "[read_skill] registry unavailable".into(), None),
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
                                    match &skill_registry {
                                        Some(registry) => match crate::studio_project_context::activate_skill(registry, name, invocation_role.as_deref().unwrap_or(&assigned_agent), None, &store_path, task_id).await {
                                            Ok(body) => (true, body, None),
                                            Err(reason) => (false, reason, None),
                                        },
                                        None => (false, "[read_skill] registry unavailable".into(), None),
                                    }
                                }
                            } else {
                                execute_tool_call(
                                    exec,
                                    if typed_call.is_some() || approved_delivery_envelope.is_some(){"studio_call"}else{&actual_tool},
                                    approved_delivery_envelope.as_deref().unwrap_or_else(||if typed_call.is_some(){native_envelope_args.as_slice()}else{tool_args}),
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
                        if let Some(operation)=workflow_operation {if let Err(error)=operation.finish(success,&res){run_outcome.termination=TerminationReason::ContextUnavailable;reply_text=format!("Workflow result unknown: {error}");break 'tool_rounds;}}
                        if success && matches!(actual_tool.as_str(),"grep_content"|"search_files") {
                            let context=crate::studio_project_context::read(data_dir_for_studio_flags,task_id);
                            if let Some(rules)=context["rules"].as_array() {for rule in rules {
                                if let (Some(path),Some(hash))=(rule["path"].as_str(),rule["sha256"].as_str()) {active_repository_rules.insert(format!("{path}:{hash}"));}
                            }}
                        }
                        previous_batch_call_succeeded = success;
                        if success {
                            run_outcome.successful_tool_calls += 1;
                            if let Some(key) = goal_read_key.as_ref() {
                                let reads = successful_goal_reads_by_path.entry(key.clone()).or_default();
                                *reads = reads.saturating_add(1);
                            } else if matches!(
                                actual_tool.as_str(),
                                "search_replace" | "edit_file" | "apply_patch" | "write_file" | "write_code"
                            ) {
                                successful_goal_reads_by_path.clear();
                            }
                        } else { run_outcome.failed_tool_calls += 1; }
                        if serde_json::from_str::<serde_json::Value>(&res).ok().is_some_and(|value|value["result_unknown"]==true){run_outcome.termination=TerminationReason::ContextUnavailable;reply_text=format!("Operation result unknown; inspect effects and use explicit recovery before continuing. {res}");break 'tool_rounds;}

                        if success {if let Err(error)=crate::studio_file_effects::after(&store_path,task_id,&mut run_outcome){run_outcome.termination=TerminationReason::ContextUnavailable;reply_text=error;break 'tool_rounds;}}
                        if matches!(effective_role_tool, "run_command" | "run_terminal" | "run_in_container") {
                            run_outcome.checks.push(serde_json::json!({"kind":"command", "tool":effective_role_tool, "status":if success {"passed"} else {"failed"}, "exit_code":serde_json::from_str::<serde_json::Value>(&res).ok().and_then(|value|value.get("exit_code").cloned()).unwrap_or(serde_json::Value::Null), "source":"tool_executor"}));
                        }
                        if let Some(img) = captured_image {
                            // Studio browser PNGs are persisted and displayed by the
                            // task evidence viewer. Keep vision input separate from
                            // prose: embedding the data URI makes long binary strings
                            // leak into Markdown clients that do not render it.
                            if !code_studio_disk_task || actual_tool != "browser" {
                                last_captured_image_base64 = Some(img.clone());
                            }
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
                            "apply_patch" | "edit_file" | "write_file" | "write_code" | "delete_file" | "terminal_session" | "git_commit" | "github_create_pr"
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
                        let mut payload = serde_json::json!({
                            "tool": tool_display,
                            "skill": if &actual_tool != name { Some(name.as_str()) } else { None::<&str> },
                            "args": redacted_args,
                            "result_preview": if res.len() > 300 { format!("{}...", &res[..res.floor_char_boundary(300)]) } else { res.clone() },
                            "success": success,
                            "explanation": serde_json::Value::Null
                        });
                        if !success && matches!(actual_tool.as_str(), "run_command" | "run_terminal") {
                            let result = serde_json::from_str::<serde_json::Value>(&res).ok();
                            let output = result
                                .as_ref()
                                .and_then(|value| value.get("output"))
                                .and_then(serde_json::Value::as_str)
                                .or_else(|| result.as_ref().and_then(|value| value.get("error")).and_then(serde_json::Value::as_str))
                                .unwrap_or(&res);
                            const MAX_COMMAND_DIAGNOSTIC_CHARS: usize = 1600;
                            let output_chars = output.chars().count();
                            let diagnostic: String = output
                                .chars()
                                .rev()
                                .take(MAX_COMMAND_DIAGNOSTIC_CHARS)
                                .collect::<Vec<_>>()
                                .into_iter()
                                .rev()
                                .collect();
                            payload["diagnostic_preview"] = serde_json::json!(if output_chars > MAX_COMMAND_DIAGNOSTIC_CHARS {
                                format!("… (début omis)\n{diagnostic}")
                            } else {
                                diagnostic
                            });
                            if let Some(exit_code) = result.as_ref().and_then(|value| value.get("exit_code")) {
                                payload["exit_code"] = exit_code.clone();
                            }
                        }
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
                        goal_boundary_attempt |= crate::studio_execution::goal_verification_after_tool(&actual_tool, success);
                        round_non_survey_progress |= success && !crate::api_studio::studio_survey_tool(&actual_tool);
                        if actual_tool=="run_command" && !success {studio_goal_failure_seen=true;}
                        if !success && (res.contains("stale_file_version") || res.contains("edit_no_change")) {
                            studio_repair_focus_next_turn=false;
                            studio_read_only_streak_rounds=0;
                            if code_studio_disk_task && is_retryable_failed_mutation(&actual_tool, &res) {
                                failed_mutation_retries_by_agent
                                    .entry((actual_tool.to_ascii_lowercase(), args_str.clone()))
                                    .and_modify(|(count, previous_error)| {
                                        *count = count.saturating_add(1);
                                        *previous_error = res.clone();
                                    })
                                    .or_insert((1, res.clone()));
                            }
                        } else if success && code_studio_disk_task && crate::studio_tool_contract::is_file_mutation(&actual_tool) {
                            if let Some(path) = tool_args.first() {
                                let prefix = format!("{path} ");
                                failed_mutation_retries_by_agent.retain(|(_, args), _| {
                                    args != path && !args.starts_with(&prefix)
                                });
                            }
                        }
                        tool_results.push(res);
                    }
                }
                let results_blob = tool_results.join("\n");
                if code_studio_disk_task {
                    let per_call:Vec<String>=tool_result_starts.iter().enumerate().map(|(index,start)| {
                        let end=tool_result_starts.get(index+1).copied().unwrap_or(tool_results.len());
                        let recorded=tool_results[*start..end].join("\n");
                        if recorded.is_empty(){"No execution result recorded; do not assume success.".into()}else{recorded}
                    }).collect();
                    if !delivered_prompt.trim().is_empty() {conversation.push(if !conversation.is_empty() && (delivered_prompt.trim_start().starts_with("[Active goal") || delivered_prompt.trim_start().starts_with("[Repair the FIRST") || delivered_prompt.trim_start().starts_with("[STUDIO_READ_ONLY_STREAK]")) {akasha_llm::ConversationMessage::runtime_control(delivered_prompt.clone())} else {akasha_llm::ConversationMessage::user(delivered_prompt.clone())});}
                    conversation.extend(super::studio_conversation::tool_turn_with_results(&response, &results_blob, &per_call, round, if scope_unchanged && goal_tracker.is_none() {&task_scope}else{""}));
                    // Current policy/context is reloaded on resume, then kept once in the transcript.
                    resume_overlay.clear();
                }
                let mut studio_readonly_nudge: Option<String> = None;
                if code_studio_disk_task && (goal_tracker.is_some() || run_outcome.requires_file_effect()) && !calls.is_empty() {
                    let only_survey = !round_non_survey_progress;
                    if only_survey {
                        studio_read_only_streak_rounds =
                            studio_read_only_streak_rounds.saturating_add(1);
                    } else {
                        studio_read_only_streak_rounds = 0;
                        studio_repair_focus_next_turn = false;
                    }
                    if goal_tracker.is_some() {
                        insert_task_tracking_event(&store_path,task_id,"studio_goal_progress_guard",serde_json::json!({"round":round,"no_progress":only_survey,"streak":studio_read_only_streak_rounds,"non_survey_progress":round_non_survey_progress,"failure_seen":studio_goal_failure_seen,"repair_focus":studio_repair_focus_next_turn,"call_names":calls.iter().map(|(name,_)|name).collect::<Vec<_>>()}));
                    }
                    let max_streak = std::env::var("AKASHA_STUDIO_READ_ONLY_STREAK_MAX")
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(4)
                        .max(2)
                        .min(20);
                    if studio_read_only_streak_rounds >= max_streak {
                        studio_read_only_streak_rounds = 0;
                        studio_repair_focus_next_turn = goal_tracker.is_some() && studio_goal_failure_seen;
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
                            "[STUDIO_READ_ONLY_STREAK]\nThe last {} model rounds made no successful non-survey progress (read-only inspection or failed commands). The user request requires repository changes. Use the currently advertised native functions when available. Repair the already inspected faulty file or address a documented environment blocker with write_code, write_file, search_replace, edit_file or apply_patch. Never invent an access denial. If truly blocked, give the precise recorded blocking reason. Do not repeat another unchanged exploratory-only round.",
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
                current_prompt = if code_studio_disk_task {
                    String::new()
                } else if round_had_ask_user {
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
                if code_studio_disk_task {
                    if let Ok(Some(mut checkpoint))=crate::studio_recovery::load(&store_path,task_id,false){checkpoint.round=round;checkpoint.prompt=current_prompt.clone();checkpoint.conversation=conversation.clone();checkpoint.phase="after_tools".into();checkpoint.waiting=None;checkpoint.outcome=run_outcome.clone();if let Err(error)=crate::studio_recovery::save(&store_path,&checkpoint){run_outcome.termination=TerminationReason::ContextUnavailable;reply_text=error;break 'tool_rounds;}}
                }
                if let Some(goal)=goal_tracker.as_mut().filter(|goal| goal_boundary_attempt && goal.ready_for_tool_boundary(&tool_disk_workspace_root)) {
                    if !matches!(store.get(task_id).ok().flatten().map(|task|task.status),Some(TaskStatus::Running)) {break 'tool_rounds;}
                    match goal.verify_boundary(&store_path,task_id,&tool_disk_workspace_root,&mut run_outcome).await {
                        Ok(crate::studio_execution::GoalBoundary::Verified)=>{reply_text=format!("Les critères mécaniques configurés de l’objectif sont vérifiés : {}. Cette validation porte uniquement sur les propriétés définies dans les critères.",goal.goal.criteria.criteria.iter().map(|criterion|criterion.text.as_str()).collect::<Vec<_>>().join("; "));break 'tool_rounds;},
                        Ok(crate::studio_execution::GoalBoundary::Budget)=>{run_outcome.termination=TerminationReason::BudgetExhausted;reply_text=format!("Objectif non atteint dans les limites configurées : {}",goal.state.detail);break 'tool_rounds;},
                        Ok(crate::studio_execution::GoalBoundary::Pending(errors))=>{studio_goal_failure_seen=true;current_prompt.push_str(&format!("\n[Repair the FIRST failing goal dependency] {}\nThese are fresh mechanical diagnostics from a command that has already executed. Read the implicated files and repair all related errors coherently. Do not rerun an unchanged failing check or reinstall already present dependencies. Use a further command only when additional diagnostics or a documented environment repair is needed; repeated inspection alone cannot satisfy this criterion. Correcting a faulty file with changed content is required and is not replaying an identical write. Keep the original functionality and protected acceptance tests unchanged. Retry this dependency before working on later criteria.\n",errors.join("; ")));},
                        Err(error)=>{run_outcome.termination=TerminationReason::ContextUnavailable;reply_text=error;break 'tool_rounds;},
                    }
                }
                // This is actual persisted goal state, not an unconditional
                // synthetic "continue" after a completed native operation.
                // It also keeps tools advertised by templates that attach their
                // catalogue only to the most recent user instruction.
                if current_prompt.is_empty() {
                    if let Some(goal)=goal_tracker.as_ref() {
                        current_prompt=goal.continuation_hint_for_workspace(&tool_disk_workspace_root);
                    }
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

                    // Code Studio tasks continue autonomously in bounded
                    // increments. Only the global hard cap can request a user
                    // decision, so ordinary scaffolding doesn't stop at a
                    // small per-run tool budget.
                    if code_studio_disk_task {
                        match auto_extend_studio_round_budget(
                            &store_path,
                            task_id,
                            round,
                            &mut max_tool_rounds,
                            &current_prompt,
                            &run_outcome,
                        ) {
                            Ok(true) => continue 'tool_rounds,
                            Ok(false) => {}
                            Err(error) => {
                                run_outcome.termination = TerminationReason::ContextUnavailable;
                                reply_text = error;
                                break 'tool_rounds;
                            }
                        }
                    }

                    // When human_input_store is available, ask user whether to continue (+10 rounds), reset and continue, or stop.
                    if code_studio_disk_task {
                        if let Err(error)={crate::studio_recovery::record_waiting(&store_path,task_id,"waiting_tool_round_limit",serde_json::json!({"tool_round_limit":true,"question":"Continue tool rounds?","current_round":round,"max_tool_rounds":max_tool_rounds}),(&current_prompt,round,max_tool_rounds,&run_outcome))} { run_outcome.termination=TerminationReason::ContextUnavailable;reply_text=error;break 'tool_rounds; }
                    }
                    let should_stop = match studio_round_limit_choice(&human_input_store,&bus,task_id,round,max_tool_rounds).await.as_deref() {
                        Some("Continuer (+10 tours)") => { max_tool_rounds=max_tool_rounds.saturating_add(10); false }
                        Some("Réinitialiser et continuer (+10 tours)") => { round=0; max_tool_rounds=max_tool_rounds.saturating_add(10); false }
                        _ => true,
                    };
                    if should_stop {
                        run_outcome.termination = TerminationReason::ToolRoundLimit;
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
            if let Some(goal)=goal_tracker.as_mut() {
                if !matches!(store.get(task_id).ok().flatten().map(|task|task.status),Some(TaskStatus::Running)) {break 'tool_rounds;}
                match goal.verify_boundary(&store_path,task_id,&tool_disk_workspace_root,&mut run_outcome).await {
                    Ok(crate::studio_execution::GoalBoundary::Verified)=>{},
                    Ok(crate::studio_execution::GoalBoundary::Budget)=>{run_outcome.termination=TerminationReason::BudgetExhausted;reply_text=format!("Objectif non atteint dans les limites configurées : {}",goal.state.detail);break 'tool_rounds;},
                    Ok(crate::studio_execution::GoalBoundary::Pending(errors))=>{
                        let _=bus.send(EventEnvelope::new(EventType::ProgressUpdate,Some(serde_json::json!({"task_id":task_id,"message":format!("Objectif : cycle {}/{}; critères encore en échec : {}",goal.state.cycles,goal.goal.max_cycles,errors.join("; ")),"progress_pct":60}))).with_correlation(task_id));
                        conversation.push(akasha_llm::ConversationMessage::assistant(response.clone()));
                        current_prompt=format!("[Goal not verified — continue within current permissions]\nObjective: {}\nMechanical verification failures: {}\nInspect the current files and recorded tool results. Choose the next useful action; do not replay successful mutations.\nOriginal request:\n{}",goal.goal.objective,errors.join("; "),super::studio_conversation::user_request(&task_scope));
                        continue;
                    },
                    Err(error)=>{run_outcome.termination=TerminationReason::ContextUnavailable;reply_text=error;break 'tool_rounds;},
                }
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
                    explicit_file_goals.clear();scope_unchanged=false;
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

    let mut reply_text = if reply_text.trim().is_empty() {
        if !run_outcome.failed() { run_outcome.termination = TerminationReason::ModelEmpty; }
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
    let halted_user = matches!(
        task_status_snapshot,
        Some(
            TaskStatus::Paused
                | TaskStatus::Cancelled
                | TaskStatus::Interrupted
                | TaskStatus::Failed
                | TaskStatus::WaitingUserInput
        )
    );
    let mut studio_verify_error: Option<String> = None;
    let mut studio_autofix_applied = false;
    // Planning/review must never execute project scripts or the mutating autofix path.
    if !halted_user && code_studio_disk_task && !restricted_studio_role && !run_outcome.failed() {
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
                Ok(executed) => {
                    run_outcome.checks.push(serde_json::json!({"kind":"project_build", "status":if executed {"passed"} else {"not_run"}, "exit_code":if executed {Some(0)} else {None::<i32>}}));
                    studio_verify_error = None;
                    break;
                }
                Err(e) => {
                    run_outcome.checks.push(serde_json::json!({"kind":"project_build", "status":"failed", "detail":e.chars().take(2000).collect::<String>()}));
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
                let mut mech = Vec::new();
                for criterion in &pay.criteria {
                    let errors = crate::api_studio::run_mechanical_acceptance_checks(
                        tool_disk_workspace_root.as_path(),
                        &crate::api_studio::StudioAcceptancePayload { criteria: vec![criterion.clone()] },
                        tmo,
                    ).await;
                    let status = if matches!(criterion.kind, crate::api_studio::StudioCriterionKind::Manual) { "manual" }
                        else if errors.is_empty() { "passed" } else { "failed" };
                    run_outcome.checks.push(serde_json::json!({
                        "kind":"acceptance", "id":criterion.id, "criterion_kind":criterion.kind,
                        "status":status, "text":criterion.text,
                        "exit_code":if status == "passed" && matches!(criterion.kind, crate::api_studio::StudioCriterionKind::CommandOk) {Some(0)} else {None::<i32>}
                    }));
                    mech.extend(errors);
                }
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
    if !first_meaningful_progress_sent && !reply_text.trim().is_empty()
        && run_outcome.termination == TerminationReason::Success {
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
                    require_tool: false,
                    conversation: None,
                    tools: None,
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
    if is_error_outcome || run_outcome.failed() {
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
    crate::studio_observation::close_task(task_id).await;
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

    if code_studio_disk_task && !run_outcome.failed(){if let Some(exec)=tools_executor.as_ref(){let policy=crate::studio_policy::for_workspace(&exec.read().await.policy, &tool_disk_workspace_root);if let Err(error)=crate::studio_file_effects::refresh(&store_path,task_id,&tool_disk_workspace_root,super::studio_conversation::user_request(&task_scope),&assigned_agent,&policy,&mut run_outcome){run_outcome.termination=TerminationReason::ContextUnavailable;reply_text=error;}}}
    if code_studio_disk_task && !run_outcome.failed() && run_outcome.requires_file_effect() && !run_outcome.confirmed_file_mutation() {
        run_outcome.termination=TerminationReason::ToolError;reply_text="No confirmed file mutation; reads and prose cannot attest the requested edit.".into();
    }
    if code_studio_disk_task && !run_outcome.failed() && run_outcome.failed_tool_calls > 0 && run_outcome.successful_tool_calls == 0 {
        run_outcome.termination = TerminationReason::ToolError;
        reply_text = "No tool operation succeeded; the requested effects are not validated.\n".to_string() + &reply_text;
    }
    if code_studio_disk_task && !explicit_file_goals.is_empty() {
        if let Some(registry)=tools_executor.as_ref() {
            let executor=crate::studio_policy::executor(registry.read().await.clone(), studio_policy_root);
            match crate::studio_tool_contract::workspace_guard(&tool_disk_workspace_root).await {
                Ok(_guard)=>{
                    let checks=super::studio_completion::check_deliverables(&tool_disk_workspace_root,&executor.policy,&explicit_file_goals,if scope_unchanged {&explicit_file_literals}else{&[]});
                    if checks.iter().any(|c|c["status"]=="failed") && !run_outcome.failed() {run_outcome.termination=TerminationReason::VerificationFailed;reply_text="Explicitly requested files are missing after task verification.".into();}
                    run_outcome.checks.retain(|c|c["kind"]!="requested_file");run_outcome.checks.extend(checks);
                },
                Err(error)=>{if !run_outcome.failed(){run_outcome.termination=TerminationReason::ContextUnavailable;reply_text=error;}},
            }
        }
    }
    if studio_verify_error.is_some() { run_outcome.termination = TerminationReason::VerificationFailed; }
    if goal_tracker.as_ref().is_some_and(|goal|goal.state.state!="completed") && !run_outcome.failed() {run_outcome.termination=TerminationReason::VerificationFailed;reply_text="Objectif non vérifié : l’exécution est arrêtée ou bloquée.".into();}
    let proposed = if run_outcome.failed() { TaskStatus::Failed } else { TaskStatus::Completed };
    let reason = if run_outcome.failed() {
        Some(studio_verify_display_message.as_deref().unwrap_or(&reply_text).chars().take(2000).collect::<String>())
    } else { None };
    let (final_status, outcome) = match store.finalize_outcome(task_id, proposed, run_outcome.json(reason)) {
        Ok(result) => result,
        Err(error) => {
            tracing::error!(%task_id, %error, "Cannot persist task outcome; completion not announced");
            cancel_task_watchdogs(&mut watchdog_cancel, &mut stall_cancel);
            notify_task_completion(&task_completion_registry, task_id).await;
            return;
        }
    };
    if let Some(goal)=goal_tracker.as_mut() {
        let state=match final_status {TaskStatus::Completed if goal.state.state=="completed"=>"completed",TaskStatus::Paused=>"paused",TaskStatus::Interrupted=>"interrupted",TaskStatus::Cancelled=>"cancelled",TaskStatus::WaitingUserInput=>"waiting_user_input",_ if goal.state.state=="budget_exhausted"=>"budget_exhausted",_=>"blocked"};
        let detail=if state=="completed" {"Critères mécaniques vérifiés".to_string()}else{reply_text.chars().take(2000).collect()};
        let _=goal.save(&store_path,task_id,state,&detail);
    }
    let task_status_snapshot = Some(final_status.clone());
    let is_paused = matches!(final_status, TaskStatus::Paused | TaskStatus::WaitingUserInput);
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
    if code_studio_disk_task {if let Ok(Some(mut checkpoint))=crate::studio_recovery::load(&store_path,task_id,false){checkpoint.phase=final_status.as_str().into();checkpoint.outcome=run_outcome.clone();let _=crate::studio_recovery::save(&store_path,&checkpoint);}}
    let final_status_str = final_status.as_str();
    if code_studio_disk_task && matches!(final_status,TaskStatus::Failed|TaskStatus::Cancelled|TaskStatus::Interrupted) {
        if let Some(project)=store.lineage_root_studio_project_id(task_id).ok().flatten() {
            let lock=crate::studio_kanban_dispatch::project_dispatch_lock(&project);
            let _reservation=lock.lock().await;
            if let Ok(root)=crate::studio::resolve_studio_project_dir(data_dir_for_studio_flags,&project) {
                crate::studio_kanban_dispatch::reconcile_project_tickets(&root,&store_path);
            }
        }
    }

    let mut final_payload = serde_json::json!({
        "task_id": task_id.to_string(),
        "status": final_status_str,
        "model_used": last_llm_model_used,
        "outcome": outcome
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
    if final_status_str == "failed" && final_payload.get("reason").is_none() {
        final_payload["reason"] = outcome["reason"].clone();
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
        Some(serde_json::json!({ "status": final_status_str, "outcome": outcome })),
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
            "outcome": outcome,
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
    if !matches!(final_status, TaskStatus::Paused | TaskStatus::Cancelled | TaskStatus::Interrupted | TaskStatus::WaitingUserInput) {
        if final_status == TaskStatus::Completed && !restricted_studio_role {
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
                    let ticket_root = match store.lineage_root_studio_project_id(task_id).ok().flatten() {
                        Some(project)=>crate::studio::resolve_studio_project_dir(data_dir_for_studio_flags,&project).ok(),
                        None=>Some(tool_disk_workspace_root.clone()),
                    };
                    if let Some(ticket_root)=ticket_root {let _ = crate::api_studio::studio_mark_ticket_ready_for_review(
                        &ticket_root,
                        &ticket_id,
                        &task_id.to_string(),
                        "system",
                    );}
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
            && final_status == TaskStatus::Completed
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
            let outcome_label = final_status.as_str();
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

#[cfg(test)]
mod write_file_reminder_tests {
    use super::write_file_reminder_for_policy;
    use crate::api::WRITE_FILE_REMINDER;

    #[test]
    fn reminder_is_omitted_when_write_file_is_disallowed() {
        assert_eq!(write_file_reminder_for_policy(true, false, false), "");
    }

    #[test]
    fn reminder_is_included_when_write_file_is_allowed_for_file_intent() {
        assert_eq!(
            write_file_reminder_for_policy(true, false, true),
            WRITE_FILE_REMINDER
        );
    }

    #[test]
    fn reminder_is_omitted_for_code_studio_disk_tasks_and_other_intents() {
        assert_eq!(write_file_reminder_for_policy(true, true, true), "");
        assert_eq!(write_file_reminder_for_policy(false, false, true), "");
    }
}
