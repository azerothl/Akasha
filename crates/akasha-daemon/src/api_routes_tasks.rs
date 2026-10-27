//! Tasks, messages, schedules, and timeline routes.

/// Consume only the current approval and acknowledge delivery, never just removal.
fn deliver_human_reply(
    waiting: &mut std::collections::HashMap<uuid::Uuid, crate::api::PendingHumanInput>,
    task: uuid::Uuid,
    request_id: Option<&str>,
    response: String,
) -> Result<(), (&'static str, &'static str)> {
    if response.trim().is_empty() { return Err(("400 Bad Request", "empty_human_reply")); }
    let pending = waiting.get(&task).ok_or(("404 Not Found", "no_pending_human_input"))?;
    if pending.response_tx.is_closed() {
        waiting.remove(&task);
        return Err(("410 Gone", "human_input_expired"));
    }
    if pending.request_id.is_some() && pending.request_id.as_deref() != request_id {
        return Err(("409 Conflict", "stale_approval_request"));
    }
    waiting.remove(&task).unwrap().response_tx.send(response)
        .map_err(|_| ("410 Gone", "human_input_expired"))
}

#[cfg(test)]
mod human_reply_delivery_tests {
    use super::deliver_human_reply;
    fn question() -> (uuid::Uuid, std::collections::HashMap<uuid::Uuid, crate::api::PendingHumanInput>, tokio::sync::oneshot::Receiver<String>) {
        let task = uuid::Uuid::new_v4();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let pending = crate::api::PendingHumanInput { request_id:Some("current".into()), question:"Continue?".into(), context:String::new(), choices:None, response_tx:tx };
        (task, [(task, pending)].into_iter().collect(), rx)
    }
    #[test]
    fn stale_or_blank_reply_never_consumes_current_question() {
        let (task, mut pending, mut rx) = question();
        assert_eq!(deliver_human_reply(&mut pending, task, Some("old"), "yes".into()), Err(("409 Conflict", "stale_approval_request")));
        assert_eq!(deliver_human_reply(&mut pending, task, None, "yes".into()), Err(("409 Conflict", "stale_approval_request")));
        assert_eq!(deliver_human_reply(&mut pending, task, Some("current"), " ".into()), Err(("400 Bad Request", "empty_human_reply")));
        assert!(pending.contains_key(&task));
        assert!(matches!(rx.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)));
    }
    #[test]
    fn expired_wait_is_removed_and_never_acknowledged_as_delivered() {
        let (task, mut pending, rx) = question(); drop(rx);
        assert_eq!(deliver_human_reply(&mut pending, task, Some("current"), "yes".into()), Err(("410 Gone", "human_input_expired")));
        assert!(pending.is_empty());
    }
    #[test]
    fn answer_is_delivered_exactly_once() {
        let (task, mut pending, mut rx) = question();
        assert_eq!(deliver_human_reply(&mut pending, task, Some("current"), "exact answer".into()), Ok(()));
        assert_eq!(rx.try_recv().unwrap(), "exact answer");
        assert_eq!(deliver_human_reply(&mut pending, task, Some("current"), "again".into()), Err(("404 Not Found", "no_pending_human_input")));
    }
}

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
use std::collections::{BinaryHeap, HashSet, VecDeque};
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

fn code_studio_root_agent(action: Option<&str>, code_mode: Option<&str>) -> &'static str {
    // A bounded goal is already an explicit implementation workflow with
    // mechanical acceptance criteria. Run it on the implementation agent so
    // its goal loop has the write tools it needs; other root workflows start
    // with the Kanban coordinator and delegate to specialists.
    if action == Some("goal") {
        return "studio_fullstack";
    }
    if code_mode == Some("plan") {
        return "studio_planner";
    }
    "studio_project_manager"
}

fn should_create_studio_chat_ticket(
    has_project: bool,
    has_explicit_ticket: bool,
    has_active_queued_target: bool,
    requested_queue_mode: Option<crate::steering_queue::QueueMode>,
    message: &str,
) -> bool {
    if !has_project {
        return false;
    }
    if has_active_queued_target
        && requested_queue_mode == Some(crate::steering_queue::QueueMode::FollowUp)
    {
        // A queued follow-up is a distinct ticket only when its message asks for
        // actionable work. Courtesy messages should remain ordinary queue text.
        return crate::api_studio::studio_user_message_suggests_evolution(message);
    }
    !has_explicit_ticket
        && !has_active_queued_target
        && crate::api_studio::studio_user_message_suggests_evolution(message)
}

fn queue_delivery_requires_running_target(
    requested_mode: Option<crate::steering_queue::QueueMode>,
    resolved_target: Option<Uuid>,
) -> bool {
    requested_mode.is_some() && resolved_target.is_none()
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
            let session_id = store
                .get_events(t.id)
                .ok()
                .and_then(|evs| {
                    evs.into_iter().find_map(|e| {
                        e.payload.as_ref().and_then(|p| {
                            p.get("session_id")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                        })
                    })
                });
            serde_json::json!({
                "id": t.id.to_string(),
                "parent_task_id": t.parent_task_id.map(|u| u.to_string()),
                "status": t.status.as_str(),
                "assigned_agent": t.assigned_agent,
                "created_at": t.created_at.to_rfc3339(),
                "updated_at": t.updated_at.to_rfc3339(),
                "label": label,
                "session_id": session_id,
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

fn report_done_messages(status:&TaskStatus,progress:&[(u8,String)],outcome:Option<&serde_json::Value>)->Vec<String> {
    if *status!=TaskStatus::Completed || outcome.is_none_or(|value|value["result"]!="succeeded" || value["acceptance_verified"]!=true) {return Vec::new();}
    let mut seen=HashSet::new();
    progress.iter().filter(|(pct,message)|*pct>=100 && !task_progress_is_chat_stub(message))
        .map(|(_,message)|message.trim().to_string()).filter(|message|seen.insert(message.clone())).take(5).collect()
}

#[cfg(test)]
mod report_semantics_tests {
    use super::*;
    #[test]
    fn timeout_is_never_counted_as_a_completed_action_even_at_one_hundred_percent() {
        let progress=vec![(24,"Waiting for model".into()),(100,"LLM response timed out after 300 seconds.".into()),(100,"LLM response timed out after 300 seconds.".into())];
        assert!(report_done_messages(&TaskStatus::Failed,&progress,Some(&serde_json::json!({"result":"failed"}))).is_empty());
        assert!(report_done_messages(&TaskStatus::Completed,&progress,Some(&serde_json::json!({"result":"partial"}))).is_empty());
        assert!(report_done_messages(&TaskStatus::Completed,&progress,Some(&serde_json::json!({"result":"succeeded","acceptance_verified":false}))).is_empty());
    }
    #[test]
    fn completed_messages_are_deduplicated() {
        let progress=vec![(100,"Build verified".into()),(100,"Build verified".into())];
        assert_eq!(report_done_messages(&TaskStatus::Completed,&progress,Some(&serde_json::json!({"result":"succeeded","acceptance_verified":true}))),vec!["Build verified"]);
    }
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
    let outcome=store.get_outcome(id).ok().flatten();
    let done=report_done_messages(&task.status,&persisted_progress,outcome.as_ref());

    let mut failed: Vec<String> = Vec::new();
    if matches!(task.status, TaskStatus::Failed | TaskStatus::Cancelled) {
        if let Some(reason)=outcome.as_ref().and_then(|value|value["reason"].as_str()).filter(|reason|!reason.trim().is_empty()) {
            failed.push(reason.chars().take(2000).collect());
        } else if let Some((_, msg)) = persisted_progress
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
    if task.status==TaskStatus::Completed && outcome.as_ref().is_none_or(|value|value["acceptance_verified"]!=true) {
        needs_review.push("L’exécution est terminée, mais les critères ne sont pas tous vérifiés. La progression à 100 % ne vaut pas validation du résultat.".into());
    }
    if task.status!=TaskStatus::Completed && outcome.as_ref().is_some_and(|value|value["confirmed_file_mutation"]==true) {
        needs_review.push("Des fichiers ont été modifiés avant l’arrêt. Examinez le diff ; la reprise conserve les opérations déjà enregistrées.".into());
    }
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
        TaskStatus::Failed => vec![
            "Consulter le diagnostic, rétablir le fournisseur si nécessaire, puis reprendre cette tâche depuis son checkpoint et son ticket existant.".to_string(),
        ],
        TaskStatus::Cancelled => vec!["Examiner les effets déjà produits avant de démarrer explicitement une nouvelle tâche.".to_string()],
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
        "transcript_path": transcript,
        "outcome": outcome
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
        TaskStatus::Pending | TaskStatus::Queued | TaskStatus::Running | TaskStatus::Paused | TaskStatus::Interrupted | TaskStatus::WaitingUserInput
    );
    if !cancellable {
        let body = serde_json::json!({
            "error": "task_not_cancellable",
            "detail": "La tâche est déjà terminée, annulée ou en pause.",
            "status": task.status.as_str()
        });
        return json_response("400 Bad Request", &body.to_string());
    }
    match store.transition_if(id, &[TaskStatus::Pending, TaskStatus::Queued, TaskStatus::Running, TaskStatus::Paused, TaskStatus::Interrupted, TaskStatus::WaitingUserInput], TaskStatus::Cancelled) {
        Ok(true) => {},
        Ok(false) => return json_response("409 Conflict", r#"{"error":"task_changed_before_cancel"}"#),
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
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

/// Collect root + all descendant task ids (BFS / stack DFS).
pub(crate) fn collect_task_tree_ids(store: &TaskStore, root: Uuid) -> Vec<Uuid> {
    let mut to_cancel: Vec<Uuid> = Vec::new();
    let mut stack = vec![root];
    let mut seen = HashSet::new();
    while let Some(cur) = stack.pop() {
        if !seen.insert(cur) {
            continue;
        }
        to_cancel.push(cur);
        if let Ok(children) = store.get_children(cur) {
            for child in children {
                stack.push(child.id);
            }
        }
    }
    to_cancel
}

fn cancel_tree_member(store: &TaskStore, id: Uuid) -> Result<bool, String> {
    store.transition_if(id,&[TaskStatus::Pending,TaskStatus::Queued,TaskStatus::Running,TaskStatus::Paused,TaskStatus::Interrupted,TaskStatus::WaitingUserInput],TaskStatus::Cancelled).map_err(|e|e.to_string())
}

async fn clear_cancelled_tree_waits(store_path:&Path,response:&str,waits:&HumanInputStore) {
    if !response.starts_with("HTTP/1.1 200 ") {return;}
    let Some(body)=response.split_once("\r\n\r\n").and_then(|(_,body)|serde_json::from_str::<serde_json::Value>(body).ok()) else{return;};
    let Some(ids)=body["cancelled_ids"].as_array() else{return;};
    let Ok(store)=TaskStore::open(store_path) else{return;};
    let mut pending=waits.write().await;
    for id in ids.iter().filter_map(|id|id.as_str().and_then(|id|Uuid::parse_str(id).ok())) {
        // A newer wait created after a concurrent resume must also survive.
        if store.get(id).ok().flatten().is_some_and(|task|task.status==TaskStatus::Cancelled) {pending.remove(&id);}
    }
}

/// P6-B4: cancel a parent task and all descendants (aggregated cancel).
pub(crate) async fn cancel_task_tree(
    store_path: &Path,
    id: Uuid,
    main_agent: &crate::agents::MainAgent,
) -> String {
    let store = match TaskStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let root = match store.get(id) {
        Ok(Some(t)) => t,
        Ok(None) => return json_response("404 Not Found", r#"{"error":"task_not_found"}"#),
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };

    let to_cancel = collect_task_tree_ids(&store, id);

    let mut cancelled: Vec<String> = Vec::new();
    let mut skipped: Vec<serde_json::Value> = Vec::new();
    for tid in to_cancel {
        let Some(task) = store.get(tid).ok().flatten() else {
            continue;
        };
        match cancel_tree_member(&store,tid) {
            Ok(true)=>{},
            Ok(false)=>{skipped.push(serde_json::json!({"task_id":tid.to_string(),"status":store.get(tid).ok().flatten().map(|task|task.status.as_str().to_owned()).unwrap_or_else(||task.status.as_str().to_owned())}));continue;},
            Err(_)=>{skipped.push(serde_json::json!({"task_id":tid.to_string(),"error":"store"}));continue;},
        }
        let _ = main_agent.bus().send(
            EventEnvelope::new(
                EventType::TaskCancelled,
                Some(serde_json::json!({
                    "task_id": tid.to_string(),
                    "aggregated": true,
                    "root_task_id": id.to_string(),
                })),
            )
            .with_correlation(tid),
        );
        cancelled.push(tid.to_string());
    }

    let body = serde_json::json!({
        "cancelled": !cancelled.is_empty(),
        "root_task_id": id.to_string(),
        "root_label": task_label(root.initial_message.as_ref(), &root.id),
        "cancelled_ids": cancelled,
        "skipped": skipped,
    });
    json_response("200 OK", &body.to_string())
}

/// P6-B4: list direct children for subagent thread inspect.
pub(crate) async fn get_task_children(store_path: &Path, id: Uuid) -> String {
    let store = match TaskStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    if store.get(id).ok().flatten().is_none() {
        return json_response("404 Not Found", r#"{"error":"task_not_found"}"#);
    }
    let children = match store.get_children(id) {
        Ok(c) => c,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let list: Vec<serde_json::Value> = children
        .into_iter()
        .map(|t| {
            serde_json::json!({
                "id": t.id.to_string(),
                "parent_task_id": t.parent_task_id.map(|u| u.to_string()),
                "status": t.status.as_str(),
                "assigned_agent": t.assigned_agent,
                "created_at": t.created_at.to_rfc3339(),
                "updated_at": t.updated_at.to_rfc3339(),
                "label": task_label(t.initial_message.as_ref(), &t.id),
            })
        })
        .collect();
    json_response(
        "200 OK",
        &serde_json::json!({ "parent_task_id": id.to_string(), "children": list }).to_string(),
    )
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
    match store.transition_if(id,&[TaskStatus::Pending,TaskStatus::Queued,TaskStatus::Running,TaskStatus::WaitingUserInput],TaskStatus::Paused) {
        Ok(true)=>{},
        Ok(false)=>return json_response("409 Conflict",r#"{"error":"task_changed_before_pause"}"#),
        Err(_)=>return json_response("500 Internal Server Error",r#"{"error":"store"}"#),
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
    let project=store.lineage_root_studio_project_id(id).ok().flatten();
    let _reservation=if let Some(project)=project.as_ref() {
        Some(crate::studio_kanban_dispatch::project_dispatch_lock(project).lock_owned().await)
    } else {None};
    let task = match store.get(id) {
        Ok(Some(t)) => t,
        Ok(None) => return json_response("404 Not Found", r#"{"error":"task_not_found"}"#),
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    if !is_resumable(&task.status) {
        let body = serde_json::json!({
            "error": "task_not_resumable",
            "detail": "Les tâches en pause ou interrompues, et les tâches Code Studio en échec disposant d’un checkpoint, peuvent être reprises.",
            "status": task.status.as_str()
        });
        return json_response("400 Bad Request", &body.to_string());
    }
    if project.as_ref().is_some_and(|project|crate::studio_kanban_dispatch::project_has_active_task(store_path,project)) {
        return json_response("409 Conflict",r#"{"error":"studio_project_busy","detail":"Une autre tâche est active dans ce projet; attendez sa fin avant de reprendre."}"#);
    }
    match main_agent.resume_task(store_path, id) {
        Ok(()) => {
            if let Some(root)=store_path.parent().and_then(|data_dir|store.lineage_root_studio_project_id(id).ok().flatten()
                .and_then(|project|crate::studio::resolve_studio_project_dir(data_dir,&project).ok())) {
                for ticket in crate::api_studio::studio_list_tickets(&root).into_iter().filter(|ticket|ticket.related_task_id.as_deref()==Some(id.to_string().as_str()) && ticket.status!="done") {
                    let _=crate::api_studio::studio_attach_task_to_ticket(&root,&ticket.id,&id.to_string(),"user_resume");
                }
                crate::studio_kanban_dispatch::reconcile_project_tickets(&root,store_path);
            }
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
            let context_window_failure = {
                let lower = hint.to_ascii_lowercase();
                lower.contains("context_window_exceeded")
                    || lower.contains("maximum context length")
                    || (lower.contains("context") && lower.contains("too many tokens"))
            };
            let local_context_preflight = hint
                .to_ascii_lowercase()
                .contains("context_window_preflight_rejected");
            let truncated = if hint.chars().count() > 500 {
                hint.chars().take(500).collect::<String>() + "…"
            } else {
                hint
            };
            if local_context_preflight {
                v.push(serde_json::json!({
                    "id": "choose_model_for_context",
                    "label": "Choisir un modèle adapté",
                    "kind": "ui",
                    "ui_action": "focus_model_selector"
                }));
            } else if context_window_failure {
                v.push(serde_json::json!({
                    "id": "retry_compacted_context",
                    "label": "Reprendre avec un contexte allégé",
                    "kind": "ui",
                    "ui_action": "retry_compacted_context"
                }));
            }
            v.push(serde_json::json!({
                "id": "analyze_failure",
                "label": if local_context_preflight { "Solutions au dépassement de contexte" } else if context_window_failure { "Comprendre l’erreur de contexte" } else { "Demander une analyse de l’échec" },
                "kind": "message",
                "message": format!(
                    "{}\nContexte:\n{}\n\nPropose un plan de correction ciblé sans répéter l’implémentation. Si la cause est un contexte trop volumineux, identifie les résumés ou historiques redondants à compacter tout en conservant la demande, les règles et les résultats récents nécessaires.",
                    if local_context_preflight { "Le précontrôle local a arrêté la requête avant de contacter le fournisseur : le contexte protégé dépasse la fenêtre configurée. Aucun appel au modèle ni outil n'a été exécuté. La reprise automatique est arrêtée pour éviter de soumettre la même requête. Choisissez un modèle avec une fenêtre plus grande ou augmentez le contexte configuré (par exemple Ollama model_options.<modèle>.num_ctx / AKASHA_OLLAMA_NUM_CTX) si la mémoire disponible le permet; réduisez les pièces jointes ou l'historique facultatifs si nécessaire." } else if context_window_failure { "Le fournisseur du modèle a refusé la requête car le contexte dépasse sa fenêtre. Choisissez un modèle à fenêtre plus grande, augmentez le contexte configuré si les ressources le permettent, ou réduisez le contexte facultatif." } else { "La dernière tâche a échoué ou été annulée." },
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
                    // Keep the parent's response last: chat consumes the final
                    // entry, while task details still retain child progress.
                    let at = progress_list.len().saturating_sub(1);
                    progress_list.insert(at, e);
                }
            }
        }
    }
    // Terminal response and task status are persisted atomically. The event bus
    // may still be draining earlier streamed text when polling first sees the
    // terminal status; that text must not replace the final answer or error.
    if matches!(task.status, TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled | TaskStatus::Interrupted) {
        if let Some(outcome) = store.get_outcome(id).ok().flatten() {
            let response = outcome["response"].as_str().or_else(|| {
                if task.status != TaskStatus::Completed { outcome["reason"].as_str() } else { None }
            });
            if let Some(response) = response.filter(|text| !text.trim().is_empty()) {
                progress_list.push(ProgressEntry { progress_pct:100, message:response.to_string(), task_id:Some(id.to_string()) });
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
    if let Ok(Some(outcome)) = store.get_outcome(id) {
        body["outcome"] = outcome;
    }
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
            None,
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

fn composer_message(mode: &str, prompt: &str, message: &str) -> String {
    // Byte length preserves arbitrary multi-paragraph prompts without ambiguous delimiters.
    format!("[Composer mode: {mode}; prompt_bytes: {}]\n{prompt}\n\n{message}", prompt.len())
}

pub(crate) fn task_label(initial_message: Option<&String>, task_id: &Uuid) -> String {
    const LABEL_MAX: usize = 100;
    match initial_message {
        Some(m) if !m.trim().is_empty() => {
            // Labels describe the request, never the private instructions prepended for the LLM.
            // Unwrap before truncating, otherwise the user's words can be lost entirely.
            let mut s = m.trim();
            if s.starts_with("[Task]\nObjective: ") {
                s = s.trim_start_matches("[Task]\nObjective: ");
                s = s.split("\nContext: ").next().unwrap_or(s);
                s = s.split("\nSuccess criteria:").next().unwrap_or(s);
            }
            if s.starts_with("[Orchestrated — disk deliverables REQUIRED]") {
                s = s.splitn(2, "\n\n").nth(1).unwrap_or("").trim();
            }
            if s.starts_with("[Composer mode:") {
                // Old envelopes have no unambiguous end for a user-managed prompt.
                // Keep their labels generic instead of exposing private paragraphs.
                s = s.split_once("]\n").and_then(|(header, rest)| {
                    let (_, length) = header.rsplit_once("; prompt_bytes: ")?;
                    let length = length.parse::<usize>().ok()?;
                    rest.get(length..)?.strip_prefix("\n\n")
                }).unwrap_or("").trim();
            }
            if s.starts_with("[Pièce(s) jointe(s) à ce message") {
                s = s.splitn(2, "\n\nMessage : ").nth(1).unwrap_or("");
                s = s.split("\n\n--- Document(s) joint(s) ---").next().unwrap_or(s).trim();
            }
            let clean = s.split_whitespace().collect::<Vec<_>>().join(" ");
            let s = if clean.is_empty() { "Tâche" } else { clean.as_str() };
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
if method == "GET" && path_only.starts_with("/api/tasks/") && path_only.ends_with("/goal") {
    let raw=path_only.trim_start_matches("/api/tasks/").trim_end_matches("/goal");
    let id=match Uuid::parse_str(raw){Ok(id)=>id,Err(_)=>return Some(json_response("400 Bad Request",r#"{"error":"invalid_task"}"#))};
    return Some(match crate::studio_execution::state(store_path,id) {
        Ok(Some(value))=>json_response("200 OK",&serde_json::to_string(&value).unwrap()),
        Ok(None)=>json_response("404 Not Found",r#"{"error":"no_goal"}"#),
        Err(_)=>json_response("500 Internal Server Error",r#"{"error":"goal_state_unavailable"}"#),
    });
}
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
    let display_request = body_json.as_ref()
        .and_then(|v| v.get("display_message").and_then(|v| v.as_str()))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(&message).to_string();
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
                        if mime == "application/pdf" {
                            match crate::pdf_extract_safe::extract_text_from_mem(&decoded) {
                                Ok(text) => {
                                    doc_texts.push(format!(
                                        "[Document « {} »]\n{}",
                                        name,
                                        text.trim()
                                    ));
                                }
                                Err(failure) => {
                                    doc_texts.push(format!(
                                        "[Document « {} »]\n{}",
                                        name,
                                        crate::pdf_extract_safe::pdf_extract_bracketed_user_message(
                                            failure
                                        )
                                    ));
                                }
                            }
                        } else if let Ok(t) = String::from_utf8(decoded) {
                            if !t.trim().is_empty() {
                                doc_texts.push(format!("[Document « {} »]\n{}", name, t.trim()));
                            }
                        } else {
                            continue;
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
    // Composer mode / named agent profile id (architect | code | ask | …).
    let composer_mode = body_json
        .as_ref()
        .and_then(|v| {
            v.get("composer_mode")
                .or_else(|| v.get("agent_profile_id"))
                .and_then(|x| x.as_str())
        })
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty() && s != "agent");
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
    let studio_execution = match main_agent.validate_studio_execution(body_json.as_ref().unwrap_or(&serde_json::Value::Null)) {
        Ok(value) => value,
        Err(detail) => return Some(json_response("400 Bad Request", &serde_json::json!({"error":"invalid_studio_execution","detail":detail}).to_string())),
    };
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
    // Resolve queue delivery before Kanban registration. Steering continues the active
    // ticket; an explicit follow-up is separate work and receives its own ticket.
    let requested_queue_mode = message_delivery_mode
        .as_deref()
        .and_then(crate::steering_queue::QueueMode::parse);
    let queued_target_task_id = match (
        requested_queue_mode,
        steering_queue.as_ref(),
    ) {
        (Some(_), Some(queue)) => queue
            .resolve_running_task(&session_id, target_task_id)
            .await,
        _ => None,
    };
    if queue_delivery_requires_running_target(requested_queue_mode, queued_target_task_id) {
        let body = serde_json::json!({
            "error": "queue_target_not_running",
            "detail": "Le message n'a pas été envoyé : la tâche cible n'est plus active. Rechargez la conversation avant de créer une nouvelle demande."
        });
        return Some(json_response("409 Conflict", &body.to_string()));
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
    // Serialize user task creation with the Kanban scheduler. Both paths hold
    // this reservation through handle_envelope, then observe the task row that
    // the winner inserted before another writer can start in this project.
    let _studio_project_reservation = if let Some(project_id) = studio_project_id.as_deref() {
        Some(
            crate::studio_kanban_dispatch::project_dispatch_lock(project_id)
                .lock_owned()
                .await,
        )
    } else {
        None
    };
    if queued_target_task_id.is_none()
        && studio_project_id.as_deref().is_some_and(|project_id| {
            crate::studio_kanban_dispatch::project_has_active_task(store_path, project_id)
        })
    {
        let body = serde_json::json!({
            "error": "studio_project_busy",
            "detail": "Une tâche Code Studio est déjà active pour ce projet. Attendez sa fin ou mettez le message en file comme suivi."
        });
        return Some(json_response("409 Conflict", &body.to_string()));
    }
    if let Some(root) = studio_disk_root.as_ref().filter(|_| {
        should_create_studio_chat_ticket(
            studio_project_id.is_some(),
            studio_ticket_id.is_some(),
            queued_target_task_id.is_some(),
            requested_queue_mode,
            &message,
        )
    }) {
        match crate::api_studio::studio_create_evolution_ticket_from_chat(root, &message) {
            Ok(id) => {
                tracing::info!(
                    ticket_id = %id,
                    "auto-created Code Studio evolution ticket from chat"
                );
                studio_ticket_id = Some(id);
            }
            Err(e) => {
                tracing::error!(error = %e, "Code Studio actionable request could not be registered on Kanban");
                let body = serde_json::json!({
                    "error": "studio_ticket_creation_failed",
                    "detail": "La demande n'a pas été lancée car son ticket Kanban n'a pas pu être créé.",
                    "cause": e
                });
                return Some(json_response("500 Internal Server Error", &body.to_string()));
            }
        }
    }
    if let Some(root) = studio_disk_root
        .as_ref()
        .filter(|_| queued_target_task_id.is_none())
    {
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
    if let (Some(root), Some(ticket_id), Some(model)) = (
        studio_disk_root.as_ref(),
        studio_ticket_id.as_deref(),
        studio_execution
            .as_ref()
            .and_then(|execution| execution.model.as_ref()),
    ) {
        if let Err(error) = crate::api_studio::studio_append_ticket_event(
            root,
            ticket_id,
            "studio_model_selected",
            "user_chat",
            Some(serde_json::json!({
                "provider": model.provider,
                "model": model.model,
            })),
        ) {
            tracing::error!(
                ticket_id,
                error = %error,
                "Could not persist the selected model for Code Studio Kanban dispatch"
            );
            let body = serde_json::json!({
                "error": "studio_model_preference_save_failed",
                "detail": "Le modèle choisi n'a pas pu être mémorisé pour les prochains tickets.",
                "cause": error
            });
            return Some(json_response("500 Internal Server Error", &body.to_string()));
        }
    }
    let studio_ui_agent_preference = body_json
        .as_ref()
        .and_then(|v| v.get("studio_assigned_agent").and_then(|x| x.as_str()))
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty());
    // Plan mode uses the technically restricted planner. Other Studio modes use the manager.
    // Chat composer modes: architect/code force the matching specialist when not in Studio.
    let studio_forced_agent = if studio_disk_root.is_some() {
        Some(
            code_studio_root_agent(
                studio_execution
                    .as_ref()
                    .and_then(|value| value.action.as_deref()),
                studio_code_mode.as_deref(),
            )
            .to_string(),
        )
    } else if let Some(ref mode) = composer_mode {
        match mode.as_str() {
            "architect" | "code" => Some(mode.clone()),
            "ask" => Some("ask".to_string()),
            other if crate::api::agent_role_system_prompt(other).is_some() => Some(other.to_string()),
            _ => None,
        }
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
            if let Some(tid) = queued_target_task_id {
                let queued_message = if qmode
                    == crate::steering_queue::QueueMode::FollowUp
                {
                    studio_ticket_id
                        .as_deref()
                        .and_then(|ticket_id| {
                            studio_disk_root.as_ref().and_then(|root| {
                                crate::api_studio::studio_get_ticket(root, ticket_id)
                            })
                        })
                        .map(|ticket| {
                            format!(
                                "{}\n\n[Ticket Kanban obligatoire]\n- ticket_id: {}\n- titre: {}\n- assigned_agent: {}\n- acceptance_criteria: {}\nTraite ce ticket comme un élément de travail distinct de la tâche active, sans dupliquer son code ni son suivi.",
                                message,
                                ticket.id,
                                ticket.title,
                                ticket.assigned_agent,
                                serde_json::to_string(&ticket.acceptance_criteria)
                                    .unwrap_or_else(|_| "[]".to_string())
                            )
                        })
                        .unwrap_or_else(|| message.clone())
                } else {
                    message.clone()
                };
                let queued = sq.enqueue(tid, qmode, queued_message).await;
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
                    "studio_ticket_id": studio_ticket_id,
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
    let mut message_for_llm = if studio_disk_root.is_some() {
        format!("[Current user request — task scope]\n{message}\n[End current user request]\n\nRespect the exact requested paths and contents. Project templates and examples are context; do not modify them unless this request requires it.")
    } else { message };
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
        // Give the project manager a fresh, deterministic view of all unfinished work on
        // every Code Studio run. This makes Kanban review part of task routing instead of
        // relying on a one-time, best-effort bootstrap message.
        let tickets = crate::api_studio::studio_list_tickets(root);
        let current_ticket = studio_ticket_id
            .as_deref()
            .and_then(|ticket_id| crate::api_studio::studio_get_ticket(root, ticket_id));
        let initial_backlog_task = current_ticket.as_ref().is_some_and(|ticket| {
            crate::api_studio::is_initial_backlog_root(ticket)
        });
        let open: Vec<_> = tickets
            .iter()
            .filter(|ticket| !ticket.status.eq_ignore_ascii_case("done"))
            .filter(|ticket| Some(ticket.id.as_str()) != studio_ticket_id.as_deref())
            .take(12)
            .map(|ticket| {
                serde_json::json!({
                    "id": ticket.id,
                    "title": ticket.title,
                    "status": ticket.status,
                    "assigned_agent": ticket.assigned_agent,
                    "depends_on_ticket_ids": ticket.depends_on_ticket_ids,
                    "acceptance_criteria": ticket.acceptance_criteria.iter().take(3).map(|criterion| {
                        serde_json::json!({
                            "id": criterion.id,
                            "text": criterion.text.chars().take(180).collect::<String>(),
                        })
                    }).collect::<Vec<_>>(),
                })
            })
            .collect();
        let kanban_snapshot = serde_json::json!({
            "unfinished_total": tickets.iter().filter(|ticket| !ticket.status.eq_ignore_ascii_case("done")).count(),
            "included": open.len(),
            "current_ticket_included_below": current_ticket.as_ref().map(|ticket| ticket.id.as_str()),
            "tickets": open,
        });
        message_for_llm = format!(
            "[Kanban — instantané compact des tickets non terminés; les détails du ticket courant sont fournis séparément. Vérifie les prérequis avant toute délégation.\n{}\n]\n\n{}",
            kanban_snapshot,
            message_for_llm
        );
        if initial_backlog_task {
            message_for_llm = format!(
                "[Code Studio — première demande : planifier le backlog initial avant toute implémentation. Le ticket courant et la demande utilisateur sont la source de vérité. Les résumés du projet déjà présents dans cette demande ne doivent pas être répétés.]\n\n{}",
                message_for_llm
            );
        }

        let direct_files=studio_delegate_single_level && studio_code_mode.as_deref()!=Some("plan")
            && !matches!(composer_mode.as_deref(),Some("ask"|"architect")) && studio_ticket_id.is_none()
            && studio_evolution_branch.is_none()
            && crate::api_llm_loop::scoped_file_request(&raw_user_message);
        let plan=if direct_files {
            crate::api_studio::scoped_file_plan_context(root)
        } else if initial_backlog_task {
            // The initial project plan mostly repeats the creation request. Keep the
            // request and selected stack; workers can read the full plan when needed.
            None
        } else {
            crate::api_studio::studio_code_plan_message_prefix(root)
        };
        let prune_plan_chunks=direct_files && plan.as_deref().is_some_and(|prefix|prefix.starts_with(crate::api_llm_loop::DIRECT_FILE_TASK_MARKER));
        if let Some(plan) = plan {
            message_for_llm = format!("{plan}{message_for_llm}");
        }
        let (project_summary_prefix, evol_prefix, policy_prefix, tech_prefix) =
            crate::api_studio::studio_meta_prefixes_for_request(root, &raw_user_message);
        if let Some(s) = project_summary_prefix.filter(|_| !initial_backlog_task) {
            message_for_llm = format!("{s}{message_for_llm}");
        }
        if let Some(p) = evol_prefix.filter(|_| !initial_backlog_task) {
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
        if let Some(ticket) = current_ticket.as_ref() {
                let criteria = ticket.acceptance_criteria.iter().take(4).map(|criterion| {
                    format!("- {}", criterion.text.chars().take(180).collect::<String>())
                }).collect::<Vec<_>>().join("\n");
                let dependencies = if ticket.depends_on_ticket_ids.is_empty() {
                    "aucune".to_string()
                } else {
                    ticket.depends_on_ticket_ids.join(", ")
                };
                message_for_llm = format!(
                    "[Ticket Kanban obligatoire]\n- ticket_id: {}\n- titre: {}\n- assigned_agent: {}\n- review_agent: {}\n- status: {}\n- dépendances: {}\n- critères:\n{}\n- contraintes: ne pas clôturer en done; produire un résultat exécutable et laisser le ticket en review pour validation.\n\n{}",
                    ticket.id,
                    ticket.title,
                    ticket.assigned_agent,
                    ticket.review_agent,
                    ticket.status,
                    dependencies,
                    if criteria.is_empty() { "- Aucun critère enregistré" } else { &criteria },
                    message_for_llm
                );
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
                    let mut chunks = store.retrieve(
                        &pid,
                        &root,
                        &query,
                        crate::code_rag::RetrieveOptions { top_k, max_chars },
                    )?;
                    // The direct-file prefix already preserves plan constraints.
                    // Do not reintroduce its maintenance templates through RAG.
                    if prune_plan_chunks {chunks.retain(|chunk|!chunk.path.eq_ignore_ascii_case("CODE_STUDIO_PLAN.md"));}
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
        if studio_code_mode.as_deref() == Some("plan") {
            message_for_llm = format!("[Planification bornée : utiliser les outils de lecture et uniquement CODE_STUDIO_PLAN.md pour l’écriture. Aucune délégation ni commande système.]\n\n{}", message_for_llm);
        } else if studio_execution
            .as_ref()
            .and_then(|value| value.action.as_deref())
            == Some("goal")
        {
            message_for_llm = format!(
                "[Exécution directe du goal Code Studio : tu es l’agent `studio_fullstack`. Le contrat de goal et ses critères mécaniques sont actifs. Implémente directement la demande sur le dépôt, vérifie chaque critère dans l’ordre, corrige les diagnostics avant de continuer, puis consigne les preuves et les fichiers sur le ticket Kanban. Ne tente pas de délégation : ce workflow possède les outils d’édition et de vérification nécessaires.]\n\n{}",
                message_for_llm
            );
        } else if studio_delegate_single_level {
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
    // Composer modes (Ask / Architect / Code) apply to normal chat as well as Code Studio.
    if let Some(ref mode) = composer_mode {
        let prefix = crate::agent_profiles::AgentProfilesStore::new(data_dir)
            .get(mode)
            .ok()
            .flatten()
            .map(|p| p.system_prompt.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| crate::api::agent_role_system_prompt(mode).map(|s| s.to_string()));
        if let Some(p) = prefix {
            message_for_llm = composer_message(mode, &p, &message_for_llm);
        } else if mode == "ask" {
            message_for_llm = composer_message(
                "ask",
                "Answer questions only. Use read-only tools if needed. Do not write files or run mutating shell commands.",
                &message_for_llm,
            );
        }
    }
    if let Some(action)=studio_execution.as_ref().and_then(|value|value.action.as_deref()) {
        let instruction=match action {
            "debug"=>"Reproduce the reported failure, identify its cause from observed evidence, apply a focused fix and verify it. Report unverified assumptions explicitly.",
            "audit"=>"Delegate repository inspection to studio_auditor, which is read-only. Do not modify application source in this workflow. You may read and update the Kanban: create one actionable ticket per confirmed finding, include inspected paths and evidence, then summarize the findings and their ticket IDs.",
            "test"=>"Inspect the project scripts and choose relevant checks for the request. Run them, preserve real exit codes and report failures. Do not modify source files unless the user requested fixes.",
            _=>"Pursue the explicit objective. Mechanical checks and hard budgets are enforced by the daemon; do not claim verification without tool evidence.",
        };
        message_for_llm=format!("[Studio workflow: {action}]\n{instruction}\n\n{message_for_llm}");
    }
    if let Some(goal)=studio_execution.as_ref().and_then(|value|value.goal.as_ref()) {
        message_for_llm=format!("[Explicit goal contract]\nObjective: {}\nHard limits: {} verification cycles, {} seconds, {} main-loop tokens.\n{}\n{}",goal.objective,goal.max_cycles,goal.max_seconds,goal.max_tokens,crate::api_studio::format_acceptance_prefix_for_llm(&goal.criteria),message_for_llm);
    }
    let mut envelope = crate::gateway::MessageEnvelope::api(
        session_id.clone(),
        message_for_llm,
        image_data_urls,
        priority,
        incognito,
    );
    if let Some(ids)=body_json.as_ref().and_then(|body|body.get("document_ids")) {
        match crate::document_conversation::prepare(data_dir,ids,&display_request) {
            Ok(request)=>{envelope.document_request=Some(request);envelope.incognito=true;},
            Err(error)=>return Some(json_response("409 Conflict",&serde_json::json!({"error":error.to_string()}).to_string())),
        }
    }
    envelope.display_request = Some(display_request);
    envelope.studio_execution = studio_execution;
    if let Some(consents) = body_json.as_ref().and_then(|body|body.get("remote_consents")) {
        match serde_json::from_value::<Vec<crate::privacy::Consent>>(consents.clone()) {
            Ok(consents) => envelope.remote_consents = consents,
            Err(_) => return Some(json_response("400 Bad Request",r#"{"error":"invalid_remote_consents"}"#)),
        }
    }
    envelope.studio_disk_root = studio_disk_root.clone();
    envelope.studio_forced_agent = studio_forced_agent;
    envelope.studio_evolution_branch = studio_evolution_branch;
    if envelope.document_request.is_some() {envelope.studio_forced_agent=Some("ask".into());}
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
                crate::studio_kanban_dispatch::reconcile_project_tickets(root,store_path);
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
                "studio_ticket_id": studio_ticket_id,
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
        let mut g = store.write().await;
        g.retain(|_, pending| !pending.response_tx.is_closed());
        let pending: Vec<_> = g
            .iter()
            .map(|(id, p)| {
                serde_json::json!({
                    "task_id": id.to_string(),
                    "request_id": p.request_id,
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
                let response=cancel_task(store_path,id,main_agent).await;
                if response.starts_with("HTTP/1.1 200 ") {if let Some(waits)=human_input_store.as_ref(){waits.write().await.remove(&id);}}
                return Some(response);
            }
            if method == "POST" && parts.get(1) == Some(&"cancel-tree") {
                let response=cancel_task_tree(store_path,id,main_agent).await;
                if let Some(waits)=human_input_store.as_ref(){clear_cancelled_tree_waits(store_path,&response,waits).await;}
                return Some(response);
            }
            if method == "GET" && parts.get(1) == Some(&"children") {
                return Some(get_task_children(store_path, id).await);
            }
            if method == "POST" && parts.get(1) == Some(&"pause") {
                let response=pause_task(store_path,id,main_agent).await;
                if response.starts_with("HTTP/1.1 200 ") {if let Some(waits)=human_input_store.as_ref(){waits.write().await.remove(&id);}}
                return Some(response);
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
                    let mut g = store.write().await;
                    g.retain(|_, pending| !pending.response_tx.is_closed());
                    if let Some(pending) = g.get(&id) {
                        let body = serde_json::json!({
                            "task_id": id.to_string(),
                            "request_id": pending.request_id,
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
                let request_id = body.as_deref().and_then(|b|serde_json::from_slice::<serde_json::Value>(b).ok()).and_then(|v|v["request_id"].as_str().map(str::to_owned));
                let response_text = body
                    .as_deref()
                    .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
                    .and_then(|v| v.get("response").and_then(|r| r.as_str().map(String::from)))
                    .unwrap_or_else(|| String::new());
                if let Some(ref store) = human_input_store {
                    let result = deliver_human_reply(&mut *store.write().await, id, request_id.as_deref(), response_text);
                    match result {
                        Ok(()) => return Some(json_response("200 OK", &serde_json::json!({ "ok": true, "message": "Réponse transmise à l'agent." }).to_string())),
                        Err((status, error)) => return Some(json_response(status, &serde_json::json!({"error":error,"task_id":id}).to_string())),
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
    use super::*;
    use akasha_store::Task;
    use chrono::Utc;
    use tempfile::NamedTempFile;

    #[test]
    fn bounded_goals_run_on_the_implementation_agent_and_other_workflows_keep_the_manager() {
        assert_eq!(code_studio_root_agent(Some("goal"), None), "studio_fullstack");
        assert_eq!(code_studio_root_agent(Some("debug"), None), "studio_project_manager");
        assert_eq!(code_studio_root_agent(Some("test"), None), "studio_project_manager");
        assert_eq!(code_studio_root_agent(Some("audit"), None), "studio_project_manager");
        assert_eq!(code_studio_root_agent(None, Some("plan")), "studio_planner");
    }

    #[test]
    fn steering_reuses_active_work_but_actionable_follow_up_gets_a_ticket() {
        let request = "Ajoute un test pour vérifier la sauvegarde des préférences.";
        assert!(should_create_studio_chat_ticket(
            true, false, false, None, request
        ));
        assert!(!should_create_studio_chat_ticket(
            true,
            false,
            true,
            Some(crate::steering_queue::QueueMode::Steering),
            request
        ));
        assert!(!should_create_studio_chat_ticket(
            true,
            false,
            true,
            Some(crate::steering_queue::QueueMode::FollowUp),
            "Merci !"
        ));
        assert!(should_create_studio_chat_ticket(
            true,
            false,
            true,
            Some(crate::steering_queue::QueueMode::FollowUp),
            request
        ));
        assert!(should_create_studio_chat_ticket(
            true,
            true,
            true,
            Some(crate::steering_queue::QueueMode::FollowUp),
            "Ajoute un mode nuit indépendant pour les captures."
        ));
        assert!(!should_create_studio_chat_ticket(
            true,
            true,
            true,
            Some(crate::steering_queue::QueueMode::Steering),
            request
        ));
        assert!(!should_create_studio_chat_ticket(
            true,
            true,
            false,
            None,
            request
        ));
        assert!(!should_create_studio_chat_ticket(
            false, false, false, None, request
        ));
    }

    #[test]
    fn context_window_failures_offer_same_task_retry_with_compacted_context() {
        let actions = code_studio_suggested_actions(
            &akasha_store::TaskStatus::Failed,
            Some("context_window_exceeded: protected instructions do not fit"),
            None,
            None,
        );
        assert_eq!(actions[0]["ui_action"], "retry_compacted_context");
        assert_eq!(actions[0]["label"], "Reprendre avec un contexte allégé");
        assert!(actions[1]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("dépasse sa fenêtre"));

        let generic = code_studio_suggested_actions(
            &akasha_store::TaskStatus::Failed,
            Some("provider unavailable"),
            None,
            None,
        );
        assert!(generic.iter().all(|action| action["ui_action"] != "retry_compacted_context"));
    }

    #[test]
    fn local_context_preflight_offers_a_workaround_instead_of_a_ineffective_retry() {
        let actions = code_studio_suggested_actions(
            &akasha_store::TaskStatus::Failed,
            Some("context_window_exceeded: context_window_preflight_rejected: no request sent"),
            None,
            None,
        );
        assert_eq!(actions[0]["ui_action"], "focus_model_selector");
        assert!(actions.iter().all(|action| action["ui_action"] != "retry_compacted_context"));
        let solution = actions[1]["message"].as_str().unwrap_or_default();
        assert!(solution.contains("avant de contacter le fournisseur"));
        assert!(solution.contains("AKASHA_OLLAMA_NUM_CTX"));
        assert!(solution.contains("Aucun appel au modèle ni outil"));
        assert!(solution.contains("éviter de soumettre la même requête"));
    }

    #[test]
    fn recognized_queue_delivery_cannot_fall_back_to_a_new_task() {
        use crate::steering_queue::QueueMode;
        assert!(queue_delivery_requires_running_target(Some(QueueMode::Steering), None));
        assert!(queue_delivery_requires_running_target(Some(QueueMode::FollowUp), None));
        assert!(!queue_delivery_requires_running_target(
            Some(QueueMode::Steering),
            Some(Uuid::nil())
        ));
        assert!(!queue_delivery_requires_running_target(None, None));
    }

    #[test]
    fn task_label_unwraps_private_instructions_before_truncation() {
        let id = Uuid::nil();
        let long_instruction = "Internal instruction. ".repeat(30);
        let message = composer_message("ask", &long_instruction, "Comparer Vec et HashMap\n  en français");
        assert_eq!(task_label(Some(&message), &id), "Comparer Vec et HashMap en français");
        let attachment = "[Pièce(s) jointe(s) à ce message : aide]\n\nMessage : Résumer ce document\n\n--- Document(s) joint(s) ---\nsecret attachment body".to_string();
        let attachment = composer_message("ask", "Read only.\n\nInstructions privées et caractères accentués.", &attachment);
        assert_eq!(task_label(Some(&attachment), &id), "Résumer ce document");
        let prompt = "Premier paragraphe privé.\n\nSecond paragraphe privé : été 🦀.\n\nDernière consigne.";
        let message = composer_message("custom", prompt, "Ma vraie demande\n\nAvec un second paragraphe");
        assert_eq!(task_label(Some(&message), &id), "Ma vraie demande Avec un second paragraphe");
        for legacy in [
            "[Composer mode: custom]\nPrivate first paragraph\n\nPrivate second paragraph\n\nUser request",
            "[Composer mode: custom; prompt_bytes: 1]\né\n\nUser request",
            "[Composer mode: custom; prompt_bytes: 999999]\nprivate",
        ] {
            assert_eq!(task_label(Some(&legacy.to_string()), &id), "Tâche");
        }
        let child = "[Task]\nObjective: [Orchestrated — disk deliverables REQUIRED]\nInternal tools requirement.\n\nCréer le rapport\nContext: private shared plan\nSuccess criteria: secret".to_string();
        assert_eq!(task_label(Some(&child), &id), "Créer le rapport");
    }

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

    #[test]
    fn collect_task_tree_ids_includes_nested_children() {
        let db = NamedTempFile::new().expect("temp");
        let store = TaskStore::open(db.path()).expect("open");
        let now = Utc::now();
        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        let grand = Uuid::new_v4();
        for (id, parent) in [(root, None), (child, Some(root)), (grand, Some(child))] {
            store
                .insert(&Task {
                    id,
                    parent_task_id: parent,
                    status: TaskStatus::Running,
                    assigned_agent: "conversation".into(),
                    created_at: now,
                    updated_at: now,
                    initial_message: Some(format!("t-{id}")),
                    studio_project_id: None,
                })
                .expect("insert");
        }
        let ids = collect_task_tree_ids(&store, root);
        assert_eq!(ids.len(), 3);
        assert!(ids.contains(&root));
        assert!(ids.contains(&child));
        assert!(ids.contains(&grand));
    }
}

#[cfg(test)]
mod tree_wait_cleanup_tests {
    use super::*;
    #[tokio::test]
    async fn tree_cancel_transitions_waiting_members_and_only_releases_confirmed_cancelled_waits() {
        let dir=tempfile::tempdir().unwrap();let db=dir.path().join("tasks.db");let store=TaskStore::open(&db).unwrap();let root=Uuid::new_v4();let waiting=Uuid::new_v4();let paused=Uuid::new_v4();let completed=Uuid::new_v4();let unrelated=Uuid::new_v4();let now=chrono::Utc::now();
        let waits=crate::api::new_human_input_store();let mut receivers=std::collections::HashMap::new();
        for (id,parent,status) in [(root,None,TaskStatus::Running),(waiting,Some(root),TaskStatus::WaitingUserInput),(paused,Some(root),TaskStatus::Paused),(completed,Some(root),TaskStatus::Completed),(unrelated,None,TaskStatus::WaitingUserInput)] {
            store.insert(&akasha_store::Task{id,parent_task_id:parent,status,assigned_agent:"studio_fullstack".into(),created_at:now,updated_at:now,initial_message:None,studio_project_id:None}).unwrap();
            let (tx,rx)=tokio::sync::oneshot::channel();receivers.insert(id,rx);waits.write().await.insert(id,crate::api::PendingHumanInput{request_id:Some(id.to_string()),question:"Continue?".into(),context:String::new(),choices:None,response_tx:tx});
        }
        let ids=collect_task_tree_ids(&store,root);let cancelled=ids.into_iter().filter(|id|cancel_tree_member(&store,*id).unwrap()).collect::<Vec<_>>();assert_eq!(cancelled.len(),3);assert_eq!(store.get(waiting).unwrap().unwrap().status,TaskStatus::Cancelled);
        let response=json_response("200 OK",&serde_json::json!({"cancelled_ids":cancelled,"skipped":[{"task_id":completed}]}).to_string());
        clear_cancelled_tree_waits(&db,&response,&waits).await;
        let guard=waits.read().await;assert!(guard.contains_key(&completed));assert!(guard.contains_key(&unrelated));assert!(!guard.contains_key(&waiting));drop(guard);
        assert!(matches!(receivers.get_mut(&waiting).unwrap().try_recv(),Err(tokio::sync::oneshot::error::TryRecvError::Closed)));
        assert!(matches!(receivers.get_mut(&unrelated).unwrap().try_recv(),Err(tokio::sync::oneshot::error::TryRecvError::Empty)));
    }
}
