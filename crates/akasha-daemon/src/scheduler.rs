//! Scheduler service: tick, create task_runs with dedup, push to orchestrator (spec 37_scheduler_design)

use akasha_core::{EventEnvelope, EventType};
use akasha_store::{Schedule, ScheduleStore, Task, TaskRun, TaskRunStatus, TaskStatus, TaskStore};
use chrono::{Duration, Utc};
use std::path::PathBuf;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::agents::OrchestratorTask;

const TICK_INTERVAL_SECS: u64 = 30;
/// Maximum number of missed occurrences to catch up per schedule per tick.
const MAX_CATCHUP: usize = 10;

/// Run the scheduler loop: every TICK_INTERVAL_SECS, evaluate enabled schedules,
/// create task_runs with dedup_key, create tasks and push to orchestrator.
pub async fn run_scheduler(
    store_path: PathBuf,
    orch_tx: mpsc::Sender<OrchestratorTask>,
    bus: crate::agents::EventBus,
) {
    let schedule_store = match ScheduleStore::open(&store_path) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "Scheduler failed to open ScheduleStore");
            return;
        }
    };
    let task_store = match TaskStore::open(&store_path) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "Scheduler failed to open TaskStore");
            return;
        }
    };

    let mut interval = tokio::time::interval(std::time::Duration::from_secs(TICK_INTERVAL_SECS));
    interval.tick().await;
    loop {
        interval.tick().await;
        if let Err(e) = tick(&schedule_store, &task_store, &orch_tx, &bus).await {
            tracing::warn!(error = %e, "Scheduler tick failed");
        }
    }
}

async fn tick(
    schedule_store: &ScheduleStore,
    task_store: &TaskStore,
    orch_tx: &mpsc::Sender<OrchestratorTask>,
    bus: &crate::agents::EventBus,
) -> anyhow::Result<()> {
    let now = Utc::now();

    sync_terminal_task_run_statuses(&schedule_store, &task_store, now)?;

    let _ = bus.send(EventEnvelope::new(
        EventType::SchedulerTick,
        Some(serde_json::json!({ "at": now.to_rfc3339() })),
    ));

    let enabled = schedule_store.list_enabled_schedules()?;
    for schedule in enabled {
        let Some(interval_secs) = schedule.interval_seconds else {
            continue;
        };
        let slots = due_slots(schedule_store, &schedule, now, interval_secs)?;
        for planned_for in slots.into_iter().take(MAX_CATCHUP) {
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
            match orch_tx.send((task_id, message, session_id)).await {
                Ok(()) => {
                    schedule_store.update_task_run_status(
                        run_id,
                        TaskRunStatus::Running,
                        Some(now),
                        None,
                    )?;
                }
                Err(_) => {
                    tracing::warn!(task_id = %task_id, "Scheduler: orchestrator channel closed");
                }
            }
        }
    }
    Ok(())
}

fn sync_terminal_task_run_statuses(
    schedule_store: &ScheduleStore,
    task_store: &TaskStore,
    now: chrono::DateTime<Utc>,
) -> anyhow::Result<()> {
    let task_runs = schedule_store.list_task_runs(None, 1_000)?;
    for run in task_runs {
        if run.status != TaskRunStatus::Running {
            continue;
        }
        let Some(task) = task_store.get(run.task_id)? else {
            continue;
        };
        let terminal_status = match task.status {
            TaskStatus::Completed => Some(TaskRunStatus::Completed),
            TaskStatus::Failed => Some(TaskRunStatus::Failed),
            TaskStatus::Cancelled => Some(TaskRunStatus::Cancelled),
            _ => None,
        };
        if let Some(status) = terminal_status {
            schedule_store.update_task_run_status(run.id, status, None, Some(now))?;
        }
    }
    Ok(())
}

/// Returns all due slots (<= now) for which no run has been created yet, starting from the
/// slot after the last known run. Bounded implicitly by MAX_CATCHUP at the call site.
fn due_slots(
    schedule_store: &ScheduleStore,
    schedule: &Schedule,
    now: chrono::DateTime<Utc>,
    interval_secs: u64,
) -> anyhow::Result<Vec<chrono::DateTime<Utc>>> {
    if let Some(end_at) = schedule.end_at {
        if end_at < now {
            return Ok(vec![]);
        }
    }
    let runs = schedule_store.list_task_runs(Some(schedule.id), 1)?;
    let last_run = runs.into_iter().next();
    let base = last_run
        .as_ref()
        .map(|r| r.planned_for)
        .unwrap_or(schedule.start_at);
    if base > now {
        return Ok(vec![]);
    }
    let delta = Duration::seconds(interval_secs as i64);
    // If there are existing runs, start from the slot after the last known run.
    // If there are no runs yet, the first candidate is start_at itself (the schedule's first
    // occurrence is at start_at, not start_at + delta).
    let first_candidate = if last_run.is_some() { base + delta } else { base };
    let mut slots = Vec::new();
    let mut candidate = first_candidate;
    // Include slots up to and including now: a slot at exactly `now` is considered due.
    while candidate <= now {
        slots.push(candidate);
        candidate = candidate + delta;
    }
    Ok(slots)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn sync_terminal_task_run_statuses_marks_completed_runs() {
        let db = NamedTempFile::new().expect("temp db");
        let schedule_store = ScheduleStore::open(db.path()).expect("open schedule store");
        let task_store = TaskStore::open(db.path()).expect("open task store");
        let now = Utc::now();

        let task_id = Uuid::new_v4();
        task_store
            .insert(&Task {
                id: task_id,
                parent_task_id: None,
                status: TaskStatus::Completed,
                assigned_agent: "conversation".to_string(),
                created_at: now,
                updated_at: now,
            })
            .expect("insert task");

        let run_id = Uuid::new_v4();
        schedule_store
            .insert_task_run(&TaskRun {
                id: run_id,
                schedule_id: Some(Uuid::new_v4()),
                task_id,
                status: TaskRunStatus::Running,
                planned_for: now,
                started_at: Some(now),
                ended_at: None,
                dedup_key: "dedup".to_string(),
            })
            .expect("insert task run");

        sync_terminal_task_run_statuses(&schedule_store, &task_store, now).expect("sync task runs");

        let updated_run = schedule_store
            .get_task_run(run_id)
            .expect("get task run")
            .expect("task run exists");
        assert_eq!(updated_run.status, TaskRunStatus::Completed);
        assert_eq!(updated_run.ended_at, Some(now));
    }
}
