//! Durable Studio execution checkpoints. Resume is explicit and rechecks current context.
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use uuid::Uuid;

fn active_runs() -> &'static std::sync::Mutex<std::collections::HashSet<Uuid>> {
    static ACTIVE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<Uuid>>> =
        std::sync::OnceLock::new();
    ACTIVE.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}
pub(crate) fn is_active(task: Uuid) -> bool {
    active_runs()
        .lock()
        .map(|runs| runs.contains(&task))
        .unwrap_or(true)
}
/// Serialize admission with resume's stopped->queued transition and enqueue.
/// Workers wait briefly here instead of losing an already queued execution.
pub(crate) fn with_admission<T>(task:Uuid,action:impl FnOnce()->T)->Result<T,String> {
    use std::sync::{Arc,Mutex,OnceLock,Weak};
    static LOCKS:OnceLock<Mutex<std::collections::HashMap<Uuid,Weak<Mutex<()>>>>>=OnceLock::new();
    let lock={
        let mut locks=LOCKS.get_or_init(||Mutex::new(std::collections::HashMap::new())).lock().map_err(|_|"task admission registry unavailable")?;
        if locks.len()>1024 {locks.retain(|_,lock|lock.strong_count()>0);}
        match locks.get(&task).and_then(Weak::upgrade) {
            Some(lock)=>lock,
            None=>{let lock=Arc::new(Mutex::new(()));locks.insert(task,Arc::downgrade(&lock));lock}
        }
    };
    let _guard=lock.lock().map_err(|_|"task admission unavailable")?;
    Ok(action())
}

pub(crate) struct ActiveRun(Uuid);
impl ActiveRun {
    pub(crate) fn enter(task: Uuid) -> Result<Self, String> {
        with_admission(task,||Self::enter_admitted(task))?
    }
    pub(crate) fn enter_stopped(db:&Path,task:Uuid)->Result<Self,String> {
        with_admission(task,||{
            let record=akasha_store::TaskStore::open(db).map_err(|error|error.to_string())?.get(task).map_err(|error|error.to_string())?.ok_or("task not found")?;
            if !matches!(record.status,akasha_store::TaskStatus::Paused|akasha_store::TaskStatus::Interrupted|akasha_store::TaskStatus::Completed|akasha_store::TaskStatus::Failed|akasha_store::TaskStatus::Cancelled) {return Err("operator delivery requires a stopped task".into());}
            Self::enter_admitted(task)
        })?
    }
    fn enter_admitted(task:Uuid)->Result<Self,String> {
        if !active_runs()
            .lock()
            .map_err(|_| "active execution lock unavailable")?
            .insert(task)
        {
            return Err(
                "task execution is still active; wait for it to stop before resuming".into(),
            );
        }
        Ok(Self(task))
    }
}
impl Drop for ActiveRun {
    fn drop(&mut self) {
        if let Ok(mut runs) = active_runs().lock() {
            runs.remove(&self.0);
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Checkpoint {
    pub schema_version: u32,
    pub task_id: Uuid,
    pub project_id: String,
    pub workspace: PathBuf,
    pub agent: String,
    pub session_id: String,
    pub preferred_task_type: Option<String>,
    pub incognito: bool,
    pub round: u32,
    #[serde(default)]
    pub max_tool_rounds: Option<u32>,
    pub prompt: String,
    #[serde(default)]
    pub conversation: Vec<akasha_llm::ConversationMessage>,
    pub phase: String,
    pub waiting: Option<Value>,
    pub outcome: crate::task_outcome::RunOutcome,
}
fn connection(db: &Path) -> Result<Connection, String> {
    let connection = Connection::open(db).map_err(|e| e.to_string())?;
    connection
        .busy_timeout(std::time::Duration::from_secs(10))
        .map_err(|e| e.to_string())?;
    connection.execute_batch("CREATE TABLE IF NOT EXISTS studio_checkpoints(task TEXT PRIMARY KEY,version INTEGER NOT NULL,body TEXT NOT NULL,resume_requested INTEGER NOT NULL DEFAULT 0,updated_at TEXT NOT NULL);").map_err(|e|e.to_string())?;
    Ok(connection)
}
pub(crate) fn load(
    db: &Path,
    task: Uuid,
    requested_only: bool,
) -> Result<Option<Checkpoint>, String> {
    let connection = connection(db)?;
    let body: Option<String> = connection
        .query_row(
            "SELECT body FROM studio_checkpoints WHERE task=?1 AND (?2=0 OR resume_requested=1)",
            params![task.to_string(), requested_only as i32],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    body.map(|body| {
        if body.len() > 2 * 1024 * 1024 {
            return Err("checkpoint exceeds 2 MiB".into());
        }
        let checkpoint: Checkpoint =
            serde_json::from_str(&body).map_err(|_| "invalid recovery checkpoint")?;
        if checkpoint.schema_version != 1 || checkpoint.task_id != task {
            return Err("unsupported checkpoint version or task".into());
        }
        Ok(checkpoint)
    })
    .transpose()
}
pub(crate) fn save(db: &Path, checkpoint: &Checkpoint) -> Result<(), String> {
    let text = serde_json::to_string(checkpoint).map_err(|e| e.to_string())?;
    if text.len() > 2 * 1024 * 1024 {
        return Err("checkpoint exceeds 2 MiB; execution stopped before next operation".into());
    }
    connection(db)?.execute("INSERT INTO studio_checkpoints(task,version,body,resume_requested,updated_at) VALUES(?1,1,?2,0,?3) ON CONFLICT(task) DO UPDATE SET body=excluded.body,resume_requested=0,updated_at=excluded.updated_at",params![checkpoint.task_id.to_string(),text,chrono::Utc::now().to_rfc3339()]).map_err(|e|e.to_string())?;
    Ok(())
}
pub(crate) fn request_resume(db: &Path, task: Uuid) -> Result<Checkpoint, String> {
    if is_active(task) {
        return Err("task execution is still active; wait for pause/stop before resuming".into());
    }
    let checkpoint = load(db, task, false)?
        .ok_or("Studio task has no durable checkpoint; start a new task explicitly")?;
    let store = akasha_store::TaskStore::open(db).map_err(|e| e.to_string())?;
    let current = store
        .get(task)
        .map_err(|e| e.to_string())?
        .ok_or("task missing")?;
    let project = store
        .lineage_root_studio_project_id(task)
        .map_err(|e| e.to_string())?
        .ok_or("Studio project missing")?;
    if checkpoint.agent != current.assigned_agent || checkpoint.project_id != project {
        return Err("checkpoint role/project differs from current task".into());
    }
    let data_dir=db.parent().ok_or("task data directory missing")?;
    let workspace=crate::studio_project_context::manifest_workspace(data_dir,&project,Some(task))?.canonicalize().map_err(|e|e.to_string())?;
    if checkpoint.workspace.canonicalize().map_err(|_|"recorded workspace unavailable; restore it before retrying resume")?!=workspace {return Err("recorded workspace differs from current task workspace".into());}
    if !matches!(
        current.status,
        akasha_store::TaskStatus::Paused | akasha_store::TaskStatus::Interrupted | akasha_store::TaskStatus::Failed
    ) {
        return Err("task is not resumable".into());
    }
    connection(db)?
        .execute(
            "UPDATE studio_checkpoints SET resume_requested=1 WHERE task=?1",
            [task.to_string()],
        )
        .map_err(|e| e.to_string())?;
    Ok(checkpoint)
}
pub(crate) fn record_waiting(
    db: &Path,
    task: Uuid,
    phase: &str,
    waiting: Value,
    snapshot: (&str, u32, u32, &crate::task_outcome::RunOutcome),
) -> Result<(), String> {
    let Some(mut checkpoint) = load(db, task, false)? else {
        return Err("checkpoint missing before human wait".into());
    };
    checkpoint.phase = phase.into();
    checkpoint.waiting = Some(waiting);
    checkpoint.prompt=snapshot.0.to_owned();
    checkpoint.round=snapshot.1;
    checkpoint.max_tool_rounds=Some(snapshot.2);
    checkpoint.outcome=snapshot.3.clone();
    save(db, &checkpoint)
}
pub(crate) fn mark_interrupted(db: &Path, task: Uuid) -> Result<(), String> {
    if let Some(mut checkpoint) = load(db, task, false)? {
        checkpoint.phase = format!("interrupted:{}", checkpoint.phase);
        save(db, &checkpoint)?;
    }
    Ok(())
}
pub(crate) fn summary(db: &Path, task: Uuid) -> Result<Value, String> {
    let checkpoint = load(db, task, false)?;
    let operations = crate::studio_tool_contract::recovery_summary(db, task)?;
    let children=akasha_store::TaskStore::open(db).map_err(|e|e.to_string())?.get_children(task).map_err(|e|e.to_string())?.into_iter().map(|child|json!({"task_id":child.id,"status":child.status.as_str(),"agent":child.assigned_agent})).collect::<Vec<_>>();
    Ok(
        json!({"schema_version":1,"checkpoint":checkpoint.map(|c|json!({"schema_version":c.schema_version,"project_id":c.project_id,"workspace":c.workspace,"agent":c.agent,"session_id":c.session_id,"round":c.round,"max_tool_rounds":c.max_tool_rounds,"phase":c.phase,"waiting":c.waiting})),"ledger":operations,"children":children,"execution_active":is_active(task)}),
    )
}
pub(crate) fn resolve(
    db: &Path,
    task: Uuid,
    operation: Uuid,
    decision: &str,
    note: &str,
) -> Result<Value, String> {
    if !matches!(decision, "applied" | "not_applied") || note.trim().len() < 8 || note.len() > 2000
    {
        return Err(
            "decision applied/not_applied and a review note (8..2000 characters) required".into(),
        );
    }
    let store = akasha_store::TaskStore::open(db).map_err(|e| e.to_string())?;
    let current = store
        .get(task)
        .map_err(|e| e.to_string())?
        .ok_or("task missing")?;
    if !matches!(
        current.status,
        akasha_store::TaskStatus::Paused
            | akasha_store::TaskStatus::Interrupted
            | akasha_store::TaskStatus::Failed
            | akasha_store::TaskStatus::Completed
            | akasha_store::TaskStatus::Cancelled
    ) {
        return Err("pause the task before resolving an unknown operation".into());
    }
    if is_active(task) {
        return Err("wait for the task execution to stop before resolving effects".into());
    }
    let connection = connection(db)?;
    let record =
        json!({"operation_id":operation,"decision":decision,"note":note,"automatic":false});
    let changed=resolve_stopped_operation(&connection,task,operation,decision,&record)?;
    if changed != 1 {
        return Err("unknown operation not found, already resolved, or task resumed concurrently".into());
    }
    if let Err(error) = store.insert_event(
        task,
        "operation_resolved",
        Some(&record),
        &chrono::Utc::now().to_rfc3339(),
    ) {
        tracing::warn!(%task,%operation,%error,"Recovery decision stored but event unavailable");
    }
    Ok(record)
}

pub(crate) fn route(method: &str, path: &str, body: Option<&[u8]>, db: &Path) -> Option<String> {
    use crate::api_http::json_response;
    let parts = path.trim_matches('/').split('/').collect::<Vec<_>>();
    if parts.len() < 4 || parts[..2] != ["api", "tasks"] || parts[3] != "recovery" {
        return None;
    }
    let Some(task) = Uuid::parse_str(parts[2]).ok() else {
        return Some(json_response("400 Bad Request", "{}"));
    };
    if akasha_store::TaskStore::open(db)
        .ok()
        .and_then(|store| store.get(task).ok().flatten())
        .is_none()
    {
        return Some(json_response("404 Not Found", "{}"));
    }
    let result = if method == "GET" && parts.len() == 4 {
        summary(db, task)
    } else if method == "POST"
        && parts.len() == 7
        && parts[4] == "operations"
        && parts[6] == "resolve"
    {
        let value = body
            .and_then(|body| serde_json::from_slice::<Value>(body).ok())
            .unwrap_or(Value::Null);
        Uuid::parse_str(parts[5])
            .map_err(|e| e.to_string())
            .and_then(|id| {
                resolve(
                    db,
                    task,
                    id,
                    value["decision"].as_str().unwrap_or(""),
                    value["note"].as_str().unwrap_or(""),
                )
            })
    } else {
        Err("unsupported recovery action".into())
    };
    Some(match result {
        Ok(value) => json_response("200 OK", &value.to_string()),
        Err(error) => json_response("409 Conflict", &json!({"error":error}).to_string()),
    })
}

fn resolve_stopped_operation(connection: &Connection, task:Uuid, operation:Uuid, decision:&str, record:&Value) -> Result<usize,String> {
    // The stopped predicate and operation update share one SQLite write. If a
    // competing resume queued the task after the earlier check, no row changes.
    connection.execute("UPDATE studio_operations SET state=?3,updated_at=?4,result=?5 WHERE task=?1 AND operation=?2 AND state='started' AND EXISTS(SELECT 1 FROM tasks WHERE id=?1 AND status IN ('paused','interrupted','failed','completed','cancelled'))",params![task.to_string(),operation.to_string(),format!("resolved_{decision}"),chrono::Utc::now().to_rfc3339(),record.to_string()]).map_err(|e|e.to_string())
}
#[cfg(test)]
mod resolution_race_tests {
    use super::*;
    #[test] fn queued_resume_between_validation_and_resolution_cannot_resolve_the_effect() {
        let connection=Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE tasks(id TEXT PRIMARY KEY,status TEXT);CREATE TABLE studio_operations(task TEXT,operation TEXT,state TEXT,updated_at TEXT,result TEXT);").unwrap();
        let task=Uuid::new_v4();let operation=Uuid::new_v4();
        connection.execute("INSERT INTO tasks VALUES(?1,'paused')",[task.to_string()]).unwrap();
        connection.execute("INSERT INTO studio_operations VALUES(?1,?2,'started','',NULL)",params![task.to_string(),operation.to_string()]).unwrap();
        connection.execute("UPDATE tasks SET status='queued' WHERE id=?1",[task.to_string()]).unwrap();
        assert_eq!(resolve_stopped_operation(&connection,task,operation,"not_applied",&json!({"decision":"not_applied"})).unwrap(),0);
        assert_eq!(connection.query_row("SELECT state FROM studio_operations",[],|r|r.get::<_,String>(0)).unwrap(),"started");
        connection.execute("UPDATE tasks SET status='paused' WHERE id=?1",[task.to_string()]).unwrap();
        assert_eq!(resolve_stopped_operation(&connection,task,operation,"not_applied",&json!({"decision":"not_applied"})).unwrap(),1);
    }
}
