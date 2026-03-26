use akasha_core::{EventEnvelope, EventType};
use akasha_store::TaskStore;
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use uuid::Uuid;

static EMITTED_MILESTONES: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn milestone_registry() -> &'static Mutex<HashSet<String>> {
    EMITTED_MILESTONES.get_or_init(|| Mutex::new(HashSet::new()))
}

pub fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(default)
}

pub fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(default)
}

pub fn env_duration_ms(key: &str, default_ms: u64) -> std::time::Duration {
    std::time::Duration::from_millis(env_u64(key, default_ms))
}

pub fn resolve_root_task_id(store_path: &Path, task_id: Uuid) -> Option<Uuid> {
    let store = TaskStore::open(store_path).ok()?;
    let mut current = task_id;
    for _ in 0..16 {
        let task = store.get(current).ok().flatten()?;
        if let Some(parent_id) = task.parent_task_id {
            current = parent_id;
        } else {
            return Some(current);
        }
    }
    Some(current)
}

pub fn task_age_ms(store_path: &Path, task_id: Uuid) -> Option<u64> {
    let store = TaskStore::open(store_path).ok()?;
    let task = store.get(task_id).ok().flatten()?;
    let delta = chrono::Utc::now().signed_duration_since(task.created_at);
    Some(delta.num_milliseconds().max(0) as u64)
}

fn mark_once(task_id: Uuid, name: &str) -> bool {
    let key = format!("{}:{}", task_id, name);
    let mut guard = match milestone_registry().lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.insert(key)
}

/// Remove all milestone deduplication entries for the given root task id.
/// Call this after a task has fully completed or failed to prevent unbounded
/// growth of the global milestone registry.
pub fn clear_task_milestones(task_id: Uuid, store_path: Option<&Path>) {
    let root_id = if let Some(p) = store_path {
        match resolve_root_task_id(p, task_id) {
            Some(id) => id,
            None => {
                tracing::warn!(%task_id, "clear_task_milestones: could not resolve root task id, using task_id directly");
                task_id
            }
        }
    } else {
        task_id
    };
    let prefix = format!("{}:", root_id);
    let mut guard = match milestone_registry().lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.retain(|key| !key.starts_with(&prefix));
}

pub fn emit_timeline(
    bus: &crate::agents::EventBus,
    correlation_id: Uuid,
    name: &str,
    elapsed_ms: Option<u64>,
    extra: Option<Value>,
) {
    let mut payload = Map::new();
    payload.insert("name".to_string(), Value::String(name.to_string()));
    if let Some(ms) = elapsed_ms {
        payload.insert("elapsed_ms".to_string(), serde_json::json!(ms));
    }
    if let Some(extra) = extra {
        match extra {
            Value::Object(map) => {
                for (k, v) in map {
                    payload.insert(k, v);
                }
            }
            other => {
                payload.insert("details".to_string(), other);
            }
        }
    }
    let _ = bus.send(
        EventEnvelope::new(EventType::TimelineMilestone, Some(Value::Object(payload)))
            .with_correlation(correlation_id),
    );
}

pub fn emit_timeline_for_task(
    bus: &crate::agents::EventBus,
    store_path: Option<&Path>,
    task_id: Uuid,
    name: &str,
    extra: Option<Value>,
) {
    let correlation_id = store_path
        .and_then(|p| resolve_root_task_id(p, task_id))
        .unwrap_or(task_id);
    let elapsed_ms = store_path.and_then(|p| task_age_ms(p, correlation_id));
    emit_timeline(bus, correlation_id, name, elapsed_ms, extra);
}

pub fn emit_timeline_once_for_task(
    bus: &crate::agents::EventBus,
    store_path: Option<&Path>,
    task_id: Uuid,
    name: &str,
    extra: Option<Value>,
) -> bool {
    let correlation_id = store_path
        .and_then(|p| resolve_root_task_id(p, task_id))
        .unwrap_or(task_id);
    if !mark_once(correlation_id, name) {
        return false;
    }
    let elapsed_ms = store_path.and_then(|p| task_age_ms(p, correlation_id));
    emit_timeline(bus, correlation_id, name, elapsed_ms, extra);
    true
}

pub fn log_latency_metric(store_path: &Path, task_id: Uuid, metric_name: &str) {
    if let Some(root_task_id) = resolve_root_task_id(store_path, task_id) {
        if let Some(elapsed_ms) = task_age_ms(store_path, root_task_id) {
            tracing::info!(
                task_id = %root_task_id,
                metric = metric_name,
                elapsed_ms,
                "orchestration latency metric"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use akasha_store::{Task, TaskStatus, TaskStore};
    use tempfile::NamedTempFile;

    #[test]
    fn resolve_root_task_id_follows_parent_chain() {
        let db = NamedTempFile::new().expect("temp db");
        let store = TaskStore::open(db.path()).expect("open store");
        let now = chrono::Utc::now();
        let root_id = Uuid::new_v4();
        let child_id = Uuid::new_v4();
        store
            .insert(&Task {
                id: root_id,
                parent_task_id: None,
                status: TaskStatus::Pending,
                assigned_agent: "conversation".to_string(),
                created_at: now,
                updated_at: now,
                initial_message: None,
            })
            .expect("insert root");
        store
            .insert(&Task {
                id: child_id,
                parent_task_id: Some(root_id),
                status: TaskStatus::Pending,
                assigned_agent: "conversation".to_string(),
                created_at: now,
                updated_at: now,
                initial_message: None,
            })
            .expect("insert child");

        assert_eq!(resolve_root_task_id(db.path(), child_id), Some(root_id));
    }

    #[test]
    fn milestone_once_deduplicates_per_root_and_name() {
        let root = Uuid::new_v4();
        assert!(mark_once(root, "first_meaningful_progress"));
        assert!(!mark_once(root, "first_meaningful_progress"));
        assert!(mark_once(root, "task_completed"));
    }
}
