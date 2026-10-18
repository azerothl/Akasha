//! Regression checks for the Studio audit delivery, with real temporary files/SQLite.
use crate::{studio_role_policy as roles, studio_tool_contract as contracts};
use serde_json::{json, Value};
use std::path::Path;
use uuid::Uuid;

fn policy(root: &Path) -> akasha_tools::ToolsPolicy {
    akasha_tools::ToolsPolicy {
        allowed_read_paths: vec![root.display().to_string()],
        allowed_write_paths: vec![root.display().to_string()],
        allowed_commands: vec!["git".into(), "cmd".into(), "node".into()],
        ..Default::default()
    }
}
fn call(tool: &str, args: Value, id: Uuid) -> contracts::Decoded {
    contracts::decode(&json!({"schema_version":1,"operation_id":id,"tool":tool,"arguments":args}).to_string()).unwrap()
}

#[test]
fn outcome_does_not_invent_acceptance() {
    let mut run = crate::task_outcome::RunOutcome::default();
    assert_eq!(run.json(None)["acceptance_verified"], false);
    run.checks = vec![json!({"kind":"acceptance","status":"not_run"})];
    assert_eq!(run.json(None)["acceptance_verified"], false);
    run.checks[0]["status"] = json!("passed");
    assert_eq!(run.json(None)["acceptance_verified"], true);
    run.termination = crate::task_outcome::TerminationReason::ModelEmpty;
    assert_eq!(run.json(None)["result"], "failed");
    assert_eq!(run.json(None)["acceptance_verified"], false);
    run.successful_tool_calls = 1;
    assert_eq!(run.json(None)["result"], "partial");
}

#[test]
fn every_abnormal_termination_is_failure() {
    use crate::task_outcome::{RunOutcome, TerminationReason::*};
    for termination in [ModelEmpty, ToolLoop, BudgetExhausted, ProviderError, Timeout, ToolRoundLimit, VerificationFailed, ContextUnavailable] {
        assert!(RunOutcome { termination, ..Default::default() }.failed());
    }
}

#[test]
fn mixed_typed_and_legacy_tools_filter_reasoning_and_keep_order() {
    let calls = crate::api_tool_parser::parse_tool_calls("<think>\nTOOL_JSON: {\"hidden\":true}\n</think>\nTOOL: write_file workspace:/a.txt\nhello\nTOOL_JSON: {\"visible\":true}\nTOOL: read_todos");
    assert_eq!(calls.iter().map(|c|c.0.as_str()).collect::<Vec<_>>(), vec!["write_file","studio_call","read_todos"]);
    assert!(!calls[0].1.join(" ").contains("TOOL_JSON"));
    assert_eq!(calls[1].1, vec!["{\"visible\":true}"]);
    assert!(contracts::decode("invalid incomplete legacy quotes").err().unwrap().contains("incomplete quotes"));
}

#[test]
fn queued_resume_clears_and_conditionally_restores_outcome() {
    use akasha_store::{Task, TaskStatus, TaskStore};
    let dir=tempfile::tempdir().unwrap(); let store=TaskStore::open(&dir.path().join("tasks.db")).unwrap();
    let id=Uuid::new_v4();let now=chrono::Utc::now();
    store.insert(&Task{id,parent_task_id:None,status:TaskStatus::Running,assigned_agent:"code".into(),created_at:now,updated_at:now,initial_message:None,studio_project_id:None}).unwrap();
    store.finalize_outcome(id,TaskStatus::Paused,json!({"status":"paused"})).unwrap();
    let saved=store.get_outcome(id).unwrap();
    assert!(store.transition_if(id,&[TaskStatus::Paused],TaskStatus::Queued).unwrap());
    assert!(store.get_outcome(id).unwrap().is_none());
    assert!(store.rollback_queued_resume(id,TaskStatus::Paused,saved.as_ref()).unwrap());
    assert_eq!(store.get_outcome(id).unwrap(),saved);
    store.update_status(id,TaskStatus::Queued).unwrap();
    assert!(store.get_outcome(id).unwrap().is_none());
    store.update_status(id,TaskStatus::Cancelled).unwrap();
    assert!(!store.rollback_queued_resume(id,TaskStatus::Paused,saved.as_ref()).unwrap());
}

#[test]
fn push_prerequisite_includes_github_cli_permission() {
    let dir=tempfile::tempdir().unwrap();let mut p=policy(dir.path());
    p.allowed_commands=vec!["git".into()];
    let row=crate::studio_capabilities::tool(&p,"studio_project_manager","git_push").unwrap();
    assert!(!row.runnable);
    assert!(row.notes.iter().any(|n|n.contains("github_cli_unavailable_or_denied")));
}

#[test]
fn finalization_preserves_concurrent_stops_in_sqlite() {
    use akasha_store::{Task, TaskStatus, TaskStore};
    for stopped in [TaskStatus::Cancelled, TaskStatus::Paused, TaskStatus::Failed, TaskStatus::Interrupted, TaskStatus::WaitingUserInput] {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("tasks.db");
        let store = TaskStore::open(&db).unwrap();
        let id = Uuid::new_v4();
        let now = chrono::Utc::now();
        store.insert(&Task {id,parent_task_id:None,status:TaskStatus::Running,assigned_agent:"studio_fullstack".into(),created_at:now,updated_at:now,initial_message:None,studio_project_id:None}).unwrap();
        let other = TaskStore::open(&db).unwrap();
        assert!(other.transition_if(id, &[TaskStatus::Running], stopped.clone()).unwrap());
        let (actual, value) = store.finalize_outcome(id, TaskStatus::Completed, json!({"result":"succeeded","acceptance_verified":true})).unwrap();
        assert_eq!(actual, stopped);
        assert_eq!(value["acceptance_verified"], false);
        assert_eq!(store.get_outcome(id).unwrap().unwrap(), value);
        assert!(!store.transition_if(id, &[TaskStatus::Running], TaskStatus::Completed).unwrap());
    }
}

#[test]
fn restricted_roles_deny_execution_and_external_effects() {
    for role in ["studio_planner", "studio_reviewer", "STUDIO_REVIEWER"] {
        for tool in ["run_command", "run_terminal", "run_command_background", "run_in_container", "terminal_session", "browser", "mcp_fixture_echo", "delegate_to_agent", "install_skill", "lsp_query", "lsp_apply_rename", "git_commit", "git_push", "github_create_pr", "write_code", "edit_file", "delete_file", "workspace_graph_search"] {
            assert!(!roles::allows_tool(role, tool), "{role} allowed {tool}");
        }
        assert!(roles::allows_tool(role, "read_file"));
    }
}

#[test]
fn planner_path_and_hardlink_are_safe() {
    let dir = tempfile::tempdir().unwrap();
    for path in ["src/main.ts", "../CODE_STUDIO_PLAN.md", "workspace:/../CODE_STUDIO_PLAN.md", "CODE_STUDIO_PLAN.md:stream", "workspace:/sub/CODE_STUDIO_PLAN.md"] {
        let args = vec![json!({"path":path,"content":"blocked"}).to_string()];
        assert!(roles::check("studio_planner", "write_file", &args, Some(dir.path())).is_err());
    }
    let outside = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(outside.path(), "original").unwrap();
    let plan = dir.path().join("CODE_STUDIO_PLAN.md");
    std::fs::hard_link(outside.path(), &plan).unwrap();
    let args = vec![json!({"path":"workspace:/CODE_STUDIO_PLAN.md","content":"safe plan"}).to_string()];
    roles::write_planner_plan(&args, Some(dir.path())).unwrap();
    assert_eq!(std::fs::read_to_string(outside.path()).unwrap(), "original");
    assert_eq!(std::fs::read_to_string(plan).unwrap(), "safe plan");
}

#[test]
fn reviewer_cannot_read_outside_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();
    assert!(roles::check("studio_reviewer", "read_file", &[outside.path().display().to_string()], Some(dir.path())).is_err());
    std::fs::write(dir.path().join("ok.txt"), "ok").unwrap();
    assert!(roles::check("studio_reviewer", "read_file", &["workspace:/ok.txt".into()], Some(dir.path())).is_ok());
}

#[test]
fn explicit_empty_profile_never_falls_back() {
    let mut p = akasha_tools::ToolsPolicy::default();
    p.default_profile = Some("empty".into());
    p.tool_profiles.insert("empty".into(), Vec::new());
    let tools = crate::studio_capabilities::catalogue(&p, "studio_fullstack");
    assert!(!tools.is_empty());
    assert!(tools.iter().all(|row| !row.allowed && !row.runnable && !row.announced));
}

#[test]
fn malformed_contracts_are_rejected_before_effects() {
    let id = Uuid::new_v4();
    for value in [
        json!({"schema_version":2,"operation_id":id,"tool":"read_file","arguments":{"path":"a"}}),
        json!({"schema_version":1,"operation_id":id,"tool":"write_file","arguments":{"path":"a","content":"b"}}),
        json!({"schema_version":1,"operation_id":id,"tool":"run_command","arguments":{"program":"node","args":[],"surprise":true}}),
        json!({"schema_version":1,"operation_id":id,"tool":"run_command","arguments":{"program":"node\ncmd"}}),
        json!({"schema_version":1,"operation_id":id,"tool":"read_file","arguments":{"path":"a","content":"b"}}),
    ] { assert!(contracts::decode(&value.to_string()).is_err()); }
    assert!(contracts::decode("{\"schema_version\":1").is_err());
    let decoded = call("write_file", json!({"path":"space name.txt","content":"é🙂\nquoted \"value\"","expected_version":"missing"}), id);
    assert_eq!(decoded.expected.as_deref(), Some("missing"));
    assert_eq!(serde_json::from_str::<Value>(&decoded.argv[0]).unwrap()["content"], "é🙂\nquoted \"value\"");
}

#[tokio::test]
async fn stale_files_and_unread_existing_files_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("test.db");
    let p = policy(dir.path());
    let task = Uuid::new_v4();
    let path = dir.path().join("a.txt");
    std::fs::write(&path, "before").unwrap();
    let write = call("write_file", json!({"path":"a.txt","content":"after","expected_version":"missing"}), Uuid::new_v4());
    assert!(contracts::Operation::begin(&db, task, "write_file", &write.argv, dir.path(), &p, Some(&write)).await.is_err());
    let read = call("read_file", json!({"path":"a.txt"}), Uuid::new_v4());
    let op = contracts::Operation::begin(&db, task, "read_file", &read.argv, dir.path(), &p, Some(&read)).await.unwrap();
    let result: Value = serde_json::from_str(&op.finish(true, "before").unwrap()).unwrap();
    let write = call("write_file", json!({"path":"a.txt","content":"after","expected_version":result["file_version"]}), Uuid::new_v4());
    std::fs::write(&path, "external editor").unwrap();
    assert!(contracts::Operation::begin(&db, task, "write_file", &write.argv, dir.path(), &p, Some(&write)).await.is_err());
    assert_eq!(std::fs::read_to_string(path).unwrap(), "external editor");
}

#[tokio::test]
async fn completed_operation_is_cached_and_changed_arguments_denied() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("test.db");
    let p = policy(dir.path());
    let task = Uuid::new_v4();
    let id = Uuid::new_v4();
    let decoded = call("run_command", json!({"program":"node","args":["-v"]}), id);
    let op = contracts::Operation::begin(&db, task, "run_command", &decoded.argv, dir.path(), &p, Some(&decoded)).await.unwrap();
    op.finish(true, r#"{"exit_code":0,"stdout":"v22"}"#).unwrap();
    let op = contracts::Operation::begin(&db, task, "run_command", &decoded.argv, dir.path(), &p, Some(&decoded)).await.unwrap();
    assert!(op.cached.is_some());
    drop(op);
    let changed = call("run_command", json!({"program":"node","args":["other.js"]}), id);
    assert!(contracts::Operation::begin(&db, task, "run_command", &changed.argv, dir.path(), &p, Some(&changed)).await.is_err());
}

#[tokio::test]
async fn interrupted_operation_blocks_mutations_but_allows_inspection() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("test.db");
    let p = policy(dir.path());
    let task = Uuid::new_v4();
    let decoded = call("run_command", json!({"program":"node","args":["effect.js"]}), Uuid::new_v4());
    drop(contracts::Operation::begin(&db, task, "run_command", &decoded.argv, dir.path(), &p, Some(&decoded)).await.unwrap());
    assert!(contracts::Operation::begin(&db, task, "run_command", &decoded.argv, dir.path(), &p, Some(&decoded)).await.is_err());
    assert_eq!(contracts::recovery_summary(&db, task).unwrap()["unknown_count"], 1);
    let read = call("git_status", json!({"argv":["workspace:/"]}), Uuid::new_v4());
    let op = contracts::Operation::begin(&db, task, "git_status", &read.argv, dir.path(), &p, Some(&read)).await.unwrap();
    op.finish(true, "inspection").unwrap();
}

#[test]
fn active_runs_cannot_overlap_and_release_on_drop() {
    let task = Uuid::new_v4();
    let guard = crate::studio_recovery::ActiveRun::enter(task).unwrap();
    assert!(crate::studio_recovery::is_active(task));
    assert!(crate::studio_recovery::ActiveRun::enter(task).is_err());
    drop(guard);
    assert!(!crate::studio_recovery::is_active(task));
    assert!(crate::studio_recovery::ActiveRun::enter(task).is_ok());
}

#[test]
fn checkpoint_round_session_wait_and_size_are_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("test.db");
    let task = Uuid::new_v4();
    let mut cp = crate::studio_recovery::Checkpoint {schema_version:1,task_id:task,project_id:Uuid::new_v4().to_string(),workspace:dir.path().into(),agent:"studio_fullstack".into(),session_id:"same-session".into(),preferred_task_type:None,incognito:true,round:7,prompt:"private context".into(),phase:"before_model".into(),waiting:None,outcome:Default::default()};
    crate::studio_recovery::save(&db, &cp).unwrap();
    crate::studio_recovery::record_waiting(&db, task, "approval", json!({"tool":"git_push"})).unwrap();
    let loaded = crate::studio_recovery::load(&db, task, false).unwrap().unwrap();
    assert_eq!(loaded.round, 7);
    assert_eq!(loaded.session_id, "same-session");
    assert_eq!(loaded.waiting.unwrap()["tool"], "git_push");
    cp.prompt = "x".repeat(2*1024*1024);
    assert!(crate::studio_recovery::save(&db, &cp).is_err());
}

#[test]
fn nested_rules_are_ordered_bounded_and_pinned() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("AGENTS.md"), "root rules").unwrap();
    std::fs::write(dir.path().join("src/AGENTS.md"), "nested rules").unwrap();
    let p = policy(dir.path());
    let rules = crate::studio_project_context::rules_for_path(dir.path(), "workspace:/src/new.ts", &p).unwrap();
    assert_eq!(rules.len(), 2);
    assert_eq!(rules[0].body, "root rules");
    assert_eq!(rules[1].body, "nested rules");
    assert_eq!(rules[1].sha256.len(), 64);
    std::fs::write(dir.path().join("src/AGENTS.md"), "x".repeat(16*1024+1)).unwrap();
    assert!(crate::studio_project_context::rules_for_path(dir.path(), "workspace:/src/new.ts", &p).is_err());
}

#[tokio::test]
async fn shipped_skills_load_and_project_resources_stay_jailed() {
    let data = tempfile::tempdir().unwrap();
    let spec = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec");
    let registry = crate::skills::SkillRegistry::new();
    registry.load_all(data.path(), &spec).await.unwrap();
    for name in ["studio-explore", "studio-implement", "studio-debug", "studio-verify", "studio-review", "studio-verify-ui", "studio-deliver"] {
        assert!(registry.get(name).await.is_some(), "missing {name}");
    }
    let root = data.path().join("project");
    let skill = root.join(".agents/skills/local");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(skill.join("SKILL.md"), "---\nname: local\ndescription: local test\nagents: [studio_planner]\n---\nLocal body").unwrap();
    std::fs::write(skill.join("resource.txt"), "safe resource").unwrap();
    let scoped = registry.for_project(&root).await.unwrap();
    assert!(scoped.activate("local", "studio_reviewer", None).await.is_none());
    assert!(scoped.activate("local", "studio_planner", Some("../secret")).await.is_none());
    assert_eq!(scoped.activate("local", "studio_planner", Some("resource.txt")).await.unwrap().0, "safe resource");
    std::fs::write(skill.join("SKILL.md"), "changed instructions").unwrap();
    assert!(scoped.activate("local", "studio_planner", None).await.is_none());
}

#[tokio::test]
async fn scoped_studio_write_reaches_disk_and_records_disk_version() {
    use akasha_store::{Task, TaskStatus, TaskStore};
    let data = tempfile::tempdir().unwrap();
    let project = Uuid::new_v4().to_string();
    let root = data.path().join("studio-projects").join(&project);
    std::fs::create_dir_all(&root).unwrap();
    let db = data.path().join("akasha.db");
    let task = Uuid::new_v4();
    let now = chrono::Utc::now();
    TaskStore::open(&db).unwrap().insert(&Task {id:task,parent_task_id:None,status:TaskStatus::Running,assigned_agent:"studio_fullstack".into(),created_at:now,updated_at:now,initial_message:None,studio_project_id:Some(project)}).unwrap();
    let executor = std::sync::Arc::new(akasha_tools::ToolExecutor::new(policy(data.path())));
    let workspace = std::sync::Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
    let ctx = crate::api_tool_dispatch::ToolCallContext {process_registry:None,long_term_client:None,task_id:task,store_path:Some(&db),conv_tx:None,message_webhook_url:None,plugin_registry:None,device_bridge:None,workspace_store:Some(&workspace),browser_registry:None,workspace_root:Some(&root),session_id:None};
    let decoded = call("write_file", json!({"path":"workspace:/proof.txt","content":"real disk","expected_version":"missing"}), Uuid::new_v4());
    let raw = serde_json::to_string(&json!({"schema_version":1,"operation_id":decoded.envelope.operation_id,"tool":"write_file","arguments":decoded.envelope.arguments})).unwrap();
    let (success, result, _) = crate::api_tool_dispatch::execute_tool_call(ctx, &executor, "studio_call", &[raw]).await;
    assert!(success, "{result}");
    assert_eq!(std::fs::read_to_string(root.join("proof.txt")).unwrap(), "real disk");
    assert_ne!(serde_json::from_str::<Value>(&result).unwrap()["file_version"], "missing");
}
