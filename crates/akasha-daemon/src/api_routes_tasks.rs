//! Tasks, messages, schedules, and timeline routes.

use crate::agents::{MainAgent, TaskPriority};
use crate::api::{
    build_ack_message, extract_how_to_call_from_message, studio_code_rag_enabled, EventsCache, HumanInputStore, ProgressCache, ProgressEntry,
    TaskEventEntry, TaskUsageStore,
};
use akasha_core::{EventEnvelope, EventType};
use crate::api_http::json_response;
use crate::memory::ShortTermStore;
use crate::memory_actor::LongTermMemoryClient;
use crate::steering_queue::SteeringQueueStore;
use crate::user_profile::UserProfile;
use akasha_store::{
    Schedule, ScheduleException, ScheduleExceptionType, ScheduleStore, TaskRunStatus, TaskStatus,
    TaskStore,
};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::Arc;
use uuid::Uuid;

pub struct RouteCtx<'a> {
    pub store_path: &'a Path,
    pub data_dir: &'a Path,
    pub progress: &'a ProgressCache,
    pub events: &'a EventsCache,
    pub main_agent: &'a MainAgent,
    pub short_term: Option<Arc<ShortTermStore>>,
    pub long_term_client: Option<LongTermMemoryClient>,
    pub human_input_store: Option<HumanInputStore>,
    pub steering_queue: Option<SteeringQueueStore>,
    pub task_usage_store: &'a TaskUsageStore,
}

fn full_path(path_only: &str, query_str: Option<&str>) -> String {
    match query_str.filter(|q| !q.is_empty()) {
        Some(q) => format!("{path_only}?{q}"),
        None => path_only.to_string(),
    }
}

fn decode_url_component(s: &str) -> String {
    urlencoding::decode(s)
        .unwrap_or_else(|_| s.to_string().into())
        .into_owned()
}

pub(crate) async fn get_task_list(store_path: &Path, status_filter: Option<String>) -> String {
    let store = match TaskStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let mut tasks = match store.get_all() {
        Ok(t) => t,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    if let Some(ref status) = status_filter {
        let status = status.trim().to_lowercase();
        if !status.is_empty() {
            tasks.retain(|t| t.status.as_str() == status);
        }
    }
    let list: Vec<serde_json::Value> = tasks
        .into_iter()
        .rev()
        .take(50)
        .map(|t| {
            let label = task_label(t.initial_message.as_ref(), &t.id);
            serde_json::json!({
                "id": t.id.to_string(),
                "parent_task_id": t.parent_task_id.map(|u| u.to_string()),
                "status": t.status.as_str(),
                "assigned_agent": t.assigned_agent,
                "created_at": t.created_at.to_rfc3339(),
                "updated_at": t.updated_at.to_rfc3339(),
                "label": label,
            })
        })
        .collect();
    let body = serde_json::json!({ "tasks": list });
    json_response("200 OK", &body.to_string())
}

pub(crate) async fn get_task_events(store_path: &Path, events: &EventsCache, id: Uuid) -> String {
    let mut list: Vec<TaskEventEntry> = {
        let mut root_events = Vec::new();
        match TaskStore::open(store_path) {
            Ok(store) => {
                if let Ok(persisted) = store.get_events(id) {
                    root_events.extend(persisted.into_iter().map(|e| TaskEventEntry {
                        schema_version: 1,
                        kind: e.event_type.clone(),
                        event_type: e.event_type,
                        payload: e.payload,
                        at: e.at,
                        task_id: Some(id.to_string()),
                    }));
                }
            }
            Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
        }
        let g = events.read().await;
        if let Some(q) = g.get(&id) {
            root_events.extend(q.iter().map(|e| {
                let mut e = e.clone();
                if e.task_id.is_none() {
                    e.task_id = Some(id.to_string());
                }
                e
            }));
        }
        root_events
    };

    let mut child_ids: std::collections::BTreeSet<Uuid> = std::collections::BTreeSet::new();
    if let Ok(store) = TaskStore::open(store_path) {
        if let Ok(children) = store.get_children(id) {
            for child in children {
                child_ids.insert(child.id);
            }
        }
    }
    for entry in &list {
        if entry.event_type != "sub_agent_spawned" {
            continue;
        }
        let Some(payload) = entry.payload.as_ref() else {
            continue;
        };
        for key in ["task_id", "child_task_id", "subtask_id"] {
            if let Some(s) = payload.get(key).and_then(|v| v.as_str()) {
                if let Ok(child_id) = Uuid::parse_str(s) {
                    child_ids.insert(child_id);
                }
            }
        }
    }
    if !child_ids.is_empty() {
        let g = events.read().await;
        for child_id in &child_ids {
            if let Some(q) = g.get(child_id) {
                list.extend(q.iter().map(|e| {
                    let mut e = e.clone();
                    e.task_id = Some(child_id.to_string());
                    e
                }));
            }
        }
        drop(g);
        if let Ok(store) = TaskStore::open(store_path) {
            for child_id in child_ids {
                if let Ok(persisted) = store.get_events(child_id) {
                    list.extend(persisted.into_iter().map(|e| TaskEventEntry {
                        schema_version: 1,
                        kind: e.event_type.clone(),
                        event_type: e.event_type,
                        payload: e.payload,
                        at: e.at,
                        task_id: Some(child_id.to_string()),
                    }));
                }
            }
        }
    }

    let mut seen = std::collections::HashSet::new();
    list.retain(|entry| {
        let payload_key = entry
            .payload
            .as_ref()
            .map(|p| serde_json::to_string(p).unwrap_or_default())
            .unwrap_or_default();
        let key = format!(
            "{}|{}|{}|{}",
            entry.task_id.as_deref().unwrap_or_default(),
            entry.event_type,
            entry.at,
            payload_key
        );
        seen.insert(key)
    });

    // Studio swarm: prefer native worker_started/worker_completed when persisted; synthesize otherwise.
    let has_native_worker_events = list.iter().any(|e| {
        e.event_type == "worker_started" || e.event_type == "worker_completed"
    });
    let mut synthetic: Vec<TaskEventEntry> = Vec::new();
    let mut spawned_workers: Vec<String> = Vec::new();
    let mut saw_failed = false;
    if has_native_worker_events {
        for entry in &list {
            if entry.event_type == "worker_started" {
                if let Some(w) = entry
                    .payload
                    .as_ref()
                    .and_then(|p| p.get("worker_task_id"))
                    .and_then(|v| v.as_str())
                {
                    spawned_workers.push(w.to_string());
                }
            } else if entry.event_type == "worker_completed" {
                let failed = entry
                    .payload
                    .as_ref()
                    .and_then(|p| p.get("success"))
                    .and_then(|v| v.as_bool())
                    == Some(false);
                if failed {
                    saw_failed = true;
                }
            }
        }
    }
    for entry in &list {
        if has_native_worker_events {
            continue;
        }
        if entry.event_type == "sub_agent_spawned" {
            let worker_task_id = entry
                .payload
                .as_ref()
                .and_then(|p| p.get("task_id"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let assigned_agent = entry
                .payload
                .as_ref()
                .and_then(|p| p.get("agent"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "unknown".to_string());
            synthetic.push(TaskEventEntry {
                schema_version: 1,
                kind: "studio_worker_state_changed".to_string(),
                event_type: "studio_worker_state_changed".to_string(),
                payload: Some(serde_json::json!({
                    "state": "spawned",
                    "worker_task_id": worker_task_id.clone(),
                    "assigned_agent": assigned_agent.clone(),
                })),
                at: entry.at.clone(),
                task_id: entry.task_id.clone(),
            });
            synthetic.push(TaskEventEntry {
                schema_version: 1,
                kind: "studio_worker_state_changed".to_string(),
                event_type: "studio_worker_state_changed".to_string(),
                payload: Some(serde_json::json!({
                    "state": "running",
                    "worker_task_id": worker_task_id.clone(),
                    "assigned_agent": assigned_agent,
                })),
                at: entry.at.clone(),
                task_id: entry.task_id.clone(),
            });
            if let Some(w) = worker_task_id {
                spawned_workers.push(w);
            }
        } else if entry.event_type == "task_completed" {
            synthetic.push(TaskEventEntry {
                schema_version: 1,
                kind: "studio_worker_state_changed".to_string(),
                event_type: "studio_worker_state_changed".to_string(),
                payload: Some(serde_json::json!({
                    "state": "completed",
                    "worker_task_id": entry.task_id.clone(),
                })),
                at: entry.at.clone(),
                task_id: entry.task_id.clone(),
            });
        } else if entry.event_type == "task_failed" {
            saw_failed = true;
            synthetic.push(TaskEventEntry {
                schema_version: 1,
                kind: "studio_worker_state_changed".to_string(),
                event_type: "studio_worker_state_changed".to_string(),
                payload: Some(serde_json::json!({
                    "state": "failed",
                    "worker_task_id": entry.task_id.clone(),
                })),
                at: entry.at.clone(),
                task_id: entry.task_id.clone(),
            });
        }
    }
    if saw_failed && spawned_workers.len() > 1 {
        synthetic.push(TaskEventEntry {
            schema_version: 1,
            kind: "studio_conflict_notice".to_string(),
            event_type: "studio_conflict_notice".to_string(),
            payload: Some(serde_json::json!({
                "reason": "Concurrent workers ended in failure; review potential file touch conflicts.",
                "workers": spawned_workers,
            })),
            at: chrono::Utc::now().to_rfc3339(),
            task_id: Some(id.to_string()),
        });
    }
    list.extend(synthetic);

    list.sort_by(|a, b| a.at.cmp(&b.at));
    let body = serde_json::json!({ "task_id": id.to_string(), "events": list });
    json_response("200 OK", &body.to_string())
}

pub(crate) async fn get_task_report(store_path: &Path, events: &EventsCache, id: Uuid) -> String {
    let store = match TaskStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let task = match store.get(id) {
        Ok(Some(t)) => t,
        Ok(None) => return json_response("404 Not Found", r#"{"error":"task_not_found"}"#),
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let persisted_progress = store.get_progress(id).unwrap_or_default();
    let mut done: Vec<String> = persisted_progress
        .iter()
        .filter(|(pct, msg)| *pct >= 100 && !task_progress_is_chat_stub(msg))
        .map(|(_, msg)| msg.clone())
        .collect();
    done.truncate(5);

    let mut failed: Vec<String> = Vec::new();
    if matches!(task.status, TaskStatus::Failed | TaskStatus::Cancelled) {
        if let Some((_, msg)) = persisted_progress
            .iter()
            .rev()
            .find(|(_, msg)| !task_progress_is_chat_stub(msg))
        {
            failed.push(msg.chars().take(1200).collect());
        } else {
            failed.push("La tâche a échoué sans détail explicite.".to_string());
        }
    }

    let mut needs_review: Vec<String> = Vec::new();
    let mut saw_approval_request = store
        .get_events(id)
        .unwrap_or_default()
        .iter()
        .any(|e| e.event_type == "tool_approval_request");
    if !saw_approval_request {
        let g = events.read().await;
        saw_approval_request = g
            .get(&id)
            .map(|q| q.iter().any(|e| e.event_type == "tool_approval_request"))
            .unwrap_or(false);
    }
    if saw_approval_request {
        needs_review.push("Une ou plusieurs actions sensibles ont demandé validation.".to_string());
    }
    let pending_queue = store_path
        .parent()
        .map(crate::permissions_queue::load)
        .map(|q| {
            q.requests
                .into_iter()
                .filter(|r| r.task_id == id.to_string() && r.status == crate::permissions_queue::QueueStatus::Pending)
                .count()
        })
        .unwrap_or(0);
    if pending_queue > 0 {
        needs_review.push(format!("{pending_queue} demande(s) d'approbation en attente."));
    }

    let mut next_steps: Vec<String> = match task.status {
        TaskStatus::Completed => vec![
            "Relire le diff studio et exécuter une vérification locale.".to_string(),
            "Si résultat valide, poursuivre avec la prochaine sous-tâche planifiée.".to_string(),
        ],
        TaskStatus::WaitingUserInput => vec![
            "Répondre à la demande d'approbation ou d'information de l'agent.".to_string(),
        ],
        TaskStatus::Failed | TaskStatus::Cancelled => vec![
            "Analyser la cause d'échec puis relancer avec une consigne ciblée.".to_string(),
        ],
        _ => vec!["Attendre la fin de la tâche puis relire le rapport.".to_string()],
    };
    next_steps.truncate(5);

    let transcript_path = store_path
        .parent()
        .map(|d| d.join("transcripts").join(format!("{id}.json")));
    let transcript = transcript_path
        .as_ref()
        .filter(|p| p.is_file())
        .map(|p| p.to_string_lossy().to_string());

    let body = serde_json::json!({
        "task_id": id.to_string(),
        "status": task.status.as_str(),
        "done": done,
        "needs_review": needs_review,
        "failed": failed,
        "next_steps": next_steps,
        "transcript_path": transcript
    });
    json_response("200 OK", &body.to_string())
}

/// `GET /api/tasks/:id/studio-diff` — fichiers texte modifiés depuis le snapshot de début de tâche (Code Studio racine).
pub(crate) async fn get_task_studio_diff(store_path: &Path, task_id: Uuid) -> String {
    let Some(data_dir) = store_path.parent().map(Path::to_path_buf) else {
        return json_response("500 Internal Server Error", r#"{"error":"no_data_dir"}"#);
    };
    let snap_path = data_dir.join("studio-task-snapshots").join(format!("{task_id}.json"));
    if !snap_path.is_file() {
        return json_response(
            "404 Not Found",
            &serde_json::json!({
                "error": "no_snapshot",
                "task_id": task_id.to_string(),
                "hint": "snapshots are created for root Code Studio tasks only"
            })
            .to_string(),
        );
    }
    let snap_json = match std::fs::read_to_string(&snap_path) {
        Ok(s) => s,
        Err(e) => {
            return json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            );
        }
    };
    let snap: crate::studio_task_snapshot::StudioTaskSnapshot = match serde_json::from_str(&snap_json) {
        Ok(s) => s,
        Err(e) => {
            return json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            );
        }
    };
    let captured_at = snap.captured_at_rfc3339.clone();
    match tokio::task::spawn_blocking(move || crate::studio_task_snapshot::compute_studio_task_diff_from_snapshot(snap)).await {
        Ok(Ok(entries)) => {
            let body = serde_json::json!({
                "task_id": task_id.to_string(),
                "captured_at": captured_at,
                "files": entries,
            });
            json_response("200 OK", &body.to_string())
        }
        Ok(Err(e)) => json_response(
            "500 Internal Server Error",
            &serde_json::json!({ "error": e.to_string() }).to_string(),
        ),
        Err(e) => json_response(
            "500 Internal Server Error",
            &serde_json::json!({ "error": e.to_string() }).to_string(),
        ),
    }
}

/// Heap entry used for `/api/timeline` bounded min-heap of recent events.
/// Ordering uses `at_ms` (milliseconds since Unix epoch) to avoid relying on
/// lexicographic comparison of RFC3339 strings, which can be unreliable when
/// `to_rfc3339()` omits fractional seconds for timestamps at whole-second boundaries.
struct TimelineHeapEntry {
    at: String,
    /// Milliseconds since Unix epoch parsed from `at`; used for all comparisons.
    at_ms: i64,
    counter: usize,
    task_id: String,
    event_type: String,
    payload: Option<serde_json::Value>,
}

impl PartialEq for TimelineHeapEntry {
    fn eq(&self, other: &Self) -> bool {
        (self.at_ms, self.counter) == (other.at_ms, other.counter)
    }
}

impl Eq for TimelineHeapEntry {}

impl PartialOrd for TimelineHeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TimelineHeapEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // We want the *oldest* event to be considered "greatest" so that
        // `BinaryHeap::pop()` removes the oldest when the heap exceeds `limit`.
        (other.at_ms, other.counter).cmp(&(self.at_ms, self.counter))
    }
}

pub(crate) async fn cancel_task(store_path: &Path, id: Uuid, main_agent: &crate::agents::MainAgent) -> String {
    let store = match TaskStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let task = match store.get(id) {
        Ok(Some(t)) => t,
        Ok(None) => return json_response("404 Not Found", r#"{"error":"task_not_found"}"#),
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let cancellable = matches!(
        task.status,
        TaskStatus::Pending | TaskStatus::Queued | TaskStatus::Running
    );
    if !cancellable {
        let body = serde_json::json!({
            "error": "task_not_cancellable",
            "detail": "La tâche est déjà terminée, annulée ou en pause.",
            "status": task.status.as_str()
        });
        return json_response("400 Bad Request", &body.to_string());
    }
    if store.update_status(id, TaskStatus::Cancelled).is_err() {
        return json_response("500 Internal Server Error", r#"{"error":"store"}"#);
    }
    let _ = main_agent.bus().send(
        EventEnvelope::new(
            EventType::TaskCancelled,
            Some(serde_json::json!({ "task_id": id.to_string() })),
        )
        .with_correlation(id),
    );
    let body = serde_json::json!({ "cancelled": true, "task_id": id.to_string() });
    json_response("200 OK", &body.to_string())
}

/// Returns true when a task in `status` can be paused.
/// `Running` : la pause est appliquée en base ; la boucle outils sort au prochain tour et ne marque pas la tâche comme terminée.
pub(crate) fn is_pausable(status: &TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Pending
            | TaskStatus::Queued
            | TaskStatus::Running
            | TaskStatus::WaitingUserInput
    )
}

/// Returns true when a task in `status` can be resumed.
pub(crate) fn is_resumable(status: &TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Paused | TaskStatus::Interrupted | TaskStatus::Failed
    )
}

pub(crate) async fn pause_task(store_path: &Path, id: Uuid, main_agent: &crate::agents::MainAgent) -> String {
    let store = match TaskStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let task = match store.get(id) {
        Ok(Some(t)) => t,
        Ok(None) => return json_response("404 Not Found", r#"{"error":"task_not_found"}"#),
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    if !is_pausable(&task.status) {
        let body = serde_json::json!({
            "error": "task_not_pausable",
            "detail": "La tâche ne peut pas être mise en pause dans son état actuel (terminée, annulée, en pause ou interrompue).",
            "status": task.status.as_str()
        });
        return json_response("400 Bad Request", &body.to_string());
    }
    if store.update_status(id, TaskStatus::Paused).is_err() {
        return json_response("500 Internal Server Error", r#"{"error":"store"}"#);
    }
    let _ = main_agent.bus().send(
        EventEnvelope::new(
            EventType::TaskPaused,
            Some(serde_json::json!({ "task_id": id.to_string() })),
        )
        .with_correlation(id),
    );
    let body = serde_json::json!({ "paused": true, "task_id": id.to_string() });
    json_response("200 OK", &body.to_string())
}

/// Progress text that must not be shown as the sole final reply in chat when a richer answer exists on subtasks.
pub(crate) fn task_progress_is_chat_stub(msg: &str) -> bool {
    let t = msg.trim();
    t.is_empty()
        || matches!(
            t,
            "Terminé."
                | "Done."
                | "Échec."
                | "Annulé."
                | "Failed."
                | "Cancelled."
                | "Sous-tâches en cours."
        )
        || t.starts_with("Task delegated to agent")
}

fn merged_last_progress_snapshot(
    store: &TaskStore,
    mem: &std::collections::HashMap<Uuid, VecDeque<ProgressEntry>>,
    task_id: Uuid,
) -> Option<String> {
    let disk: Option<String> = store
        .get_progress(task_id)
        .ok()
        .and_then(|v| v.last().map(|(_, m)| m.clone()));
    let in_mem: Option<String> = mem
        .get(&task_id)
        .and_then(|q| q.back())
        .map(|e| e.message.clone());
    [disk, in_mem]
        .into_iter()
        .flatten()
        .max_by_key(|s| s.len())
}

pub(crate) fn best_substantive_progress_in_subtree(
    store: &TaskStore,
    mem: &std::collections::HashMap<Uuid, VecDeque<ProgressEntry>>,
    task_id: Uuid,
) -> Option<String> {
    if let Some(line) = merged_last_progress_snapshot(store, mem, task_id) {
        let tr = line.trim();
        if !task_progress_is_chat_stub(tr) {
            return Some(tr.to_string());
        }
    }
    let Ok(children) = store.get_children(task_id) else {
        return None;
    };
    let mut best: Option<String> = None;
    for c in children {
        if let Some(m) = best_substantive_progress_in_subtree(store, mem, c.id) {
            if best.as_ref().map(|b| b.len()).unwrap_or(0) < m.len() {
                best = Some(m);
            }
        }
    }
    best
}

pub(crate) async fn resume_task(store_path: &Path, id: Uuid, main_agent: &crate::agents::MainAgent) -> String {
    let store = match TaskStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let task = match store.get(id) {
        Ok(Some(t)) => t,
        Ok(None) => return json_response("404 Not Found", r#"{"error":"task_not_found"}"#),
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    if !is_resumable(&task.status) {
        let body = serde_json::json!({
            "error": "task_not_resumable",
            "detail": "Seules les tâches en pause ou interrompues peuvent être reprises.",
            "status": task.status.as_str()
        });
        return json_response("400 Bad Request", &body.to_string());
    }
    match main_agent.resume_task(store_path, id) {
        Ok(()) => {
            let body = serde_json::json!({ "resumed": true, "task_id": id.to_string() });
            json_response("200 OK", &body.to_string())
        }
        Err(e) => {
            let body = serde_json::json!({ "error": "resume_failed", "detail": e.to_string() });
            json_response("500 Internal Server Error", &body.to_string())
        }
    }
}

/// Short follow-up actions for Code Studio UI (`GET /api/tasks/:id` → `suggested_actions`).
pub(crate) fn code_studio_suggested_actions(
    status: &TaskStatus,
    failure_detail: Option<&str>,
    last_progress: Option<&ProgressEntry>,
    acceptance_review: Option<&serde_json::Value>,
) -> Vec<serde_json::Value> {
    use TaskStatus::*;
    let mut v = Vec::new();
    match status {
        Completed => {
            if let Some(payload) = acceptance_review {
                if let Some(arr) = payload.get("missing").and_then(|m| m.as_array()) {
                    if !arr.is_empty() {
                        let joined = arr
                            .iter()
                            .filter_map(|x| x.as_str())
                            .take(12)
                            .collect::<Vec<_>>()
                            .join("; ");
                        if !joined.is_empty() {
                            v.push(serde_json::json!({
                                "id": "acceptance_review_followup",
                                "label": "Compléter les critères signalés",
                                "kind": "message",
                                "message": format!(
                                    "La tâche est terminée mais la revue des critères a signalé des points à clarifier ou compléter : {joined}\n\nPropose des changements concrets (fichiers + étapes) pour les traiter."
                                )
                            }));
                        }
                    }
                }
            }
            v.push(serde_json::json!({
                "id": "refresh_files",
                "label": "Rafraîchir la liste des fichiers",
                "kind": "ui",
                "ui_action": "refresh_files"
            }));
            v.push(serde_json::json!({
                "id": "open_editor",
                "label": "Onglet Éditeur",
                "kind": "ui",
                "ui_action": "open_editor"
            }));
            v.push(serde_json::json!({
                "id": "open_preview",
                "label": "Ouvrir l’aperçu",
                "kind": "ui",
                "ui_action": "open_preview"
            }));
            v.push(serde_json::json!({
                "id": "open_design",
                "label": "Voir DESIGN.md",
                "kind": "ui",
                "ui_action": "open_design"
            }));
        }
        Failed | Cancelled => {
            let hint = failure_detail
                .map(str::to_string)
                .or_else(|| last_progress.map(|e| e.message.clone()))
                .unwrap_or_default();
            let truncated = if hint.chars().count() > 500 {
                hint.chars().take(500).collect::<String>() + "…"
            } else {
                hint
            };
            v.push(serde_json::json!({
                "id": "analyze_failure",
                "label": "Demander une analyse de l’échec",
                "kind": "message",
                "message": format!(
                    "La dernière tâche a échoué ou été annulée. Contexte:\n{}\n\nPropose un plan de correction ciblé (fichiers et étapes) sans refaire toute l’implémentation.",
                    truncated
                )
            }));
        }
        WaitingUserInput => {
            v.push(serde_json::json!({
                "id": "reply_wait",
                "label": "Rappel : répondre à l’agent",
                "kind": "message",
                "message": "Je complète ma réponse pour l’agent (voir la zone « Réponse requise » dans le suivi de tâche)."
            }));
        }
        Running | Queued | Pending | Paused | Interrupted => {
            v.push(serde_json::json!({
                "id": "wait_continue",
                "label": "Poursuivre après la tâche",
                "kind": "message",
                "message": "Continue sur la base du plan actuel ; je reviens vérifier le résultat une fois la tâche terminée."
            }));
        }
    }
    v
}

pub(crate) async fn get_task_status(
    store_path: &Path,
    progress: &ProgressCache,
    task_usage_store: &TaskUsageStore,
    id: Uuid,
) -> String {
    let store = match TaskStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let task = match store.get(id) {
        Ok(Some(t)) => t,
        Ok(None) => return json_response("404 Not Found", r#"{"error":"task_not_found"}"#),
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let mut progress_list: Vec<ProgressEntry> = store
        .get_progress(id)
        .map(|v| {
            v.into_iter()
                .map(|(pct, msg)| ProgressEntry {
                    progress_pct: pct,
                    message: msg,
                    task_id: Some(id.to_string()),
                })
                .collect()
        })
        .unwrap_or_default();
    // Merge in-memory progress (live bus updates) with DB rows so polling shows watchdog/LLM
    // progress even when the persistence writer is contending on SQLite.
    let mem_entries: Vec<ProgressEntry> = {
        let g = progress.read().await;
        g.get(&id)
            .map(|q| q.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_default()
    };
    let db_last_pct = progress_list.last().map(|e| e.progress_pct).unwrap_or(0);
    let mem_last_pct = mem_entries.last().map(|e| e.progress_pct).unwrap_or(0);
    if progress_list.is_empty() {
        progress_list = mem_entries;
    } else if mem_last_pct > db_last_pct || mem_entries.len() > progress_list.len() {
        progress_list = mem_entries;
    }
    // For a root task with children, aggregate child progress so the UI shows intermediate percentages.
    if let Ok(children) = store.get_children(id) {
        if !children.is_empty() {
            // Take a snapshot of in-memory progress for all children and drop the lock
            // before doing any SQLite traversal to avoid holding the RwLock during DB I/O.
            let mem_snapshot: std::collections::HashMap<Uuid, VecDeque<ProgressEntry>> = {
                let g = progress.read().await;
                g.iter()
                    .map(|(k, v)| (*k, v.clone()))
                    .collect()
            };
            let mut sum: u32 = 0;
            for child in &children {
                let child_pct = store
                    .get_progress(child.id)
                    .ok()
                    .and_then(|v| v.last().map(|(pct, _)| *pct as u32))
                    .or_else(|| {
                        mem_snapshot
                            .get(&child.id)
                            .and_then(|q| q.back().map(|e| e.progress_pct as u32))
                    })
                    .unwrap_or(0);
                sum += child_pct;
            }
            let aggregated_pct = (sum / children.len() as u32).min(100) as u8;
            let root_last_pct = progress_list.last().map(|e| e.progress_pct).unwrap_or(0);
            let display_pct = aggregated_pct.max(root_last_pct);
            if progress_list.is_empty() {
                progress_list.push(ProgressEntry {
                    progress_pct: display_pct,
                    message: "Sous-tâches en cours.".to_string(),
                    task_id: Some(id.to_string()),
                });
            } else if let Some(last) = progress_list.last_mut() {
                last.progress_pct = display_pct;
            }
            // Chat UI polls root task_id: last progress line must reflect the real answer when the root row is only a generic completion stub while children hold the substantive reply.
            if progress_list
                .last()
                .map(|e| task_progress_is_chat_stub(&e.message))
                .unwrap_or(false)
            {
                let mut best: Option<String> = None;
                for c in &children {
                    if let Some(m) = best_substantive_progress_in_subtree(&store, &mem_snapshot, c.id) {
                        if best.as_ref().map(|b| b.len()).unwrap_or(0) < m.len() {
                            best = Some(m);
                        }
                    }
                }
                if let (Some(last_mut), Some(b)) = (progress_list.last_mut(), best) {
                    last_mut.message = b;
                }
            }
        }
    }
    let (tokens_used, cost_usd) = task_usage_store.get_task(id).await.unwrap_or((0, 0.0));
    let last_turn = task_usage_store.get_last_turn(id).await.unwrap_or_default();
    let last_turn_model = task_usage_store.get_last_model(id).await;
    let (todos, todos_updated_at) = store
        .get_todos_with_updated_at(id)
        .unwrap_or_else(|_| (Vec::new(), None));
    let todos_json: Vec<serde_json::Value> = todos
        .iter()
        .map(|t| {
            serde_json::json!({
                "id": t.id,
                "title": t.title,
                "status": t.status.as_str()
            })
        })
        .collect();
    let failure_detail: Option<String> = if matches!(
        task.status,
        TaskStatus::Failed | TaskStatus::Cancelled
    ) {
        progress_list
            .iter()
            .rev()
            .find(|e| !task_progress_is_chat_stub(&e.message))
            .map(|e| e.message.chars().take(4000).collect::<String>())
    } else {
        None
    };
    let last_for_suggest = progress_list.last().cloned();
    let acceptance_review = store.get_events(id).ok().and_then(|evs| {
        evs.iter()
            .rev()
            .find(|e| e.event_type == "studio_acceptance_review")
            .and_then(|e| e.payload.clone())
    });
    let suggested = code_studio_suggested_actions(
        &task.status,
        failure_detail.as_deref(),
        last_for_suggest.as_ref(),
        acceptance_review.as_ref(),
    );
    // Dernière ligne de progression par sous-tâche pour le détail Studio (dédoublonnage côté client par task_id).
    if let Ok(children) = store.get_children(id) {
        if !children.is_empty() {
            let mem_snapshot: std::collections::HashMap<Uuid, VecDeque<ProgressEntry>> = {
                let g = progress.read().await;
                g.iter().map(|(k, v)| (*k, v.clone())).collect()
            };
            for c in &children {
                let from_mem = mem_snapshot.get(&c.id).and_then(|q| q.back().cloned());
                let from_disk = store.get_progress(c.id).ok().and_then(|v| {
                    v.last().map(|(pct, msg)| ProgressEntry {
                        progress_pct: *pct,
                        message: msg.clone(),
                        task_id: Some(c.id.to_string()),
                    })
                });
                if let Some(mut e) = from_mem.or(from_disk) {
                    if e.task_id.is_none() {
                        e.task_id = Some(c.id.to_string());
                    }
                    progress_list.push(e);
                }
            }
        }
    }
    let mut body = serde_json::json!({
        "task_id": task.id.to_string(),
        "status": task.status.as_str(),
        "assigned_agent": task.assigned_agent,
        "created_at": task.created_at.to_rfc3339(),
        "updated_at": task.updated_at.to_rfc3339(),
        "progress": progress_list,
        "tokens_used": tokens_used,
        "cost_usd": cost_usd,
        "last_turn_tokens_in": last_turn.prompt_tokens,
        "last_turn_tokens_out": last_turn.completion_tokens,
        "last_turn_cost_usd": last_turn.cost_usd,
        "last_turn_latency_ms": last_turn.latency_ms,
        "last_turn_model_used": last_turn_model,
        "todos": todos_json,
        "suggested_actions": suggested
    });
    if let Some(u) = todos_updated_at {
        body["todos_updated_at"] = serde_json::Value::String(u);
    }
    if let Some(fd) = failure_detail {
        body["failure_detail"] = serde_json::Value::String(fd);
    }
    if let Some(ref ar) = acceptance_review {
        body["acceptance_review"] = ar.clone();
    }
    json_response("200 OK", &body.to_string())
}

pub(crate) async fn get_schedules_list(store_path: &Path) -> String {
    let store = match ScheduleStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let list = match store.list_schedules() {
        Ok(l) => l,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let arr: Vec<serde_json::Value> = list
        .into_iter()
        .map(|s| {
            serde_json::json!({
                "id": s.id.to_string(),
                "name": s.name,
                "description": s.description,
                "enabled": s.enabled,
                "timezone": s.timezone,
                "rrule": s.rrule,
                "interval_seconds": s.interval_seconds,
                "start_at": s.start_at.to_rfc3339(),
                "end_at": s.end_at.map(|t| t.to_rfc3339()),
                "created_at": s.created_at.to_rfc3339(),
                "updated_at": s.updated_at.to_rfc3339()
            })
        })
        .collect();
    let body = serde_json::json!({ "schedules": arr });
    json_response("200 OK", &body.to_string())
}

pub(crate) async fn get_schedule_exceptions(store_path: &Path, schedule_id: Uuid) -> String {
    let store = match ScheduleStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let list = match store.get_exceptions_for_schedule(schedule_id) {
        Ok(l) => l,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let arr: Vec<serde_json::Value> = list
        .into_iter()
        .map(|e| {
            serde_json::json!({
                "id": e.id.to_string(),
                "schedule_id": e.schedule_id.to_string(),
                "type": e.type_.as_str(),
                "date": e.date.format("%Y-%m-%d").to_string(),
                "override_payload": e.override_payload
            })
        })
        .collect();
    let body = serde_json::json!({ "exceptions": arr });
    json_response("200 OK", &body.to_string())
}

pub(crate) async fn post_schedule_exception(
    store_path: &Path,
    schedule_id: Uuid,
    body: Option<Vec<u8>>,
) -> String {
    let json: serde_json::Value = match body.as_deref().and_then(|b| serde_json::from_slice(b).ok())
    {
        Some(j) => j,
        None => return json_response("400 Bad Request", r#"{"error":"invalid_json"}"#),
    };
    let date_str = json.get("date").and_then(|v| v.as_str()).unwrap_or("");
    let date = match chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
        Ok(d) => d,
        Err(_) => {
            return json_response(
                "400 Bad Request",
                r#"{"error":"invalid_date","expected":"YYYY-MM-DD"}"#,
            )
        }
    };
    let type_str = json.get("type").and_then(|v| v.as_str()).unwrap_or("skip");
    let type_ = match type_str {
        "override" => ScheduleExceptionType::Override,
        _ => ScheduleExceptionType::Skip,
    };
    let override_payload = json
        .get("override_payload")
        .and_then(|v| v.as_str())
        .map(String::from);
    let store = match ScheduleStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let e = ScheduleException {
        id: Uuid::new_v4(),
        schedule_id,
        type_,
        date,
        override_payload,
    };
    if store.insert_exception(&e).is_err() {
        return json_response("500 Internal Server Error", r#"{"error":"store"}"#);
    }
    let body = serde_json::json!({
        "id": e.id.to_string(),
        "schedule_id": e.schedule_id.to_string(),
        "type": e.type_.as_str(),
        "date": e.date.format("%Y-%m-%d").to_string()
    });
    json_response("200 OK", &body.to_string())
}

pub(crate) async fn delete_schedule_exception(
    store_path: &Path,
    schedule_id: Uuid,
    exception_id: Uuid,
) -> String {
    let store = match ScheduleStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    match store.delete_exception(schedule_id, exception_id) {
        Ok(true) => {
            let body =
                serde_json::json!({ "deleted": true, "exception_id": exception_id.to_string() });
            json_response("200 OK", &body.to_string())
        }
        Ok(false) => json_response("404 Not Found", r#"{"error":"exception_not_found"}"#),
        Err(_) => json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    }
}

pub(crate) async fn get_schedule_by_id(store_path: &Path, id: Uuid) -> String {
    let store = match ScheduleStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let s = match store.get_schedule(id) {
        Ok(Some(x)) => x,
        Ok(None) => return json_response("404 Not Found", r#"{"error":"schedule_not_found"}"#),
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let body = serde_json::json!({
        "id": s.id.to_string(),
        "name": s.name,
        "description": s.description,
        "enabled": s.enabled,
        "timezone": s.timezone,
        "rrule": s.rrule,
        "interval_seconds": s.interval_seconds,
        "start_at": s.start_at.to_rfc3339(),
        "end_at": s.end_at.map(|t| t.to_rfc3339()),
        "channel_context": s.channel_context,
        "created_at": s.created_at.to_rfc3339(),
        "updated_at": s.updated_at.to_rfc3339()
    });
    json_response("200 OK", &body.to_string())
}

pub(crate) async fn post_schedule(store_path: &Path, body: Option<Vec<u8>>) -> String {
    let json: serde_json::Value = match body.as_deref().and_then(|b| serde_json::from_slice(b).ok())
    {
        Some(j) => j,
        None => return json_response("400 Bad Request", r#"{"error":"invalid_json"}"#),
    };
    let now = chrono::Utc::now();
    let id = Uuid::new_v4();
    let schedule = Schedule {
        id,
        name: json
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        description: json
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        enabled: json
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        timezone: json
            .get("timezone")
            .and_then(|v| v.as_str())
            .unwrap_or("UTC")
            .to_string(),
        rrule: json
            .get("rrule")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        interval_seconds: json.get("interval_seconds").and_then(|v| v.as_u64()),
        start_at: json
            .get("start_at")
            .and_then(|v| v.as_str())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&chrono::Utc))
            .unwrap_or(now),
        end_at: json
            .get("end_at")
            .and_then(|v| v.as_str())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&chrono::Utc)),
        channel_context: json
            .get("channel_context")
            .and_then(|v| v.as_str())
            .map(String::from),
        created_at: now,
        updated_at: now,
    };
    let store = match ScheduleStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    if store.insert_schedule(&schedule).is_err() {
        return json_response("500 Internal Server Error", r#"{"error":"store"}"#);
    }
    let body = serde_json::json!({
        "id": id.to_string(),
        "name": schedule.name,
        "description": schedule.description,
        "enabled": schedule.enabled,
        "timezone": schedule.timezone,
        "rrule": schedule.rrule,
        "interval_seconds": schedule.interval_seconds,
        "start_at": schedule.start_at.to_rfc3339(),
        "end_at": schedule.end_at.map(|t| t.to_rfc3339()),
        "created_at": schedule.created_at.to_rfc3339(),
        "updated_at": schedule.updated_at.to_rfc3339()
    });
    json_response("201 Created", &body.to_string())
}

pub(crate) async fn put_schedule(store_path: &Path, id: Uuid, body: Option<Vec<u8>>) -> String {
    let store = match ScheduleStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let mut s = match store.get_schedule(id) {
        Ok(Some(x)) => x,
        Ok(None) => return json_response("404 Not Found", r#"{"error":"schedule_not_found"}"#),
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let json: serde_json::Value = match body.as_deref().and_then(|b| serde_json::from_slice(b).ok())
    {
        Some(j) => j,
        None => return json_response("400 Bad Request", r#"{"error":"invalid_json"}"#),
    };
    if let Some(v) = json.get("name").and_then(|v| v.as_str()) {
        s.name = v.to_string();
    }
    if let Some(v) = json.get("description").and_then(|v| v.as_str()) {
        s.description = v.to_string();
    }
    if let Some(v) = json.get("enabled").and_then(|v| v.as_bool()) {
        s.enabled = v;
    }
    if let Some(v) = json.get("timezone").and_then(|v| v.as_str()) {
        s.timezone = v.to_string();
    }
    if let Some(v) = json.get("rrule").and_then(|v| v.as_str()) {
        s.rrule = v.to_string();
    }
    if let Some(v) = json.get("interval_seconds").and_then(|v| v.as_u64()) {
        s.interval_seconds = Some(v);
    }
    if let Some(v) = json.get("channel_context") {
        if v.is_null() {
            s.channel_context = None;
        } else if let Some(s_val) = v.as_str() {
            s.channel_context = Some(s_val.to_string());
        } else {
            return json_response(
                "400 Bad Request",
                r#"{"error":"invalid_field_type","field":"channel_context"}"#,
            );
        }
    }
    if store.update_schedule(&s).is_err() {
        return json_response("500 Internal Server Error", r#"{"error":"store"}"#);
    }
    json_response(
        "200 OK",
        &serde_json::json!({ "id": id.to_string() }).to_string(),
    )
}

pub(crate) async fn delete_schedule(store_path: &Path, id: Uuid) -> String {
    let store = match ScheduleStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    if store.delete_schedule(id).is_err() {
        return json_response("500 Internal Server Error", r#"{"error":"store"}"#);
    }
    json_response(
        "200 OK",
        &serde_json::json!({ "deleted": id.to_string() }).to_string(),
    )
}

pub(crate) async fn schedule_set_enabled(store_path: &Path, id: Uuid, enabled: bool) -> String {
    let store = match ScheduleStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let mut s = match store.get_schedule(id) {
        Ok(Some(s)) => s,
        Ok(None) => {
            return json_response("404 Not Found", r#"{"error":"schedule_not_found"}"#);
        }
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    s.enabled = enabled;
    s.updated_at = chrono::Utc::now();
    if store.update_schedule(&s).is_err() {
        return json_response("500 Internal Server Error", r#"{"error":"store"}"#);
    }
    json_response(
        "200 OK",
        &serde_json::json!({ "id": id.to_string(), "enabled": enabled }).to_string(),
    )
}

pub(crate) async fn schedule_run_now(
    store_path: &Path,
    main_agent: &crate::agents::MainAgent,
    schedule_id: Uuid,
) -> String {
    let store = match ScheduleStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let s = match store.get_schedule(schedule_id) {
        Ok(Some(s)) => s,
        Ok(None) => {
            return json_response("404 Not Found", r#"{"error":"schedule_not_found"}"#);
        }
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    if !s.enabled {
        return json_response(
            "400 Bad Request",
            &serde_json::json!({
                "error": "schedule_paused",
                "detail": "Resume the schedule before run-now.",
            })
            .to_string(),
        );
    }
    let msg = s
        .channel_context
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(s.name.as_str());
    let session_id = format!("schedule:{}", schedule_id);
    let correlation = Uuid::new_v4();
    match main_agent
        .handle_message(
            store_path,
            msg,
            correlation,
            true,
            &session_id,
            None,
            TaskPriority::Scheduled,
            None,
            None,
            None,
            false,
        )
        .await
    {
        Ok(task_id) => json_response(
            "200 OK",
            &serde_json::json!({
                "task_id": task_id.to_string(),
                "schedule_id": schedule_id.to_string(),
            })
            .to_string(),
        ),
        Err(e) => json_response(
            "500 Internal Server Error",
            &serde_json::json!({ "error": "run_now_failed", "detail": e.to_string() }).to_string(),
        ),
    }
}

pub(crate) fn task_label(initial_message: Option<&String>, task_id: &Uuid) -> String {
    const LABEL_MAX: usize = 100;
    match initial_message {
        Some(m) if !m.trim().is_empty() => {
            let s = m.trim();
            if s.chars().count() > LABEL_MAX {
                format!("{}…", s.chars().take(LABEL_MAX).collect::<String>())
            } else {
                s.to_string()
            }
        }
        _ => {
            let s = task_id.to_string();
            let suffix = if s.len() >= 8 {
                &s[s.len() - 8..]
            } else {
                s.as_str()
            };
            format!("Tâche …{suffix}")
        }
    }
}

pub(crate) async fn get_task_runs_list(store_path: &Path, path: &str) -> String {
    let store = match ScheduleStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let schedule_id = path
        .split('?')
        .nth(1)
        .and_then(|q| q.split('&').find(|p| p.starts_with("schedule_id=")))
        .and_then(|p| p.strip_prefix("schedule_id="))
        .and_then(|s| Uuid::parse_str(s).ok());
    let list = match store.list_task_runs(schedule_id, 100) {
        Ok(l) => l,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let task_store = TaskStore::open(store_path);
    let task_map: std::collections::HashMap<Uuid, akasha_store::Task> = task_store
        .ok()
        .and_then(|ts| ts.get_all().ok())
        .unwrap_or_default()
        .into_iter()
        .map(|t| (t.id, t))
        .collect();
    let arr: Vec<serde_json::Value> = list
        .into_iter()
        .map(|r| {
            let label = task_map
                .get(&r.task_id)
                .map(|t| task_label(t.initial_message.as_ref(), &r.task_id))
                .unwrap_or_else(|| task_label(None, &r.task_id));
            serde_json::json!({
                "id": r.id.to_string(),
                "schedule_id": r.schedule_id.map(|u| u.to_string()),
                "task_id": r.task_id.to_string(),
                "status": r.status.as_str(),
                "planned_for": r.planned_for.to_rfc3339(),
                "started_at": r.started_at.map(|t| t.to_rfc3339()),
                "ended_at": r.ended_at.map(|t| t.to_rfc3339()),
                "dedup_key": r.dedup_key,
                "label": label,
            })
        })
        .collect();
    let body = serde_json::json!({ "task_runs": arr });
    json_response("200 OK", &body.to_string())
}

pub(crate) async fn get_task_run_by_id(store_path: &Path, id: Uuid) -> String {
    let store = match ScheduleStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let r = match store.get_task_run(id) {
        Ok(Some(x)) => x,
        Ok(None) => return json_response("404 Not Found", r#"{"error":"task_run_not_found"}"#),
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let body = serde_json::json!({
        "id": r.id.to_string(),
        "schedule_id": r.schedule_id.map(|u| u.to_string()),
        "task_id": r.task_id.to_string(),
        "status": r.status.as_str(),
        "planned_for": r.planned_for.to_rfc3339(),
        "started_at": r.started_at.map(|t| t.to_rfc3339()),
        "ended_at": r.ended_at.map(|t| t.to_rfc3339()),
        "dedup_key": r.dedup_key
    });
    json_response("200 OK", &body.to_string())
}

/// GET /api/schedule_run_reports — recent completed schedule runs with schedule name and task result message (for chat).
pub(crate) async fn get_schedule_run_reports(store_path: &Path) -> String {
    let store = match ScheduleStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let task_store = match TaskStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let list = match store.list_task_runs(None, 50) {
        Ok(l) => l,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let completed: Vec<_> = list
        .into_iter()
        .filter(|r| r.status == TaskRunStatus::Completed && r.schedule_id.is_some())
        .collect();
    // Prefetch all needed schedules into a local cache before acquiring the progress lock.
    let mut schedule_names: std::collections::HashMap<Uuid, String> =
        std::collections::HashMap::new();
    for run in &completed {
        if let Some(sid) = run.schedule_id {
            if !schedule_names.contains_key(&sid) {
                if let Ok(Some(schedule)) = store.get_schedule(sid) {
                    schedule_names.insert(sid, schedule.name);
                }
            }
        }
    }
    let reports: Vec<serde_json::Value> = completed
        .into_iter()
        .filter_map(|r| {
            let schedule_id = r.schedule_id?;
            let schedule_name = schedule_names.get(&schedule_id)?.clone();
            let message = task_store
                .get_progress(r.task_id)
                .ok()
                .and_then(|entries| entries.last().map(|(_, msg)| msg.clone()))
                .unwrap_or_else(|| "Exécuté.".to_string());
            Some(serde_json::json!({
                "schedule_id": schedule_id.to_string(),
                "schedule_name": schedule_name,
                "task_id": r.task_id.to_string(),
                "task_run_id": r.id.to_string(),
                "message": message,
                "ended_at": r.ended_at.map(|t| t.to_rfc3339())
            }))
        })
        .collect();
    let body = serde_json::json!({ "reports": reports });
    json_response("200 OK", &body.to_string())
}

pub async fn try_handle(
    method: &str,
    path_only: &str,
    query_str: Option<&str>,
    body: Option<&[u8]>,
    ctx: &RouteCtx<'_>,
) -> Option<String> {
    let path = full_path(path_only, query_str);
    let store_path = ctx.store_path;
    let data_dir = ctx.data_dir;
    let progress = ctx.progress;
    let events = ctx.events;
    let main_agent = ctx.main_agent;
    let short_term = ctx.short_term.clone();
    let _long_term_client = ctx.long_term_client.clone();
    let human_input_store = ctx.human_input_store.clone();
    let steering_queue = ctx.steering_queue.clone();
    let task_usage_store = ctx.task_usage_store;
    let body = body.map(|b| b.to_vec());
if method == "GET" && path.starts_with("/api/timeline") {
    let (limit, task_id_filter) = path
        .split('?')
        .nth(1)
        .map(|q| {
            let mut limit = 50u32;
            let mut task_id_filter = None;
            for part in q.split('&') {
                if let Some(v) = part.strip_prefix("limit=") {
                    if let Ok(n) = v.parse::<u32>() {
                        limit = n.min(200);
                    }
                } else if let Some(v) = part.strip_prefix("task_id=") {
                    if let Ok(id) = Uuid::parse_str(v) {
                        task_id_filter = Some(id);
                    }
                }
            }
            (limit, task_id_filter)
        })
        .unwrap_or((50, None));

    // Use a bounded heap (by timestamp) to keep only the `limit` most recent events.
    // This avoids allocating and sorting a Vec of *all* events.
    let mut heap: BinaryHeap<TimelineHeapEntry> = BinaryHeap::new();
    {
        let g = events.read().await;
        let mut counter: usize = 0;
        for (tid, list) in g.iter() {
            if let Some(filter) = task_id_filter {
                if *tid != filter {
                    continue;
                }
            }
            for e in list.iter() {
                // The custom Ord for TimelineHeapEntry inverts the natural timestamp order so that
                // the *oldest* entry is the "greatest" and gets popped first when the heap exceeds `limit`.
                // This keeps only the `limit` most-recent events without a full sort.
                let at = e.at.clone();
                // Parse to milliseconds for correct ordering (lexicographic RFC3339 comparison
                // is unreliable when fractional seconds are omitted for whole-second values).
                let at_ms = chrono::DateTime::parse_from_rfc3339(&at)
                    .map(|dt| dt.timestamp_millis())
                    .unwrap_or(0);
                let task_id = tid.to_string();
                let event_type = e.event_type.clone();
                let payload = e.payload.clone();
                heap.push(TimelineHeapEntry {
                    at,
                    at_ms,
                    counter,
                    task_id,
                    event_type,
                    payload,
                });
                counter = counter.wrapping_add(1);
                if heap.len() > limit as usize {
                    heap.pop();
                }
            }
        }
    }

    // Extract the top `limit` events and sort them chronologically (oldest to newest).
    let mut selected: Vec<(String, String, Option<serde_json::Value>, String, i64)> = heap
        .into_iter()
        .map(|entry| {
            (
                entry.task_id,
                entry.event_type,
                entry.payload,
                entry.at,
                entry.at_ms,
            )
        })
        .collect();
    selected.sort_by(|(_, _, _, _, a_ms), (_, _, _, _, b_ms)| a_ms.cmp(b_ms));

    let list: Vec<serde_json::Value> = selected
        .into_iter()
        .map(|(task_id, event_type, payload, at, _)| {
            serde_json::json!({ "task_id": task_id, "event_type": event_type, "payload": payload, "at": at })
        })
        .collect();
    let body_json = serde_json::json!({ "events": list });
    return Some(json_response("200 OK", &body_json.to_string()));
}

if method == "POST" && path == "/api/message" {
    let body_json = body
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
    let mut message = body_json
        .as_ref()
        .and_then(|v| v.get("message").and_then(|v| v.as_str().map(String::from)))
        .unwrap_or_default();
    // Parse attachments: images -> data URLs for vision; documents -> append extracted text to message.
    let image_data_urls: Option<Vec<String>> = {
        let arr = body_json
            .as_ref()
            .and_then(|v| v.get("attachments").and_then(|a| a.as_array()));
        let mut urls = Vec::new();
        let mut doc_texts = Vec::new();
        if let Some(arr) = arr {
            for att in arr {
                let typ = att.get("type").and_then(|t| t.as_str()).unwrap_or("");
                let content_base64 = att
                    .get("content_base64")
                    .and_then(|c| c.as_str())
                    .unwrap_or("");
                let mime = att
                    .get("mime_type")
                    .and_then(|m| m.as_str())
                    .unwrap_or("image/png");
                let name = att.get("name").and_then(|n| n.as_str()).unwrap_or("file");
                if content_base64.is_empty() {
                    continue;
                }
                if typ == "image" || mime.starts_with("image/") {
                    let data_url = format!("data:{};base64,{}", mime, content_base64);
                    urls.push(data_url);
                } else if typ == "document"
                    || mime.starts_with("text/")
                    || mime == "application/pdf"
                {
                    if let Ok(decoded) = base64::Engine::decode(
                        &base64::engine::general_purpose::STANDARD,
                        content_base64,
                    ) {
                        let text = if mime == "application/pdf" {
                            pdf_extract::extract_text_from_mem(&decoded).unwrap_or_else(|_| {
                                String::from(
                                    "[Extraction du texte PDF impossible ou PDF vide.]",
                                )
                            })
                        } else if let Ok(t) = String::from_utf8(decoded) {
                            t
                        } else {
                            continue;
                        };
                        if !text.trim().is_empty() {
                            doc_texts.push(format!("[Document « {} »]\n{}", name, text.trim()));
                        } else if mime == "application/pdf" {
                            doc_texts.push(format!("[Document « {} »]\n[PDF joint : extraction du texte vide (image ou PDF scanné).]", name));
                        }
                    }
                }
            }
        }
        if !doc_texts.is_empty() {
            let user_msg = message.trim_end();
            let user_msg = if user_msg.is_empty() {
                "(Pièce(s) jointe(s))"
            } else {
                user_msg
            };
            message = format!(
                "[Pièce(s) jointe(s) à ce message : quand l'utilisateur dit « ce document », « ce fichier », « analyse-le », « analyse ce document », etc., il parle du contenu joint ci-dessous, pas des échanges précédents.]\n\nMessage : {}\n\n--- Document(s) joint(s) ---\n{}",
                user_msg,
                doc_texts.join("\n\n")
            );
        } else if message.trim().is_empty() && !urls.is_empty() {
            // Pièces jointes images uniquement : éviter message vide pour la tâche.
            message = "(Pièce(s) jointe(s))".to_string();
        }
        if urls.is_empty() {
            None
        } else {
            Some(urls)
        }
    };
    // Session: "new_session" => new UUID; else provided non-empty session_id; else day-YYYY-MM-DD (short-term = current day, survives UI restart).
    let mut session_id = {
        let new_session = body_json
            .as_ref()
            .and_then(|v| v.get("new_session"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let provided = body_json.as_ref().and_then(|v| {
            v.get("session_id")
                .and_then(|v| v.as_str().map(String::from))
        });
        if new_session {
            uuid::Uuid::new_v4().to_string()
        } else if let Some(s) = provided {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                format!("day-{}", chrono::Utc::now().format("%Y-%m-%d"))
            } else {
                trimmed.to_string()
            }
        } else {
            format!("day-{}", chrono::Utc::now().format("%Y-%m-%d"))
        }
    };
    if let Err(e) = akasha_core::check_prompt_injection(&message) {
        let body = serde_json::json!({ "error": "prompt_injection_rejected", "detail": e.to_string() });
        return Some(json_response("400 Bad Request", &body.to_string()));
    }
    // Onboarding: if user profile has no how_to_call, try to extract from message and save
    let mut message = message;
    {
        let mut user_profile = UserProfile::load(data_dir);
        if !user_profile.has_how_to_call() && !message.trim().is_empty() {
            let extracted = extract_how_to_call_from_message(message.trim());
            if let Some(name) = extracted {
                user_profile.how_to_call = Some(name.clone());
                user_profile.onboarding_completed = true;
                if user_profile.first_name.is_none()
                    || user_profile
                        .first_name
                        .as_deref()
                        .unwrap_or("")
                        .trim()
                        .is_empty()
                {
                    user_profile.first_name = Some(name.clone());
                }
                let _ = user_profile.save(data_dir);
                message = format!(
                    "[L'utilisateur vient de vous indiquer son prénom : {}. Accueillez-le chaleureusement (ex. « Ravi de te connaître, {} ! ») puis répondez à son message.]\n\n{}",
                    name,
                    name,
                    message
                );
            }
        }
    }
    // Reconnect recap temporarily disabled: it polluted the real user request when the daemon
    // is restarted several times during the same day and could generate repeated summaries.
    // Update last user activity for proactive check-in
    let _ = UserProfile::save_last_activity(data_dir, chrono::Utc::now());
    let priority = body_json
        .as_ref()
        .and_then(|v| v.get("priority").and_then(|p| p.as_str()))
        .map(|s| {
            if s.eq_ignore_ascii_case("high") {
                TaskPriority::UserHigh
            } else {
                TaskPriority::UserNormal
            }
        })
        .unwrap_or(TaskPriority::UserNormal);
    let incognito = body_json
        .as_ref()
        .map(|v| {
            v.get("incognito")
                .and_then(|x| x.as_bool())
                .unwrap_or(false)
                || v.get("no_memory")
                    .and_then(|x| x.as_bool())
                    .unwrap_or(false)
        })
        .unwrap_or(false);
    let studio_code_mode = body_json
        .as_ref()
        .and_then(|v| v.get("studio_code_mode").and_then(|x| x.as_str()))
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty());
    let message_delivery_mode = body_json
        .as_ref()
        .and_then(|v| {
            v.get("queue_mode")
                .or_else(|| v.get("message_delivery_mode"))
                .and_then(|x| x.as_str())
        })
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty());
    let target_task_id: Option<Uuid> = body_json
        .as_ref()
        .and_then(|v| v.get("target_task_id").and_then(|x| x.as_str()))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .and_then(|s| Uuid::parse_str(&s).ok());
    let studio_policy_hint = body_json
        .as_ref()
        .and_then(|v| v.get("studio_policy_hint").and_then(|x| x.as_str()))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let studio_design_hint = body_json
        .as_ref()
        .and_then(|v| v.get("studio_design_hint").and_then(|x| x.as_str()))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let studio_design_doc = body_json
        .as_ref()
        .and_then(|v| v.get("studio_design_doc").and_then(|x| x.as_str()))
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty());
    let studio_acceptance_parsed: Option<crate::api_studio::StudioAcceptancePayload> =
        match body_json
            .as_ref()
            .and_then(|v| v.get("studio_acceptance_criteria"))
        {
            Some(v) => match crate::api_studio::parse_api_acceptance_field(v) {
                Ok(p) => p,
                Err(e) => {
                    let body = serde_json::json!({
                        "error": "invalid_studio_acceptance_criteria",
                        "detail": e
                    });
                    return Some(json_response("400 Bad Request", &body.to_string()));
                }
            },
            None => None,
        };
    let studio_delegate_single_level = body_json
        .as_ref()
        .and_then(|v| v.get("studio_delegate_single_level").and_then(|x| x.as_bool()))
        .unwrap_or(false);
    let studio_project_id = body_json
        .as_ref()
        .and_then(|v| v.get("studio_project_id").and_then(|x| x.as_str()))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let mut studio_ticket_id = body_json
        .as_ref()
        .and_then(|v| v.get("studio_ticket_id").and_then(|x| x.as_str()))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let fork_from_task_id: Option<Uuid> = {
        let raw = body_json
            .as_ref()
            .and_then(|v| v.get("fork_from_task_id").and_then(|x| x.as_str()))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        match raw {
            None => None,
            Some(s) => match Uuid::parse_str(&s) {
                Ok(id) => Some(id),
                Err(_) => {
                    let body = serde_json::json!({ "error": "invalid_fork_from_task_id", "detail": "fork_from_task_id must be a valid UUID" });
                    return Some(json_response("400 Bad Request", &body.to_string()));
                }
            },
        }
    };
    let fork_after_message_index = body_json
        .as_ref()
        .and_then(|v| v.get("fork_after_message_index").and_then(|x| x.as_i64()))
        .filter(|n| *n >= 0)
        .map(|n| n as u64);
    let mut fork_meta_for_task: Option<serde_json::Value> = None;
    if let Some(parent_task_id) = fork_from_task_id.as_ref() {
        let parent_session_id = session_id.clone();
        let fork_session_id = format!("fork-{}", Uuid::new_v4().simple());
        if let Some(st) = short_term.as_ref() {
            let parent_turns = st.get_turns(&parent_session_id).await;
            let keep = fork_after_message_index
                .map(|n| (n as usize).saturating_add(1))
                .unwrap_or(parent_turns.len())
                .min(parent_turns.len());
            for turn in parent_turns.into_iter().take(keep) {
                st.append(&fork_session_id, &turn.role, turn.content).await;
            }
            fork_meta_for_task = Some(serde_json::json!({
                "fork_parent_task_id": parent_task_id.to_string(),
                "fork_parent_session_id": parent_session_id,
                "fork_session_id": fork_session_id,
                "fork_cut_index": fork_after_message_index,
                "fork_cut_turns": keep,
                "schema_version": 1
            }));
        } else {
            fork_meta_for_task = Some(serde_json::json!({
                "fork_parent_task_id": parent_task_id.to_string(),
                "fork_parent_session_id": parent_session_id,
                "fork_session_id": fork_session_id,
                "fork_cut_index": fork_after_message_index,
                "fork_cut_turns": 0usize,
                "schema_version": 1,
                "note": "short_term_store_unavailable"
            }));
        }
        session_id = fork_session_id;
    }
    let studio_disk_root = if let Some(pid) = studio_project_id.as_deref()
    {
        match crate::studio::resolve_studio_project_dir(data_dir, pid.trim()) {
            Ok(p) => {
                let _ = std::fs::create_dir_all(&p);
                Some(p)
            }
            Err(e) => {
                let body = serde_json::json!({ "error": "invalid_studio_project", "detail": e });
                return Some(json_response("400 Bad Request", &body.to_string()));
            }
        }
    } else {
        None
    };
    if let (Some(ref root), Some(_pid)) = (studio_disk_root.as_ref(), studio_project_id.as_ref())
    {
        if studio_ticket_id.is_none() {
            let mode = crate::api_studio::studio_ticket_enforcement_mode(root);
            if mode != "off"
                && crate::api_studio::studio_user_message_suggests_evolution(&message)
            {
                match crate::api_studio::studio_create_evolution_ticket_from_chat(root, &message)
                {
                    Ok(id) => {
                        tracing::info!(
                            ticket_id = %id,
                            "auto-created Code Studio evolution ticket from chat"
                        );
                        studio_ticket_id = Some(id);
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "studio evolution auto-ticket skipped");
                    }
                }
            }
        }
    }
    if let Some(ref root) = studio_disk_root {
        let studio_ticket_enforcement_mode = crate::api_studio::studio_ticket_enforcement_mode(root);
        if studio_ticket_enforcement_mode == "strict" && studio_ticket_id.is_none() {
            let body = serde_json::json!({
                "error": "ticket_required",
                "detail": "studio_ticket_id is required in strict mode"
            });
            return Some(json_response("409 Conflict", &body.to_string()));
        }
        if let Some(ref tid) = studio_ticket_id {
            let Some(ticket) = crate::api_studio::studio_get_ticket(root, tid) else {
                let body = serde_json::json!({
                    "error": "ticket_not_found",
                    "detail": "studio_ticket_id does not match an existing project ticket"
                });
                return Some(json_response("404 Not Found", &body.to_string()));
            };
            if ticket.status == "done" {
                let body = serde_json::json!({
                    "error": "ticket_already_done",
                    "detail": "cannot start execution on a done ticket"
                });
                return Some(json_response("409 Conflict", &body.to_string()));
            }
            if !crate::api_studio::studio_ticket_prerequisite_done(root, &ticket) {
                let body = serde_json::json!({
                    "error": "ticket_prerequisite_pending",
                    "detail": "depends_on_ticket_id must reference a ticket in status \"done\" before this run is allowed"
                });
                return Some(json_response("409 Conflict", &body.to_string()));
            }
        } else if studio_ticket_enforcement_mode == "soft" {
            tracing::warn!("Code Studio soft ticket enforcement: run started without studio_ticket_id");
        }
    }
    let studio_ui_agent_preference = body_json
        .as_ref()
        .and_then(|v| v.get("studio_assigned_agent").and_then(|x| x.as_str()))
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty());
    // Code Studio : toujours router vers le chef de projet ; la valeur UI devient une préférence pour les sous-agents.
    let studio_forced_agent = if studio_disk_root.is_some() {
        Some("studio_project_manager".to_string())
    } else {
        studio_ui_agent_preference.clone()
    };
    let mut studio_evolution_branch = body_json
        .as_ref()
        .and_then(|v| v.get("studio_evolution_branch").and_then(|x| x.as_str()))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if studio_evolution_branch.is_none() {
        if let (Some(pid), Some(eid)) = (
            studio_project_id.as_deref(),
            body_json
                .as_ref()
                .and_then(|v| v.get("studio_evolution_id").and_then(|x| x.as_str())),
        ) {
            studio_evolution_branch =
                crate::api_studio::evolution_branch_for_id(data_dir, pid.trim(), eid.trim());
        }
    }
    // Steering / follow-up: queue on a running task instead of spawning a new root task.
    if let (Some(ref mode_s), Some(ref sq)) = (message_delivery_mode.as_deref(), steering_queue.as_ref())
    {
        if let Some(qmode) = crate::steering_queue::QueueMode::parse(mode_s) {
            if let Some(tid) = sq
                .resolve_running_task(&session_id, target_task_id)
                .await
            {
                let queued = sq.enqueue(tid, qmode, message.clone()).await;
                let event_type = match qmode {
                    crate::steering_queue::QueueMode::Steering => "user_steering_queued",
                    crate::steering_queue::QueueMode::FollowUp => "user_follow_up_queued",
                };
                if let Ok(ts) = TaskStore::open(store_path) {
                    let _ = ts.insert_event(
                        tid,
                        event_type,
                        Some(&serde_json::json!({
                            "queue_id": queued.id,
                            "mode": queued.mode,
                            "preview": queued.text.chars().take(200).collect::<String>(),
                            "schema_version": 1
                        })),
                        &chrono::Utc::now().to_rfc3339(),
                    );
                }
                let body = serde_json::json!({
                    "ack": true,
                    "queued": true,
                    "queue_mode": queued.mode,
                    "queue_id": queued.id,
                    "task_id": tid.to_string(),
                    "session_id": session_id,
                    "message": "Message mis en file pour la tâche en cours."
                });
                return Some(json_response("200 OK", &body.to_string()));
            }
        }
    }

    // Build acknowledgment message before moving `message` into the envelope.
    let ack_message = build_ack_message(&message);
    // Capture the raw user message for code-RAG retrieval before any prefixes are injected.
    let raw_user_message = message.clone();
    let mut message_for_llm = message;
    if let Some(mode) = message_delivery_mode.as_deref() {
        if mode == "steering" || mode == "follow_up" {
            message_for_llm = format!(
                "[Delivery mode hint: `{mode}`. Prefer coherent continuation with the current session state.]\n\n{}",
                message_for_llm
            );
        }
    }
    if let Some(parent_task_id) = fork_from_task_id.as_ref() {
        let cut = fork_after_message_index
            .map(|n| n.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        message_for_llm = format!(
            "[Session fork context] Parent task: {parent_task_id}; cut index: {cut}. Continue from this branch only.\n\n{}",
            message_for_llm
        );
    }
    if let Some(ref root) = studio_disk_root {
        if let Some(plan) = crate::api_studio::studio_code_plan_message_prefix(root) {
            message_for_llm = format!("{plan}{message_for_llm}");
        }
        if let Some(s) = crate::api_studio::studio_project_summary_prefix(root) {
            message_for_llm = format!("{s}{message_for_llm}");
        }
        let (evol_prefix, policy_prefix, tech_prefix) =
            crate::api_studio::studio_meta_prefixes(root);
        if let Some(p) = evol_prefix {
            message_for_llm = format!("{p}{message_for_llm}");
        }
        if let Some(p) = policy_prefix {
            message_for_llm = format!("{p}{message_for_llm}");
        }
        if studio_evolution_branch.is_some() {
            message_for_llm = format!(
                "[Évolution Code Studio — conserver le même périmètre produit et le même type d’application que le dépôt (cf. CODE_STUDIO_PLAN.md ci-dessus et code existant) ; ne pas remplacer par un autre jeu, une autre app ou un autre domaine fonctionnel sauf instruction explicite de l’utilisateur.]\n\n{}",
                message_for_llm
            );
        }
        if let Some(prefix) = tech_prefix {
            message_for_llm = format!("{prefix}{message_for_llm}");
        }
        if let Some(ref m) = studio_code_mode {
            if let Some(p) = crate::api_studio::studio_code_mode_message_prefix(m) {
                message_for_llm = format!("{p}{message_for_llm}");
            }
        }
        if let Some(ref h) = studio_policy_hint {
            if let Some(p) = crate::api_studio::studio_one_shot_policy_hint_prefix(h) {
                message_for_llm = format!("{p}{message_for_llm}");
            }
        }
        if let Some(ref ticket_id) = studio_ticket_id {
            if let Some(ticket) = crate::api_studio::studio_get_ticket(root, ticket_id) {
                message_for_llm = format!(
                    "[Ticket Kanban obligatoire]\n- ticket_id: {}\n- titre: {}\n- assigned_agent: {}\n- review_agent: {}\n- status: {}\n- contraintes: ne pas clôturer en done; produire un résultat exécutable et laisser le ticket en review pour validation.\n\n{}",
                    ticket.id,
                    ticket.title,
                    ticket.assigned_agent,
                    ticket.review_agent,
                    ticket.status,
                    message_for_llm
                );
            }
        }
        if let Some(ref h) = studio_design_hint {
            if let Some(p) = crate::api_studio::studio_design_hint_prefix(h) {
                message_for_llm = format!("{p}{message_for_llm}");
            }
        }
        if let Some(ref d) = studio_design_doc {
            if let Some(p) = crate::api_studio::studio_design_doc_prefix(d) {
                message_for_llm = format!("{p}{message_for_llm}");
            }
        }
        if studio_code_rag_enabled() {
            if let Some(pid) = studio_project_id.as_deref() {
                let query = raw_user_message.clone();
                let data_dir = data_dir.to_path_buf();
                let root = root.clone();
                let pid = pid.to_string();
                let top_k = std::env::var("AKASHA_STUDIO_CODE_RAG_TOP_K")
                    .ok()
                    .and_then(|s| s.parse::<usize>().ok())
                    .filter(|&n| n > 0 && n <= 30)
                    .unwrap_or(8);
                let max_chars = std::env::var("AKASHA_STUDIO_CODE_RAG_MAX_CHARS")
                    .ok()
                    .and_then(|s| s.parse::<usize>().ok())
                    .filter(|&n| n >= 800 && n <= 30_000)
                    .unwrap_or(6_000);
                let code_ctx = tokio::task::spawn_blocking(move || {
                    let store = crate::code_rag::CodeRagStore::new(&data_dir);
                    let chunks = store.retrieve(
                        &pid,
                        &root,
                        &query,
                        crate::code_rag::RetrieveOptions { top_k, max_chars },
                    )?;
                    Ok::<_, anyhow::Error>(crate::code_rag::format_retrieved_chunks(
                        &chunks, max_chars,
                    ))
                })
                .await
                .ok()
                .and_then(|r| r.ok())
                .flatten();
                if let Some(prefix) = code_ctx {
                    message_for_llm = format!("{prefix}{message_for_llm}");
                }
            }
        }
        if studio_delegate_single_level {
            message_for_llm = format!(
                "[Délégation : privilégier une seule passe agent — éviter les sous-agents ou tâches parallèles implicites sans accord utilisateur.]\n\n{}",
                message_for_llm
            );
        } else {
            message_for_llm = format!(
                "[Délégation obligatoire (Code Studio ; option « délégation simple » désactivée) : tu dois router la demande via `delegate_to_agent <agent_type> <message>` vers l’agent le plus adapté (`conversation`, `code`, `qa`, `studio_planner`, `studio_scaffold`, `studio_frontend`, `studio_backend`, `studio_fullstack`, ou autre spécialiste reconnu). Le sous-agent exécute le travail sur le dépôt ; en tant que chef de projet, n’implémente pas toi-même le code applicatif dans ce tour (`write_file`, `search_replace`, `run_command` sur les sources) — délègue. Exception : seuls les fichiers de planification imposés par les règles Code Studio (`workspace:/specs/…`, sections de `CODE_STUDIO_PLAN.md`) peuvent être mis à jour par toi si une évolution l’exige avant délégation. Les sous-agents ne rappellent pas `delegate_to_agent`.\n\n{}",
                message_for_llm
            );
        }
        if let Some(pref) = studio_ui_agent_preference.as_ref() {
            if !pref.is_empty() && pref != "studio_project_manager" {
                message_for_llm = format!(
                    "[Préférence d’implémentation (sélection UI Code Studio) : `{pref}` — en déléguant via `delegate_to_agent`, oriente les sous-tâches vers le spécialiste le plus adapté (ex. studio_frontend, studio_backend, studio_fullstack, studio_scaffold, qa).]\n\n{}",
                    message_for_llm
                );
            }
        }
        if let Some(ref pay) = studio_acceptance_parsed {
            let prefix = crate::api_studio::format_acceptance_prefix_for_llm(pay);
            if !prefix.is_empty() {
                message_for_llm = format!("{prefix}{message_for_llm}");
            }
            if let Ok(embed) = serde_json::to_string(pay) {
                message_for_llm.push_str(crate::api_studio::STUDIO_ACCEPTANCE_JSON_BEGIN);
                message_for_llm.push_str(&embed);
                message_for_llm.push_str(crate::api_studio::STUDIO_ACCEPTANCE_JSON_END);
            }
        }
    }
    let mut envelope = crate::gateway::MessageEnvelope::api(
        session_id.clone(),
        message_for_llm,
        image_data_urls,
        priority,
        incognito,
    );
    envelope.studio_disk_root = studio_disk_root.clone();
    envelope.studio_forced_agent = studio_forced_agent;
    envelope.studio_evolution_branch = studio_evolution_branch;
    match crate::gateway::handle_envelope(main_agent, store_path, envelope).await {
        Ok(task_id) => {
            if let (Some(root), Some(ticket_id)) = (studio_disk_root.as_ref(), studio_ticket_id.as_ref()) {
                let _ = crate::api_studio::studio_attach_task_to_ticket(
                    root,
                    ticket_id,
                    &task_id.to_string(),
                    "system",
                );
                if let Ok(task_store) = TaskStore::open(store_path) {
                    let _ = task_store.insert_event(
                        task_id,
                        "studio_ticket_link",
                        Some(&serde_json::json!({
                            "ticket_id": ticket_id,
                        })),
                        &chrono::Utc::now().to_rfc3339(),
                    );
                }
            }
            if let Some(meta) = fork_meta_for_task {
                if let Ok(task_store) = TaskStore::open(store_path) {
                    let _ = task_store.insert_event(
                        task_id,
                        "session_fork_created",
                        Some(&meta),
                        &chrono::Utc::now().to_rfc3339(),
                    );
                }
            }
            let body = serde_json::json!({
                "ack": true,
                "task_id": task_id.to_string(),
                "session_id": session_id,
                "message": ack_message
            });
            return Some(json_response("200 OK", &body.to_string()));
        }
        Err(_) => {
            return Some(json_response("500 Internal Server Error", r#"{"error":"handle_failed"}"#));
        }
    }
}

    if method == "GET" && (path == "/api/tasks" || path.starts_with("/api/tasks?")) {
    let status_filter = path.split('?').nth(1).and_then(|q| {
        q.split('&').find(|p| p.starts_with("status=")).map(|p| {
            let raw = p.trim_start_matches("status=");
            decode_url_component(raw)
        })
    });
    return Some(get_task_list(store_path, status_filter).await);
}
// GET /api/pending-human-input — list all tasks waiting for user input (so UI can show notifications after reload or when user was away)
if method == "GET" && path == "/api/pending-human-input" {
    if let Some(ref store) = human_input_store {
        let g = store.read().await;
        let pending: Vec<_> = g
            .iter()
            .map(|(id, p)| {
                serde_json::json!({
                    "task_id": id.to_string(),
                    "question": p.question,
                    "context": p.context,
                    "choices": p.choices
                })
            })
            .collect();
        let body = serde_json::json!({ "pending": pending });
        return Some(json_response("200 OK", &body.to_string()));
    }
    return Some(json_response("200 OK", r#"{"pending":[]}"#));
}
if path.starts_with("/api/tasks/") {
    let rest = path.trim_start_matches("/api/tasks/");
    let parts: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    if let Some(&id_str) = parts.first() {
        if let Ok(id) = Uuid::parse_str(id_str) {
            if method == "POST" && parts.get(1) == Some(&"cancel") {
                return Some(cancel_task(store_path, id, main_agent).await);
            }
            if method == "POST" && parts.get(1) == Some(&"pause") {
                return Some(pause_task(store_path, id, main_agent).await);
            }
            if method == "POST" && parts.get(1) == Some(&"resume") {
                return Some(resume_task(store_path, id, main_agent).await);
            }
            if method == "GET" && parts.get(1) == Some(&"queue") {
                if let Some(ref sq) = steering_queue {
                    let body = sq.snapshot(id).await;
                    return Some(json_response("200 OK", &body.to_string()));
                }
                return Some(json_response("200 OK", r#"{"steering":[],"follow_up":[]}"#));
            }
            if method == "DELETE" && parts.get(1) == Some(&"queue") {
                if let Some(ref sq) = steering_queue {
                    let (s, f) = sq.flush(id).await;
                    if let Ok(ts) = TaskStore::open(store_path) {
                        let _ = ts.insert_event(
                            id,
                            "user_queue_flushed",
                            Some(&serde_json::json!({
                                "steering_removed": s,
                                "follow_up_removed": f,
                                "schema_version": 1
                            })),
                            &chrono::Utc::now().to_rfc3339(),
                        );
                    }
                    let body = serde_json::json!({ "ok": true, "steering_removed": s, "follow_up_removed": f });
                    return Some(json_response("200 OK", &body.to_string()));
                }
                return Some(json_response("200 OK", r#"{"ok":true,"steering_removed":0,"follow_up_removed":0}"#));
            }
            if method == "GET" && parts.get(1) == Some(&"events") {
                return Some(get_task_events(store_path, events, id).await);
            }
            if method == "GET" && parts.get(1) == Some(&"report") {
                return Some(get_task_report(store_path, events, id).await);
            }
            if method == "GET" && parts.get(1) == Some(&"studio-diff") {
                return Some(get_task_studio_diff(store_path, id).await);
            }
            // Human in the loop: GET pending question/context/choices for the task
            if method == "GET" && parts.get(1) == Some(&"human-input") {
                if let Some(ref store) = human_input_store {
                    let g = store.read().await;
                    if let Some(pending) = g.get(&id) {
                        let body = serde_json::json!({
                            "task_id": id.to_string(),
                            "question": pending.question,
                            "context": pending.context,
                            "choices": pending.choices
                        });
                        return Some(json_response("200 OK", &body.to_string()));
                    }
                }
                return Some(json_response("404 Not Found", &serde_json::json!({ "error": "no_pending_human_input", "task_id": id.to_string() }).to_string()));
            }
            // Human in the loop: POST user reply to unblock the agent
            if method == "POST" && parts.get(1) == Some(&"human-reply") {
                let response_text = body
                    .as_deref()
                    .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
                    .and_then(|v| v.get("response").and_then(|r| r.as_str().map(String::from)))
                    .unwrap_or_else(|| String::new());
                if let Some(ref store) = human_input_store {
                    let pending = {
                        let mut g = store.write().await;
                        g.remove(&id)
                    };
                    if let Some(pending) = pending {
                        let _ = pending.response_tx.send(response_text);
                        return Some(json_response("200 OK", &serde_json::json!({ "ok": true, "message": "Réponse transmise à l'agent." }).to_string()));
                    }
                }
                return Some(json_response("404 Not Found", &serde_json::json!({ "error": "no_pending_human_input", "task_id": id.to_string() }).to_string()));
            }
            if method == "GET" {
                return Some(get_task_status(store_path, progress, task_usage_store, id).await);
            }
        }
    }
}

// Schedules and task_runs (FR-028, FR-029)
if method == "GET" && path == "/api/schedules" {
    return Some(get_schedules_list(store_path).await);
}
if method == "GET" && path.starts_with("/api/schedules/") {
    let rest = path.trim_start_matches("/api/schedules/");
    let parts: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    if let Some(&id_str) = parts.first() {
        if let Ok(id) = Uuid::parse_str(id_str) {
            if parts.get(1) == Some(&"exceptions") {
                return Some(get_schedule_exceptions(store_path, id).await);
            }
            return Some(get_schedule_by_id(store_path, id).await);
        }
    }
}
// POST /api/schedules/{id}/pause|resume|run_now — scheduled job ops (enabled flag + manual fire).
if method == "POST" && path.starts_with("/api/schedules/") {
    let rest = path.trim_start_matches("/api/schedules/");
    let parts: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() == 2 {
        if let Ok(id) = Uuid::parse_str(parts[0]) {
            let resp = match parts[1] {
                "pause" => Some(schedule_set_enabled(store_path, id, false).await),
                "resume" => Some(schedule_set_enabled(store_path, id, true).await),
                "run_now" | "run-now" => Some(schedule_run_now(store_path, main_agent, id).await),
                _ => None,
            };
            if let Some(r) = resp {
                return Some(r);
            }
        }
    }
}
if method == "POST" && path.contains("/exceptions") {
    let rest = path.trim_start_matches("/api/schedules/");
    let parts: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    if parts.get(1) == Some(&"exceptions") {
        if let Some(&schedule_id_str) = parts.first() {
            if let Ok(schedule_id) = Uuid::parse_str(schedule_id_str) {
                return Some(post_schedule_exception(store_path, schedule_id, body).await);
            }
        }
    }
}
if method == "DELETE" && path.contains("/exceptions/") {
    let rest = path.trim_start_matches("/api/schedules/");
    let parts: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    if parts.get(1) == Some(&"exceptions") {
        if let (Some(&schedule_id_str), Some(&exception_id_str)) = (parts.first(), parts.get(2))
        {
            if let (Ok(schedule_id), Ok(exception_id)) = (
                Uuid::parse_str(schedule_id_str),
                Uuid::parse_str(exception_id_str),
            ) {
                return Some(delete_schedule_exception(store_path, schedule_id, exception_id).await);
            }
        }
    }
}
if method == "POST" && path == "/api/schedules" {
    return Some(post_schedule(store_path, body).await);
}
if method == "PUT" && path.starts_with("/api/schedules/") {
    let rest = path.trim_start_matches("/api/schedules/");
    if let Some(id_str) = rest.split('/').next() {
        if let Ok(id) = Uuid::parse_str(id_str) {
            return Some(put_schedule(store_path, id, body).await);
        }
    }
}
if method == "DELETE" && path.starts_with("/api/schedules/") {
    let rest = path.trim_start_matches("/api/schedules/");
    if let Some(id_str) = rest.split('/').next() {
        if let Ok(id) = Uuid::parse_str(id_str) {
            return Some(delete_schedule(store_path, id).await);
        }
    }
}
if method == "GET" && path.starts_with("/api/task_runs") {
    return Some(get_task_runs_list(store_path, path.as_str()).await);
}
if method == "GET" && path.starts_with("/api/task_runs/") {
    let rest = path.trim_start_matches("/api/task_runs/");
    if let Some(id_str) = rest.split('/').next() {
        if let Ok(id) = Uuid::parse_str(id_str) {
            return Some(get_task_run_by_id(store_path, id).await);
        }
    }
}
if method == "GET" && path == "/api/schedule_run_reports" {
    return Some(get_schedule_run_reports(store_path).await);
}
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn tasks_paths_smoke() {
        for p in [
            "/api/message",
            "/api/tasks",
            "/api/timeline",
            "/api/schedules",
        ] {
            assert!(
                p == "/api/message"
                    || p.starts_with("/api/tasks")
                    || p.starts_with("/api/timeline")
                    || p.starts_with("/api/schedules")
                    || p.starts_with("/api/task_runs")
            );
        }
    }
}
