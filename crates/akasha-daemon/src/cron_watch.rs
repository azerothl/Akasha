//! Cron / schedule watch subscriptions → agent wakeups (P6-B3).
//! When a scheduled task_run reaches a matching terminal status, enqueue a Wakeup
//! (mirrors process_watch exit/condition subscriptions for schedules).

use akasha_store::platform_extras::{Wakeup, WakeupStore};
use akasha_store::TaskRunStatus;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tokio::sync::RwLock;
use uuid::Uuid;

#[derive(Clone, Serialize)]
pub struct CronWatchEvent {
    pub ts_rfc3339: String,
    pub schedule_id: String,
    pub schedule_name: String,
    pub task_id: String,
    pub task_run_id: String,
    pub status: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CronWatchSubscription {
    pub id: String,
    pub session_id: String,
    /// Exact schedule UUID (empty = any).
    #[serde(default)]
    pub schedule_id: String,
    /// Substring match on schedule name (empty = any).
    #[serde(default)]
    pub name_contains: String,
    /// If set, only fire when terminal status equals this (`completed`/`failed`/`cancelled`).
    #[serde(default)]
    pub on_status: Option<String>,
    /// If true, fire when status is `failed` (ignored when `on_status` is Some).
    #[serde(default)]
    pub on_failure: bool,
    pub message: String,
}

static LOG: OnceLock<RwLock<VecDeque<CronWatchEvent>>> = OnceLock::new();
static SUBS: OnceLock<RwLock<Vec<CronWatchSubscription>>> = OnceLock::new();
static STORE_PATH: OnceLock<RwLock<Option<PathBuf>>> = OnceLock::new();

fn log() -> &'static RwLock<VecDeque<CronWatchEvent>> {
    LOG.get_or_init(|| RwLock::new(VecDeque::with_capacity(64)))
}

fn subs() -> &'static RwLock<Vec<CronWatchSubscription>> {
    SUBS.get_or_init(|| RwLock::new(Vec::new()))
}

fn store_path_slot() -> &'static RwLock<Option<PathBuf>> {
    STORE_PATH.get_or_init(|| RwLock::new(None))
}

const MAX: usize = 500;

/// Called once from daemon startup so watches can insert wakeups.
pub async fn configure_store_path(path: PathBuf) {
    let mut g = store_path_slot().write().await;
    *g = Some(path);
}

pub async fn push_event(ev: CronWatchEvent) {
    {
        let g = log();
        let mut w = g.write().await;
        while w.len() >= MAX {
            w.pop_front();
        }
        w.push_back(ev.clone());
    }
    maybe_fire_subscriptions(&ev).await;
}

/// Notify cron watches that a scheduled task_run reached a terminal status.
pub async fn on_task_run_terminal(
    schedule_id: Uuid,
    schedule_name: &str,
    task_id: Uuid,
    task_run_id: Uuid,
    status: &TaskRunStatus,
) {
    let status_str = status.as_str().to_string();
    if !matches!(
        status,
        TaskRunStatus::Completed | TaskRunStatus::Failed | TaskRunStatus::Cancelled
    ) {
        return;
    }
    push_event(CronWatchEvent {
        ts_rfc3339: Utc::now().to_rfc3339(),
        schedule_id: schedule_id.to_string(),
        schedule_name: schedule_name.to_string(),
        task_id: task_id.to_string(),
        task_run_id: task_run_id.to_string(),
        status: status_str,
    })
    .await;
}

async fn maybe_fire_subscriptions(ev: &CronWatchEvent) {
    let list = {
        let g = subs().read().await;
        g.clone()
    };
    if list.is_empty() {
        return;
    }
    let store_path = {
        let g = store_path_slot().read().await;
        g.clone()
    };
    let Some(store_path) = store_path else {
        return;
    };
    for sub in list {
        if !sub.schedule_id.trim().is_empty()
            && !sub.schedule_id.eq_ignore_ascii_case(&ev.schedule_id)
        {
            continue;
        }
        if !sub.name_contains.is_empty()
            && !ev
                .schedule_name
                .to_lowercase()
                .contains(&sub.name_contains.to_lowercase())
        {
            continue;
        }
        let matched = if let Some(ref want) = sub.on_status {
            let want = want.trim().to_lowercase();
            !want.is_empty() && ev.status.eq_ignore_ascii_case(&want)
        } else if sub.on_failure {
            ev.status.eq_ignore_ascii_case("failed")
        } else {
            true
        };
        if !matched {
            continue;
        }
        if let Err(e) = insert_wakeup_now(&store_path, &sub, ev) {
            tracing::warn!(error = %e, sub_id = %sub.id, "cron watch wakeup insert failed");
        }
    }
}

fn insert_wakeup_now(
    store_path: &Path,
    sub: &CronWatchSubscription,
    ev: &CronWatchEvent,
) -> anyhow::Result<()> {
    let store = WakeupStore::open(store_path)?;
    let now = Utc::now();
    let msg = format!(
        "[cron watch] {} — schedule={} ({}) status={} task={}",
        sub.message, ev.schedule_name, ev.schedule_id, ev.status, ev.task_id
    );
    let session_id = if sub.session_id.trim().is_empty() {
        format!("schedule:{}", ev.schedule_id)
    } else {
        sub.session_id.clone()
    };
    let w = Wakeup {
        id: Uuid::new_v4(),
        session_id,
        fire_at: now,
        message: msg,
        status: "pending".to_string(),
        created_by_task_id: Uuid::parse_str(&ev.task_id).ok(),
        rrule: None,
        created_at: now,
    };
    store.insert(&w)?;
    Ok(())
}

pub async fn recent(limit: usize) -> Vec<CronWatchEvent> {
    let g = log();
    let r = g.read().await;
    let lim = limit.clamp(1, MAX);
    r.iter().rev().take(lim).cloned().collect()
}

pub async fn list_subscriptions() -> Vec<CronWatchSubscription> {
    subs().read().await.clone()
}

pub async fn add_subscription(sub: CronWatchSubscription) {
    let mut g = subs().write().await;
    g.retain(|s| s.id != sub.id);
    g.push(sub);
}

pub async fn remove_subscription(id: &str) -> bool {
    let mut g = subs().write().await;
    let before = g.len();
    g.retain(|s| s.id != id);
    g.len() < before
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    static TEST_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

    async fn test_guard() -> tokio::sync::MutexGuard<'static, ()> {
        TEST_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await
    }

    async fn reset_subs() {
        subs().write().await.clear();
    }

    #[tokio::test]
    async fn subscription_fires_on_matching_failure() {
        let _lock = test_guard().await;
        reset_subs().await;
        let db = NamedTempFile::new().expect("temp db");
        configure_store_path(db.path().to_path_buf()).await;
        add_subscription(CronWatchSubscription {
            id: "sub-1".into(),
            session_id: "sess-cron".into(),
            schedule_id: String::new(),
            name_contains: "overnight".into(),
            on_status: None,
            on_failure: true,
            message: "overnight failed".into(),
        })
        .await;

        let schedule_id = Uuid::new_v4();
        on_task_run_terminal(
            schedule_id,
            "Overnight pack",
            Uuid::new_v4(),
            Uuid::new_v4(),
            &TaskRunStatus::Failed,
        )
        .await;

        let store = WakeupStore::open(db.path()).expect("open wakeups");
        let pending = store
            .list_pending_before(Utc::now() + chrono::Duration::seconds(1))
            .expect("list");
        assert_eq!(pending.len(), 1);
        assert!(pending[0].message.contains("overnight failed"));
        assert_eq!(pending[0].session_id, "sess-cron");

        // Completed should not match on_failure-only sub.
        on_task_run_terminal(
            schedule_id,
            "Overnight pack",
            Uuid::new_v4(),
            Uuid::new_v4(),
            &TaskRunStatus::Completed,
        )
        .await;
        let pending2 = store
            .list_pending_before(Utc::now() + chrono::Duration::seconds(1))
            .expect("list");
        assert_eq!(pending2.len(), 1);
    }

    #[tokio::test]
    async fn name_filter_skips_non_matching() {
        let _lock = test_guard().await;
        reset_subs().await;
        let db = NamedTempFile::new().expect("temp db");
        configure_store_path(db.path().to_path_buf()).await;
        add_subscription(CronWatchSubscription {
            id: "sub-2".into(),
            session_id: "s".into(),
            schedule_id: String::new(),
            name_contains: "morning".into(),
            on_status: Some("completed".into()),
            on_failure: false,
            message: "brief done".into(),
        })
        .await;

        on_task_run_terminal(
            Uuid::new_v4(),
            "Overnight pack",
            Uuid::new_v4(),
            Uuid::new_v4(),
            &TaskRunStatus::Completed,
        )
        .await;

        let store = WakeupStore::open(db.path()).expect("open");
        let pending = store
            .list_pending_before(Utc::now() + chrono::Duration::seconds(1))
            .expect("list");
        assert!(pending.is_empty());
    }
}
