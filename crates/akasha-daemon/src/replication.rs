//! Phase 7 — Replication bridge: publish task events to NATS when leader, subscribe and apply when cluster enabled.

use akasha_core::EventType;
use akasha_cluster::{replicate_subscribe, publish_task_event, LogEntryMsg, TaskEventMsg};
use akasha_store::{Task, TaskStatus, TaskStore};
use std::path::Path;
use tracing::{info, warn};

/// Subscribes to the event bus and publishes task-related events to NATS (leader only).
pub async fn run_replication_publisher(
    bus: crate::agents::EventBus,
    nats_client: async_nats::Client,
    store_path: &Path,
) {
    let mut rx = bus.subscribe();
    let store_path = store_path.to_path_buf();
    while let Ok(ev) = rx.recv().await {
        let task_events = [
            EventType::TaskCreated,
            EventType::TaskCompleted,
            EventType::TaskFailed,
            EventType::TaskCancelled,
            EventType::TaskDecomposed,
            EventType::SubAgentSpawned,
        ];
        if !task_events.contains(&ev.event_type) {
            continue;
        }
        let Some(ref payload) = ev.payload else { continue };
        let task_id = ev
            .correlation_id
            .or_else(|| payload.get("task_id").and_then(|v| v.as_str()).and_then(|s| uuid::Uuid::parse_str(s).ok()))
            .or_else(|| payload.get("task_id").and_then(|v| v.as_str()).and_then(|s| uuid::Uuid::parse_str(s).ok()));
        let Some(task_id) = task_id else { continue };
        let event_name = ev.event_type.as_str();
        let payload_value = payload.clone();
        if ev.event_type == EventType::TaskCreated {
            if let Ok(store) = TaskStore::open(&store_path) {
                if let Ok(Some(task)) = store.get(task_id) {
                    let task_payload = serde_json::json!({
                        "id": task.id.to_string(),
                        "status": task.status.as_str(),
                        "parent_task_id": task.parent_task_id.map(|u| u.to_string()),
                        "assigned_agent": task.assigned_agent,
                        "created_at": task.created_at.to_rfc3339(),
                        "updated_at": task.updated_at.to_rfc3339(),
                        "initial_message": task.initial_message
                    });
                    if let Err(e) =
                        publish_task_event(&nats_client, event_name, &task_id.to_string(), Some(task_payload)).await
                    {
                        warn!(error = %e, task_id = %task_id, "Replication: publish task_created failed");
                    }
                }
            }
        } else {
            if let Err(e) =
                publish_task_event(&nats_client, event_name, &task_id.to_string(), Some(payload_value)).await
            {
                warn!(error = %e, task_id = %task_id, "Replication: publish task event failed");
            }
        }
    }
}

/// Spawns the replication subscriber (apply log + task events from NATS to local store).
pub fn spawn_replication_subscriber(nats_client: async_nats::Client, store_path: std::path::PathBuf) {
    let on_log = move |entry: LogEntryMsg| {
        tracing::debug!(seq = entry.seq, "Replication: received log entry (apply to local log if needed)");
    };
    let on_task = move |msg: TaskEventMsg| {
        let store_path = store_path.clone();
        if let Ok(store) = TaskStore::open(&store_path) {
            let task_id = match uuid::Uuid::parse_str(&msg.task_id) {
                Ok(id) => id,
                Err(_) => return,
            };
            match msg.event.as_str() {
                "task_created" => {
                    if let Some(ref p) = msg.payload {
                        let id = match p.get("id").and_then(|v| v.as_str()).and_then(|s| uuid::Uuid::parse_str(s).ok()) {
                            Some(id) => id,
                            None => return,
                        };
                        let status_str = p.get("status").and_then(|v| v.as_str()).unwrap_or("pending");
                        let status = match status_str {
                            "queued" => TaskStatus::Queued,
                            "running" => TaskStatus::Running,
                            "completed" => TaskStatus::Completed,
                            "failed" => TaskStatus::Failed,
                            "paused" => TaskStatus::Paused,
                            "cancelled" => TaskStatus::Cancelled,
                            "waiting_user_input" => TaskStatus::WaitingUserInput,
                            _ => TaskStatus::Pending,
                        };
                        let created_at = p
                            .get("created_at")
                            .and_then(|v| v.as_str())
                            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                            .map(|dt| dt.with_timezone(&chrono::Utc))
                            .unwrap_or_else(chrono::Utc::now);
                        let updated_at = p
                            .get("updated_at")
                            .and_then(|v| v.as_str())
                            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                            .map(|dt| dt.with_timezone(&chrono::Utc))
                            .unwrap_or_else(chrono::Utc::now);
                        let task = Task {
                            id,
                            parent_task_id: p.get("parent_task_id").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).and_then(|s| uuid::Uuid::parse_str(s).ok()),
                            status,
                            assigned_agent: p.get("assigned_agent").and_then(|v| v.as_str()).unwrap_or("conversation").to_string(),
                            created_at,
                            updated_at,
                            initial_message: p.get("initial_message").and_then(|v| v.as_str()).map(String::from),
                        };
                        let _ = store.insert(&task);
                    }
                }
                "task_completed" | "task_failed" | "task_cancelled" => {
                    let status = match msg.event.as_str() {
                        "task_completed" => TaskStatus::Completed,
                        "task_failed" => TaskStatus::Failed,
                        "task_cancelled" => TaskStatus::Cancelled,
                        _ => return,
                    };
                    let _ = store.update_status(task_id, status);
                }
                _ => {}
            }
        }
    };
    tokio::spawn(async move {
        replicate_subscribe(nats_client, on_log, on_task).await;
    });
    info!("Replication subscriber started");
}
