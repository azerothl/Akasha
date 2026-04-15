//! Subscribes to event bus and fills progress cache and events cache for API

use akasha_core::EventType;
use chrono::Utc;
use std::collections::VecDeque;
use std::sync::mpsc;
use uuid::Uuid;

use super::EventBus;
use crate::api::{
    EventsCache, ProgressCache, ProgressEntry, TaskEventEntry, MAX_EVENTS_PER_TASK, MAX_PROGRESS_PER_TASK,
};

/// Messages queued for a single TaskStore writer thread (avoids concurrent SQLite writes / "database is locked").
#[derive(Debug)]
pub enum TaskPersistenceMsg {
    Progress {
        task_id: Uuid,
        progress_pct: u8,
        message: String,
    },
    Event {
        task_id: Uuid,
        event_type: String,
        payload: Option<serde_json::Value>,
        at: String,
    },
}

/// Optional sender to persist progress and events to DB (same queue → serialized writes).
pub type TaskPersistenceTx = Option<mpsc::Sender<TaskPersistenceMsg>>;

pub async fn run_progress_subscriber(bus: EventBus, progress: ProgressCache, persistence_tx: TaskPersistenceTx) {
    let mut rx = bus.subscribe();
    while let Ok(ev) = rx.recv().await {
        let payload = match &ev.payload {
            Some(p) => p,
            None => continue,
        };
        if ev.event_type == EventType::ProgressUpdate {
            let task_id = payload.get("task_id").and_then(|v| v.as_str()).and_then(|s| Uuid::parse_str(s).ok());
            let progress_pct = payload.get("progress_pct").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
            let message = payload.get("message").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let Some(task_id) = task_id else { continue };
            if let Some(ref tx) = persistence_tx {
                let _ = tx.send(TaskPersistenceMsg::Progress {
                    task_id,
                    progress_pct,
                    message: message.clone(),
                });
            }
            let entry = ProgressEntry { progress_pct, message };
            let mut g = progress.write().await;
            let q = g.entry(task_id).or_insert_with(VecDeque::new);
            q.push_back(entry);
            if q.len() > MAX_PROGRESS_PER_TASK {
                q.pop_front();
            }
            continue;
        }
        // When a task completes, fails or is cancelled, add a final progress entry with 100%
        // so the UI shows a clear completion line. For TaskCompleted, keep the sub-agent's reply
        // when present (last progress message) so the chat interface shows the actual response.
        if ev.event_type == EventType::TaskCompleted
            || ev.event_type == EventType::TaskFailed
            || ev.event_type == EventType::TaskCancelled
        {
            let task_id = payload.get("task_id").and_then(|v| v.as_str()).and_then(|s| Uuid::parse_str(s).ok());
            let Some(task_id) = task_id else { continue };
            let generic = [
                "Terminé.",
                "Done.",
                "Échec.",
                "Annulé.",
                "Failed.",
                "Cancelled.",
            ];
            let message: String = if ev.event_type == EventType::TaskCompleted {
                let mut g = progress.write().await;
                let q = g.entry(task_id).or_insert_with(VecDeque::new);
                let last_msg = q.back().map(|e| e.message.trim()).unwrap_or("");
                let chosen = if !last_msg.is_empty() && !generic.contains(&last_msg) {
                    last_msg.to_string()
                } else {
                    "Terminé.".to_string()
                };
                chosen
            } else if ev.event_type == EventType::TaskFailed {
                // Do not wipe the last substantive ProgressUpdate (e.g. studio verify stderr, LLM error text).
                let q_snapshot = {
                    let g = progress.read().await;
                    g.get(&task_id).cloned().unwrap_or_default()
                };
                let last_substantive = q_snapshot.iter().rev().find_map(|e| {
                    let t = e.message.trim();
                    if t.is_empty() || generic.contains(&t) {
                        None
                    } else {
                        Some(t.to_string())
                    }
                });
                let payload_reason = payload
                    .get("reason")
                    .or_else(|| payload.get("detail"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                if let Some(s) = last_substantive {
                    s
                } else if !payload_reason.is_empty() {
                    format!("Échec — {}", payload_reason)
                } else {
                    "Échec.".to_string()
                }
            } else {
                let q_snapshot = {
                    let g = progress.read().await;
                    g.get(&task_id).cloned().unwrap_or_default()
                };
                let last_substantive = q_snapshot.iter().rev().find_map(|e| {
                    let t = e.message.trim();
                    if t.is_empty() || generic.contains(&t) {
                        None
                    } else {
                        Some(t.to_string())
                    }
                });
                if let Some(s) = last_substantive {
                    s
                } else {
                    "Annulé.".to_string()
                }
            };
            if let Some(ref tx) = persistence_tx {
                let _ = tx.send(TaskPersistenceMsg::Progress {
                    task_id,
                    progress_pct: 100,
                    message: message.clone(),
                });
            }
            let mut g = progress.write().await;
            let q = g.entry(task_id).or_insert_with(VecDeque::new);
            // Replace last entry if it was already 100% with same-ish content to avoid duplicate; else push.
            let replace_last = q.back().map(|e| e.progress_pct == 100).unwrap_or(false);
            if replace_last {
                q.pop_back();
            }
            q.push_back(ProgressEntry {
                progress_pct: 100,
                message,
            });
            while q.len() > MAX_PROGRESS_PER_TASK {
                q.pop_front();
            }
        }
    }
}

/// Subscribes to event bus and fills events cache (all events with correlation_id) for GET /api/tasks/:id/events.
/// Resilient to Lagged: continues processing instead of exiting so root task events are never lost.
pub async fn run_events_subscriber(bus: EventBus, events: EventsCache, persistence_tx: TaskPersistenceTx) {
    let mut rx = bus.subscribe();
    loop {
        match rx.recv().await {
            Ok(ev) => {
                let Some(task_id) = ev.correlation_id else { continue };
                let at = Utc::now().to_rfc3339();
                let entry = TaskEventEntry {
                    event_type: ev.event_type.as_str().to_string(),
                    payload: ev.payload.clone(),
                    at: at.clone(),
                    task_id: Some(task_id.to_string()),
                };
                if let Some(ref tx) = persistence_tx {
                    let _ = tx.send(TaskPersistenceMsg::Event {
                        task_id,
                        event_type: entry.event_type.clone(),
                        payload: entry.payload.clone(),
                        at,
                    });
                }
                let mut g = events.write().await;
                let q = g.entry(task_id).or_insert_with(VecDeque::new);
                q.push_back(entry);
                if q.len() > MAX_EVENTS_PER_TASK {
                    q.pop_front();
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                // Missed some events due to slow consumer; continue to process future events
                continue;
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                // Sender dropped, channel is done
                break;
            }
        }
    }
}
