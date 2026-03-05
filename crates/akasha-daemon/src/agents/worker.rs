//! Worker — execute one sub-task, emit progress, complete/fail

use akasha_core::{EventEnvelope, EventType};
use akasha_store::{TaskStore, TaskStatus};
use uuid::Uuid;

use super::EventBus;

pub async fn run_worker(bus: EventBus, store_path: std::path::PathBuf, task_id: Uuid) {
    let store = match TaskStore::open(&store_path) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "Worker: store open failed");
            return;
        }
    };
    if store.update_status(task_id, TaskStatus::Running).is_err() {
        return;
    }
    let _ = bus.send(
        EventEnvelope::new(
            EventType::TaskStarted,
            Some(serde_json::json!({ "task_id": task_id.to_string() })),
        )
        .with_correlation(task_id),
    );

    let correlation_id = task_id;

    for (pct, msg) in [(25, "started"), (50, "working"), (75, "almost done"), (100, "done")] {
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        let _ = bus.send(
            EventEnvelope::new(
                EventType::ProgressUpdate,
                Some(serde_json::json!({
                    "task_id": task_id.to_string(),
                    "progress_pct": pct,
                    "message": msg
                })),
            )
            .with_correlation(correlation_id),
        );
    }

    let store = match TaskStore::open(&store_path) {
        Ok(s) => s,
        Err(_) => return,
    };
    let _ = store.update_status(task_id, TaskStatus::Completed);
    let _ = bus.send(
        EventEnvelope::new(
            EventType::TaskCompleted,
            Some(serde_json::json!({
                "task_id": task_id.to_string(),
                "status": "completed"
            })),
        )
        .with_correlation(correlation_id),
    );
}
