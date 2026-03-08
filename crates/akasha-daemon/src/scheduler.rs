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

/// Pending item: (run_id, task_id, message, session_id) — sent to orchestrator then run marked Running.
type PendingRun = (Uuid, Uuid, String, String);

/// Run the scheduler loop: every TICK_INTERVAL_SECS, evaluate enabled schedules,
/// create task_runs with dedup_key, create tasks and push to orchestrator.
/// Stores are opened per tick and never held across await so the future stays Send.
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
    let now = Utc::now();

    // Do all DB work and collect pending (run_id, task_id, message, session_id). No await here
    // so we never hold ScheduleStore/TaskStore (non-Send) across an await.
    let pending: Vec<PendingRun> = {
        let schedule_store = ScheduleStore::open(store_path)?;
        let task_store = TaskStore::open(store_path)?;

        sync_terminal_task_run_statuses(&schedule_store, &task_store, now)?;

        let _ = bus.send(EventEnvelope::new(
            EventType::SchedulerTick,
            Some(serde_json::json!({ "at": now.to_rfc3339() })),
        ));

        let enabled = schedule_store.list_enabled_schedules()?;
        let mut pending = Vec::new();
        for schedule in enabled {
            let Some(interval_secs) = schedule.interval_seconds else {
                continue;
            };
            let slots = due_slots(&schedule_store, &schedule, now, interval_secs)?;
            for planned_for in slots.into_iter().take(MAX_CATCHUP) {
                let dedup_key = format!("{}:{}", schedule.id, planned_for.timestamp());
                if schedule_store.dedup_key_exists(&dedup_key)? {
                    continue;
                }
                let task_id = Uuid::new_v4();
                let run_id = Uuid::new_v4();
                let initial_message = schedule
                    .channel_context
                    .as_deref()
                    .map(String::from)
                    .or_else(|| Some(schedule.name.clone()))
                    .filter(|s| !s.is_empty());
                let task = Task {
                    id: task_id,
                    parent_task_id: None,
                    status: TaskStatus::Pending,
                    assigned_agent: "conversation".to_string(),
                    created_at: now,
                    updated_at: now,
                    initial_message,
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
                pending.push((run_id, task_id, message, session_id));
            }
        }
        pending
    };

    // Send to orchestrator (await) — no store references held.
    // Only track runs whose send succeeded so we don't mark failed sends as Running.
    let mut successful_run_ids: Vec<uuid::Uuid> = Vec::new();
    for (run_id, task_id, message, session_id) in &pending {
        match orch_tx
            .send(crate::agents::OrchestratorTask {
                task_id: *task_id,
                message: message.clone(),
                session_id: session_id.clone(),
                image_data_urls: None,
            })
            .await
        {
            Ok(()) => {
                successful_run_ids.push(*run_id);
            }
            Err(_) => {
                tracing::warn!(task_id = %task_id, "Scheduler: orchestrator channel closed");
            }
        }
    }

    // Reopen schedule_store only to mark successfully sent runs as Running.
    if !successful_run_ids.is_empty() {
        let schedule_store = ScheduleStore::open(store_path)?;
        for run_id in successful_run_ids {
            if let Err(e) = schedule_store.update_task_run_status(
                run_id,
                TaskRunStatus::Running,
                Some(now),
                None,
            ) {
                tracing::warn!(run_id = %run_id, error = %e, "Scheduler: failed to update task_run status to Running");
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
                initial_message: None,
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
