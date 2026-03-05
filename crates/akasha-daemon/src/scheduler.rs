//! Scheduler service: tick, create task_runs with dedup, push to orchestrator (spec 37_scheduler_design)

use akasha_core::{EventEnvelope, EventType};
use akasha_store::{
    Schedule, ScheduleStore, Task, TaskRun, TaskRunStatus, TaskStore, TaskStatus,
};
use chrono::{Duration, Utc};
use std::path::PathBuf;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::agents::OrchestratorTask;

const TICK_INTERVAL_SECS: u64 = 30;

/// Run the scheduler loop: every TICK_INTERVAL_SECS, evaluate enabled schedules,
/// create task_runs with dedup_key, create tasks and push to orchestrator.
pub async fn run_scheduler(
    store_path: PathBuf,
    orch_tx: mpsc::Sender<OrchestratorTask>,
    bus: crate::agents::EventBus,
) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(TICK_INTERVAL_SECS));
    interval.tick().await;
    loop {
        interval.tick().await;
        if let Err(e) = tick(&store_path, &orch_tx, &bus).await {
            tracing::warn!(error = %e, "Scheduler tick failed");
        }
    }
}

async fn tick(
    store_path: &PathBuf,
    orch_tx: &mpsc::Sender<OrchestratorTask>,
    bus: &crate::agents::EventBus,
) -> anyhow::Result<()> {
    let schedule_store = ScheduleStore::open(store_path)?;
    let task_store = TaskStore::open(store_path)?;
    let now = Utc::now();

    let _ = bus.send(
        EventEnvelope::new(EventType::SchedulerTick, Some(serde_json::json!({ "at": now.to_rfc3339() }))),
    );

    let enabled = schedule_store.list_enabled_schedules()?;
    for schedule in enabled {
        let Some(interval_secs) = schedule.interval_seconds else {
            continue;
        };
        let next_planned = next_planned_for(&schedule_store, &schedule, now, interval_secs)?;
        let Some(planned_for) = next_planned else {
            continue;
        };
        if planned_for > now {
            continue;
        }
        let dedup_key = format!("{}:{}", schedule.id, planned_for.timestamp());
        if schedule_store.dedup_key_exists(&dedup_key)? {
            continue;
        }
        let task_id = Uuid::new_v4();
        let run_id = Uuid::new_v4();
        let task = Task {
            id: task_id,
            parent_task_id: None,
            status: TaskStatus::Pending,
            assigned_agent: "conversation".to_string(),
            created_at: now,
            updated_at: now,
        };
        task_store.insert(&task)?;
        let task_run = TaskRun {
            id: run_id,
            schedule_id: Some(schedule.id),
            task_id,
            status: TaskRunStatus::Queued,
            planned_for,
            started_at: None,
            ended_at: None,
            dedup_key: dedup_key.clone(),
        };
        schedule_store.insert_task_run(&task_run)?;
        let _ = bus.send(
            EventEnvelope::new(
                EventType::TaskRunCreated,
                Some(serde_json::json!({
                    "task_run_id": run_id.to_string(),
                    "schedule_id": schedule.id.to_string(),
                    "task_id": task_id.to_string(),
                    "planned_for": planned_for.to_rfc3339(),
                    "dedup_key": dedup_key
                })),
            )
            .with_correlation(task_id),
        );
        let message = schedule
            .channel_context
            .as_deref()
            .unwrap_or("Exécution planifiée.")
            .to_string();
        let session_id = format!("schedule:{}", schedule.id);
        if orch_tx.send((task_id, message, session_id)).await.is_err() {
            tracing::warn!(task_id = %task_id, "Scheduler: orchestrator channel closed");
        }
        schedule_store.update_task_run_status(
            run_id,
            TaskRunStatus::Running,
            Some(now),
            None,
        )?;
    }
    Ok(())
}

/// Returns the next due slot (last slot <= now) for which we might create a run.
fn next_planned_for(
    schedule_store: &ScheduleStore,
    schedule: &Schedule,
    now: chrono::DateTime<Utc>,
    interval_secs: u64,
) -> anyhow::Result<Option<chrono::DateTime<Utc>>> {
    let runs = schedule_store.list_task_runs(Some(schedule.id), 1)?;
    let base = runs.into_iter().next().map(|r| r.planned_for).unwrap_or(schedule.start_at);
    if let Some(end_at) = schedule.end_at {
        if end_at < now {
            return Ok(None);
        }
    }
    if base > now {
        return Ok(None);
    }
    let delta = Duration::seconds(interval_secs as i64);
    let mut candidate = base;
    while candidate + delta <= now {
        candidate = candidate + delta;
    }
    Ok(Some(candidate))
}
