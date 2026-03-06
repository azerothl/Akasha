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

/// Optional sender to persist progress to DB (daemon passes this for fast GET /api/tasks/:id).
pub type ProgressPersistenceTx = Option<mpsc::Sender<(Uuid, u8, String)>>;

pub async fn run_progress_subscriber(bus: EventBus, progress: ProgressCache, persistence_tx: ProgressPersistenceTx) {
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
                let _ = tx.send((task_id, progress_pct, message.clone()));
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
        // When a task completes, fails or is cancelled, always add a final progress entry with 100%
        // so the UI shows a clear "Terminé" / "Échec" / "Annulé" line.
        if ev.event_type == EventType::TaskCompleted
            || ev.event_type == EventType::TaskFailed
            || ev.event_type == EventType::TaskCancelled
        {
            let task_id = payload.get("task_id").and_then(|v| v.as_str()).and_then(|s| Uuid::parse_str(s).ok());
            let Some(task_id) = task_id else { continue };
            let message = if ev.event_type == EventType::TaskCompleted {
                "Terminé."
            } else if ev.event_type == EventType::TaskFailed {
                "Échec."
            } else {
                "Annulé."
            };
            if let Some(ref tx) = persistence_tx {
                let _ = tx.send((task_id, 100, message.to_string()));
            }
            let mut g = progress.write().await;
            let q = g.entry(task_id).or_insert_with(VecDeque::new);
            q.push_back(ProgressEntry {
                progress_pct: 100,
                message: message.to_string(),
            });
            while q.len() > MAX_PROGRESS_PER_TASK {
                q.pop_front();
            }
        }
    }
}

/// Subscribes to event bus and fills events cache (all events with correlation_id) for GET /api/tasks/:id/events.
pub async fn run_events_subscriber(bus: EventBus, events: EventsCache) {
    let mut rx = bus.subscribe();
    while let Ok(ev) = rx.recv().await {
        let Some(task_id) = ev.correlation_id else { continue };
        let entry = TaskEventEntry {
            event_type: ev.event_type.as_str().to_string(),
            payload: ev.payload.clone(),
            at: Utc::now().to_rfc3339(),
        };
        let mut g = events.write().await;
        let q = g.entry(task_id).or_insert_with(VecDeque::new);
        q.push_back(entry);
        if q.len() > MAX_EVENTS_PER_TASK {
            q.pop_front();
        }
    }
}
