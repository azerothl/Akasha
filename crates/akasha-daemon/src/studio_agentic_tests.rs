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

fn running_task(db: &Path, id: Uuid) {
    let now=chrono::Utc::now();
    akasha_store::TaskStore::open(db).unwrap().insert(&akasha_store::Task{id,parent_task_id:None,status:akasha_store::TaskStatus::Running,assigned_agent:"studio_fullstack".into(),created_at:now,updated_at:now,initial_message:None,studio_project_id:Some("validation".into())}).unwrap();
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
fn native_schema_supports_task_scoped_file_versions_and_filters_tool_variants() {
    let definition=contracts::native_definition(vec!["write_file".into(),"read_file".into(),"git_status".into()]);
    let schema=&definition["function"]["parameters"];
    let compiled=jsonschema::JSONSchema::compile(schema).unwrap();
    let valid=json!({"schema_version":1,"tool":"write_file","arguments":{"path":"workspace:/a.md","content":"exact","expected_version":"missing"}});
    assert!(compiled.is_valid(&valid));
    let mut missing=valid.clone();missing["arguments"].as_object_mut().unwrap().remove("expected_version");
    assert!(compiled.is_valid(&missing));
    assert_eq!(contracts::decode(&missing.to_string()).unwrap().expected,None);
    let mut invalid=valid.clone();invalid["arguments"]["expected_version"]=json!("bad");
    assert!(!compiled.is_valid(&invalid));
    assert!(compiled.is_valid(&json!({"schema_version":1,"tool":"git_status","arguments":{"argv":["workspace:/"]}})));
    assert!(!compiled.is_valid(&json!({"schema_version":1,"tool":"run_command","arguments":{"program":"node","args":[]}})));
    let decoded=contracts::decode(&valid.to_string()).unwrap();
    let other=contracts::decode(&valid.to_string()).unwrap();
    assert_ne!(decoded.envelope.operation_id,other.envelope.operation_id);
}

#[test]
fn cancellation_after_resume_preparation_cannot_transition_back_to_queued() {
    let dir=tempfile::tempdir().unwrap();let db=dir.path().join("task.db");let id=Uuid::new_v4();
    running_task(&db,id);
    let store=akasha_store::TaskStore::open(&db).unwrap();
    store.update_status(id,akasha_store::TaskStatus::Interrupted).unwrap();
    let prepared=store.get(id).unwrap().unwrap();
    assert_eq!(prepared.status,akasha_store::TaskStatus::Interrupted);
    store.update_status(id,akasha_store::TaskStatus::Cancelled).unwrap();
    assert!(!store.transition_if(id,&[akasha_store::TaskStatus::Interrupted,akasha_store::TaskStatus::Paused],akasha_store::TaskStatus::Queued).unwrap());
    assert_eq!(store.get(id).unwrap().unwrap().status,akasha_store::TaskStatus::Cancelled);
}

#[test]
fn every_abnormal_termination_is_failure() {
    use crate::task_outcome::{RunOutcome, TerminationReason::*};
    for termination in [ModelEmpty, ToolLoop, ToolError, BudgetExhausted, ProviderError, Timeout, ToolRoundLimit, VerificationFailed, ContextUnavailable] {
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
fn an_earlier_failure_outcome_wins_over_another_failed_finalization() {
    use akasha_store::{TaskStatus,TaskStore};
    let dir=tempfile::tempdir().unwrap();let db=dir.path().join("tasks.db");let id=Uuid::new_v4();running_task(&db,id);let store=TaskStore::open(&db).unwrap();
    let (_,first)=store.finalize_outcome(id,TaskStatus::Failed,json!({"termination_reason":"watchdog","reason":"original timeout","result":"failed","acceptance_verified":false})).unwrap();
    let (_,second)=store.finalize_outcome(id,TaskStatus::Failed,json!({"termination_reason":"provider_error","reason":"late provider failure","result":"failed"})).unwrap();
    assert_eq!(first,second);
}

#[test]
fn push_prerequisite_includes_github_cli_permission() {
    let dir=tempfile::tempdir().unwrap();let mut p=policy(dir.path());
    p.allowed_commands=vec!["git".into()];
    let row=crate::studio_capabilities::tool(&p,"studio_project_manager","git_push").unwrap();
    assert!(!row.runnable);
    assert!(row.notes.iter().any(|n|n.contains("github_cli_unavailable_or_denied")));
}

#[tokio::test]
async fn cancellation_while_waiting_for_workspace_lock_prevents_operation() {
    use akasha_store::{Task, TaskStatus, TaskStore};
    let dir=tempfile::tempdir().unwrap();let db=dir.path().join("tasks.db");let store=TaskStore::open(&db).unwrap();
    let first=Uuid::new_v4();let second=Uuid::new_v4();let now=chrono::Utc::now();
    for id in [first,second] {store.insert(&Task{id,parent_task_id:None,status:TaskStatus::Running,assigned_agent:"studio_fullstack".into(),created_at:now,updated_at:now,initial_message:None,studio_project_id:Some("validation".into())}).unwrap();}
    std::fs::write(dir.path().join("read.txt"),"read only").unwrap();
    let p=policy(dir.path());let args=vec!["workspace:/read.txt".into()];
    let held=contracts::Operation::begin(&db,first,"read_file",&args,dir.path(),&p,None).await.unwrap();
    let copied_db=db.clone();let copied_root=dir.path().to_path_buf();
    let waiting=tokio::spawn(async move {contracts::Operation::begin(&copied_db,second,"read_file",&args,&copied_root,&p,None).await});
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished());
    store.update_status(second,TaskStatus::Cancelled).unwrap();drop(held);
    let error=waiting.await.unwrap().err().unwrap();assert!(error.contains("stopped"));
    assert_eq!(contracts::recovery_summary(&db,second).unwrap()["operations"].as_array().unwrap().len(),0);
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
        json!({"schema_version":1,"operation_id":id,"tool":"write_file","arguments":{"path":"a","content":"b","expected_version":"not-a-hash"}}),
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
    running_task(&db,task);
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
    running_task(&db,task);
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
    running_task(&db,task);
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
    let mut cp = crate::studio_recovery::Checkpoint {schema_version:1,task_id:task,project_id:Uuid::new_v4().to_string(),workspace:dir.path().into(),agent:"studio_fullstack".into(),session_id:"same-session".into(),preferred_task_type:None,incognito:true,round:7,max_tool_rounds:Some(10),prompt:"private context".into(),conversation:vec![],phase:"before_model".into(),waiting:None,outcome:Default::default()};
    crate::studio_recovery::save(&db, &cp).unwrap();
    cp.conversation.push(akasha_llm::ConversationMessage::user("original requirement".into()));
    cp.conversation.push(akasha_llm::ConversationMessage::assistant("TOOL: read_file workspace:/a.md".into()));
    crate::studio_recovery::save(&db, &cp).unwrap();
    crate::studio_recovery::record_waiting(&db, task, "approval", json!({"tool":"git_push"}),(&cp.prompt,8,20,&cp.outcome)).unwrap();
    let loaded = crate::studio_recovery::load(&db, task, false).unwrap().unwrap();
    assert_eq!(loaded.round, 8);
    assert_eq!(loaded.max_tool_rounds,Some(20));
    assert_eq!(loaded.session_id, "same-session");
    assert_eq!(loaded.conversation.len(),2);
    assert_eq!(loaded.conversation[0].content,"original requirement");
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
    if cfg!(feature="consumer-beta") {
        assert!(!success,"the consumer package cannot write arbitrary configuration files");
        assert!(!root.join("proof.txt").exists());
        assert_eq!(serde_json::from_str::<Value>(&result).unwrap()["file_version"],"missing");
        return;
    }
    assert!(success, "{result}");
    assert_eq!(std::fs::read_to_string(root.join("proof.txt")).unwrap(), "real disk");
    assert_ne!(serde_json::from_str::<Value>(&result).unwrap()["file_version"], "missing");
}

#[test]
fn terminal_status_updates_always_have_unverified_outcomes_and_matching_failure_keeps_details() {
    let dir=tempfile::tempdir().unwrap();let db=dir.path().join("tasks.db");
    for status in [akasha_store::TaskStatus::Failed,akasha_store::TaskStatus::Interrupted,akasha_store::TaskStatus::Cancelled] {
        let id=Uuid::new_v4();running_task(&db,id);let store=akasha_store::TaskStore::open(&db).unwrap();
        store.update_status(id,status.clone()).unwrap();let outcome=store.get_outcome(id).unwrap().unwrap();
        assert_eq!(outcome["status"],status.as_str());assert_eq!(outcome["acceptance_verified"],false);
        if status==akasha_store::TaskStatus::Failed {
            let (_,detail)=store.finalize_outcome(id,status,json!({"termination_reason":"model_empty","result":"partial","verification":[],"acceptance_verified":false})).unwrap();
            assert_eq!(detail["termination_reason"],"model_empty");assert_eq!(detail["result"],"partial");
        }
    }
}

#[tokio::test]
async fn cached_read_rechecks_revoked_paths_and_command_output_is_plain_and_truncation_explicit() {
    let dir=tempfile::tempdir().unwrap();let db=dir.path().join("task.db");let task=Uuid::new_v4();running_task(&db,task);let mut p=policy(dir.path());
    std::fs::write(dir.path().join("a.txt"),"text").unwrap();
    let read=contracts::decode(&json!({"schema_version":1,"operation_id":Uuid::new_v4(),"tool":"read_file","arguments":{"path":"workspace:/a.txt"}}).to_string()).unwrap();
    let op=contracts::Operation::begin(&db,task,"read_file",&read.argv,dir.path(),&p,Some(&read)).await.unwrap();op.finish(true,"text").unwrap();
    p.allowed_read_paths=vec!["Z:/not-allowed".into()];
    assert!(contracts::Operation::begin(&db,task,"read_file",&read.argv,dir.path(),&p,Some(&read)).await.is_err());
    let p=policy(dir.path());
    let cmd=contracts::decode(&json!({"schema_version":1,"tool":"run_command","arguments":{"program":"git","args":["status"]}}).to_string()).unwrap();
    let op=contracts::Operation::begin(&db,task,"run_command",&cmd.argv,dir.path(),&p,Some(&cmd)).await.unwrap();
    let result:Value=serde_json::from_str(&op.finish(true,&json!({"output":"hello","exit_code":0}).to_string()).unwrap()).unwrap();
    assert_eq!(result["output"],"hello");assert_eq!(result["exit_code"],0);
    let op=contracts::Operation::begin(&db,task,"read_file",&read.argv,dir.path(),&p,None).await.unwrap();
    let result:Value=serde_json::from_str(&op.finish(true,&"a".repeat(70000)).unwrap()).unwrap();assert_eq!(result["truncated"],true);
}

#[tokio::test]
async fn operator_delivery_accepts_stopped_tasks_while_agent_operations_stay_blocked() {
    let dir=tempfile::tempdir().unwrap();let db=dir.path().join("task.db");let id=Uuid::new_v4();running_task(&db,id);let p=policy(dir.path());
    akasha_store::TaskStore::open(&db).unwrap().update_status(id,akasha_store::TaskStatus::Failed).unwrap();
    let args=vec!["{}".into()];
    assert!(contracts::Operation::begin(&db,id,"git_commit",&args,dir.path(),&p,None).await.is_err());
    let op=contracts::Operation::begin_operator(&db,id,"git_commit",&args,dir.path(),&p,None).await.unwrap();op.finish(false,"test only").unwrap();
}

#[tokio::test]
async fn command_cwd_is_parsed_before_cached_policy_checks() {
    let dir=tempfile::tempdir().unwrap();let db=dir.path().join("task.db");let id=Uuid::new_v4();running_task(&db,id);let p=policy(dir.path());
    let cmd=contracts::decode(&json!({"schema_version":1,"tool":"run_command","arguments":{"program":"git","args":["status"],"cwd":"workspace:/"}}).to_string()).unwrap();
    let op=contracts::Operation::begin(&db,id,"run_command",&cmd.argv,dir.path(),&p,Some(&cmd)).await.unwrap();op.finish(true,"inspection test").unwrap();
}

#[tokio::test]
async fn publication_rechecks_approved_destination_after_acquiring_workspace() {
    let dir=tempfile::tempdir().unwrap();let db=dir.path().join("task.db");let id=Uuid::new_v4();running_task(&db,id);let mut p=policy(dir.path());p.allowed_commands.push("gh".into());
    let root=dir.path().join("studio-projects").join(Uuid::new_v4().to_string());std::fs::create_dir_all(&root).unwrap();
    let git=|args:&[&str]|assert!(std::process::Command::new("git").args(args).current_dir(&root).output().unwrap().status.success());
    git(&["init","-q"]);git(&["remote","add","origin","https://github.com/example/approved.git"]);
    let args=vec![json!({"remote":"origin","branch":"main","expected_head":"unborn","expected_remote_head":"missing"}).to_string()];
    let identity=crate::studio_delivery::approval_identity(&root,"git_push",&args).await.unwrap();
    let envelope=json!({"schema_version":1,"tool":"git_push","arguments":serde_json::from_str::<Value>(&args[0]).unwrap(),"approval_identity":identity});
    let decoded=contracts::decode(&envelope.to_string()).unwrap();
    let lock=contracts::workspace_guard(&root).await.unwrap();
    let future=contracts::Operation::begin(&db,id,"git_push",&decoded.argv,&root,&p,Some(&decoded));tokio::pin!(future);
    assert!(tokio::time::timeout(std::time::Duration::from_millis(20),future.as_mut()).await.is_err());
    git(&["remote","set-url","origin","https://github.com/example/retargeted.git"]);drop(lock);
    let error=future.await.err().unwrap();
    assert!(error.contains("destination changed"),"{error}");
    let database=rusqlite::Connection::open(&db).unwrap();
    let exists:i64=database.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name='studio_operations'",[],|r|r.get(0)).unwrap();
    if exists>0 {let count:i64=database.query_row("SELECT count(*) FROM studio_operations",[],|r|r.get(0)).unwrap();assert_eq!(count,0);}
    let mut current=envelope;current["approval_identity"]=json!(crate::studio_delivery::approval_identity(&root,"git_push",&args).await.unwrap());
    let approved=contracts::decode(&current.to_string()).unwrap();
    let operation=contracts::Operation::begin(&db,id,"git_push",&approved.argv,&root,&p,Some(&approved)).await.unwrap();operation.finish(false,"guard validation only; no push executed").unwrap();
}

#[tokio::test]
async fn git_only_profiles_can_start_local_delivery_inspection() {
    let dir=tempfile::tempdir().unwrap();let db=dir.path().join("task.db");let id=Uuid::new_v4();running_task(&db,id);let mut p=policy(dir.path());p.allowed_commands=vec!["git".into()];
    let decoded=contracts::decode(&json!({"schema_version":1,"tool":"git_delivery_status","arguments":{}}).to_string()).unwrap();
    let operation=contracts::Operation::begin(&db,id,"git_delivery_status",&decoded.argv,dir.path(),&p,Some(&decoded)).await.unwrap();operation.finish(true,"local inspection only").unwrap();
    let root=dir.path().join("studio-projects").join(Uuid::new_v4().to_string());std::fs::create_dir_all(&root).unwrap();
    for args in [vec!["init","-q"],vec!["remote","add","origin","https://github.com/example/local-status.git"]] {assert!(std::process::Command::new("git").args(args).current_dir(&root).output().unwrap().status.success());}
    let mut local=policy(&root);local.allowed_commands=vec!["git".into()];
    let status=crate::studio_delivery::preflight(dir.path(),&root,&local,"origin").await.unwrap();assert_eq!(status["github"]["accessible"],false);assert!(status["remote_head"].is_null());

}

#[tokio::test]
async fn repeated_legacy_write_reuses_confirmed_receipt_only_while_bytes_match() {
    let dir=tempfile::tempdir().unwrap();let db=dir.path().join("tasks.db");let id=Uuid::new_v4();running_task(&db,id);
    let p=policy(dir.path());let args=vec!["workspace:/legacy.txt".into(),"exact".into()];
    let operation=contracts::Operation::begin(&db,id,"write_file",&args,dir.path(),&p,None).await.unwrap();
    std::fs::write(dir.path().join("legacy.txt"),"exact").unwrap();
    let first:Value=serde_json::from_str(&operation.finish(true,"saved").unwrap()).unwrap();
    let replay=contracts::Operation::begin(&db,id,"write_file",&args,dir.path(),&p,None).await.unwrap();
    let cached:Value=serde_json::from_str(&replay.cached.as_ref().unwrap().1).unwrap();
    assert_eq!(cached["operation_id"],first["operation_id"]);assert_eq!(cached["replayed"],true);drop(replay);
    assert_eq!(contracts::recovery_summary(&db,id).unwrap()["operations"].as_array().unwrap().len(),1);
    std::fs::write(dir.path().join("legacy.txt"),"external edit").unwrap();
    assert!(contracts::Operation::begin(&db,id,"write_file",&args,dir.path(),&p,None).await.err().unwrap().contains("stale_file_version"));
    assert_eq!(std::fs::read_to_string(dir.path().join("legacy.txt")).unwrap(),"external edit");
}
#[tokio::test]
async fn cached_legacy_write_still_respects_policy_and_missing_file_requires_a_new_effect() {
    let dir=tempfile::tempdir().unwrap();let db=dir.path().join("tasks.db");let id=Uuid::new_v4();running_task(&db,id);
    let mut p=policy(dir.path());let args=vec!["workspace:/legacy.txt".into(),"exact".into()];
    let operation=contracts::Operation::begin(&db,id,"write_file",&args,dir.path(),&p,None).await.unwrap();
    std::fs::write(dir.path().join("legacy.txt"),"exact").unwrap();operation.finish(true,"saved").unwrap();
    p.allowed_write_paths.clear();
    assert!(contracts::Operation::begin(&db,id,"write_file",&args,dir.path(),&p,None).await.is_err());
    p=policy(dir.path());std::fs::remove_file(dir.path().join("legacy.txt")).unwrap();
    // A read of the absent file refreshes the expected version for an intentional recreation.
    let read=contracts::Operation::begin(&db,id,"read_file",&["workspace:/legacy.txt".into()],dir.path(),&p,None).await.unwrap();read.finish(true,"missing").unwrap();
    let recreated=contracts::Operation::begin(&db,id,"write_file",&args,dir.path(),&p,None).await.unwrap();
    assert!(recreated.cached.is_none());drop(recreated);
}

#[test]
fn queued_resume_cannot_be_admitted_as_stopped_operator_delivery() {
    use crate::studio_recovery::{with_admission,ActiveRun};
    let dir=tempfile::tempdir().unwrap();let db=dir.path().join("tasks.db");let id=Uuid::new_v4();running_task(&db,id);
    let store=akasha_store::TaskStore::open(&db).unwrap();store.update_status(id,akasha_store::TaskStatus::Paused).unwrap();
    let (started_tx,started_rx)=std::sync::mpsc::channel();
    let copied_db=db.clone();
    let worker=with_admission(id,||{
        let worker=std::thread::spawn(move||{started_tx.send(()).unwrap();ActiveRun::enter_stopped(&copied_db,id)});
        started_rx.recv().unwrap();
        assert!(store.transition_if(id,&[akasha_store::TaskStatus::Paused],akasha_store::TaskStatus::Queued).unwrap());
        worker
    }).unwrap();
    assert!(worker.join().unwrap().is_err());
    assert!(!crate::studio_recovery::is_active(id));
    let worker=ActiveRun::enter(id).unwrap();assert!(crate::studio_recovery::is_active(id));drop(worker);
}
#[test]
fn worker_admission_waits_until_resume_has_enqueued_and_released_admission() {
    use crate::studio_recovery::{with_admission,ActiveRun};
    let id=Uuid::new_v4();let (started_tx,started_rx)=std::sync::mpsc::channel();let (admitted_tx,admitted_rx)=std::sync::mpsc::channel();
    let worker=with_admission(id,||{
        let worker=std::thread::spawn(move||{started_tx.send(()).unwrap();let guard=ActiveRun::enter(id).unwrap();admitted_tx.send(guard).unwrap();});
        started_rx.recv().unwrap();
        assert!(admitted_rx.try_recv().is_err());
        worker
    }).unwrap();
    let guard=admitted_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();assert!(crate::studio_recovery::is_active(id));drop(guard);worker.join().unwrap();
}

#[tokio::test]
async fn studio_replacements_without_content_changes_are_failed_not_progress() {
    let dir=tempfile::tempdir().unwrap();let db=dir.path().join("test.db");let p=policy(dir.path());let task=Uuid::new_v4();running_task(&db,task);
    let path=dir.path().join("a.txt");std::fs::write(&path,"before").unwrap();
    let read=call("read_file",json!({"path":"a.txt"}),Uuid::new_v4());
    let op=contracts::Operation::begin(&db,task,"read_file",&read.argv,dir.path(),&p,Some(&read)).await.unwrap();
    let receipt:Value=serde_json::from_str(&op.finish(true,"before").unwrap()).unwrap();
    for (search,replacement) in [("absent","after"),("before","before")] {
        let edit=call("search_replace",json!({"path":"a.txt","search":search,"replacement":replacement,"expected_version":receipt["file_version"]}),Uuid::new_v4());
        let op=contracts::Operation::begin(&db,task,"search_replace",&edit.argv,dir.path(),&p,Some(&edit)).await.unwrap();
        let result:Value=serde_json::from_str(&op.finish(true,"replaced 0 occurrence(s)").unwrap()).unwrap();
        assert_eq!(result["success"],false);assert_eq!(result["error_code"],"edit_no_change");assert_eq!(result["file_version"],receipt["file_version"]);
        assert!(result["output"].as_str().unwrap().contains("No change was applied"));
        assert!(!result["output"].as_str().unwrap().contains("occurrence(s)"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(),"before");
    }
    let edit=call("search_replace",json!({"path":"a.txt","search":"before","replacement":"after","expected_version":receipt["file_version"]}),Uuid::new_v4());
    let op=contracts::Operation::begin(&db,task,"search_replace",&edit.argv,dir.path(),&p,Some(&edit)).await.unwrap();std::fs::write(&path,"after").unwrap();
    let result:Value=serde_json::from_str(&op.finish(true,"replaced 1 occurrence(s)").unwrap()).unwrap();assert_eq!(result["success"],true);
}

#[tokio::test]
async fn studio_native_edit_reuses_read_version_and_rejects_stale_snapshot() {
    let dir=tempfile::tempdir().unwrap();let db=dir.path().join("test.db");let p=policy(dir.path());let task=Uuid::new_v4();running_task(&db,task);
    let path=dir.path().join("a.txt");std::fs::write(&path,"before").unwrap();
    let read=call("read_file",json!({"path":"a.txt"}),Uuid::new_v4());
    let op=contracts::Operation::begin(&db,task,"read_file",&read.argv,dir.path(),&p,Some(&read)).await.unwrap();
    let read_result:Value=serde_json::from_str(&op.finish(true,"before").unwrap()).unwrap();

    let edit=contracts::decode(&json!({"schema_version":1,"tool":"edit_file","arguments":{"path":"a.txt","start_line":1,"end_line":1,"content":"after"}}).to_string()).unwrap();
    assert_eq!(edit.expected,None);
    let op=contracts::Operation::begin(&db,task,&edit.envelope.tool,&edit.argv,dir.path(),&p,Some(&edit)).await.unwrap();
    std::fs::write(&path,"after").unwrap();
    let result:Value=serde_json::from_str(&op.finish(true,"updated").unwrap()).unwrap();
    assert_eq!(result["success"],true);
    assert_ne!(result["file_version"],read_result["file_version"]);

    let stale_path=dir.path().join("stale.txt");std::fs::write(&stale_path,"original").unwrap();
    let stale_read=call("read_file",json!({"path":"stale.txt"}),Uuid::new_v4());
    let op=contracts::Operation::begin(&db,task,"read_file",&stale_read.argv,dir.path(),&p,Some(&stale_read)).await.unwrap();
    op.finish(true,"original").unwrap();
    std::fs::write(&stale_path,"changed outside the task").unwrap();
    let stale_edit=contracts::decode(&json!({"schema_version":1,"tool":"edit_file","arguments":{"path":"stale.txt","start_line":1,"end_line":1,"content":"overwrite"}}).to_string()).unwrap();
    let error=contracts::Operation::begin(&db,task,&stale_edit.envelope.tool,&stale_edit.argv,dir.path(),&p,Some(&stale_edit)).await.err().unwrap();
    assert!(error.contains("stale_file_version"));
    assert_eq!(std::fs::read_to_string(stale_path).unwrap(),"changed outside the task");
}

#[tokio::test]
async fn studio_search_replace_requires_explicit_intent_for_multiple_matches() {
    let dir=tempfile::tempdir().unwrap();let db=dir.path().join("test.db");let p=policy(dir.path());let task=Uuid::new_v4();running_task(&db,task);
    let path=dir.path().join("a.txt");std::fs::write(&path,"cell cell").unwrap();
    let read=call("read_file",json!({"path":"a.txt"}),Uuid::new_v4());
    let op=contracts::Operation::begin(&db,task,"read_file",&read.argv,dir.path(),&p,Some(&read)).await.unwrap();
    let receipt:Value=serde_json::from_str(&op.finish(true,"cell cell").unwrap()).unwrap();
    let args=json!({"path":"a.txt","search":"cell","replacement":"tile","expected_version":receipt["file_version"]});
    let broad=call("search_replace",args.clone(),Uuid::new_v4());
    let error=contracts::Operation::begin(&db,task,"search_replace",&broad.argv,dir.path(),&p,Some(&broad)).await.err().unwrap();
    assert!(error.contains("ambiguous_search_replace"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(),"cell cell");
    let mut all_args=args;all_args["replace_all"]=json!(true);
    let all=call("search_replace",all_args,Uuid::new_v4());
    let op=contracts::Operation::begin(&db,task,"search_replace",&all.argv,dir.path(),&p,Some(&all)).await.unwrap();
    std::fs::write(&path,"tile tile").unwrap();
    let result:Value=serde_json::from_str(&op.finish(true,"replaced 2 occurrence(s)").unwrap()).unwrap();
    assert_eq!(result["success"],true);
    assert_eq!(std::fs::read_to_string(path).unwrap(),"tile tile");
}
