//! Ring buffer of recent background `run_command_background` completions (process watch / notifications baseline).
//! P7 B3: optional watch subscriptions that enqueue a wakeup when exit condition matches.

use akasha_store::platform_extras::{Wakeup, WakeupStore};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tokio::sync::RwLock;
use uuid::Uuid;

#[derive(Clone, Serialize)]
pub struct ProcessWatchEvent {
    pub ts_rfc3339: String,
    pub session_id: String,
    pub cmd_line: String,
    pub success: bool,
    pub exit_code: Option<i32>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ProcessWatchSubscription {
    pub id: String,
    pub session_id: String,
    /// Substring match on cmd_line (empty = any).
    pub cmd_contains: String,
    /// If set, only fire when exit_code equals this value.
    pub exit_code: Option<i32>,
    /// If true, fire when success==false (overrides exit_code when both set? exit_code wins if Some).
    pub on_failure: bool,
    pub message: String,
}

static LOG: OnceLock<RwLock<VecDeque<ProcessWatchEvent>>> = OnceLock::new();
static SUBS: OnceLock<RwLock<Vec<ProcessWatchSubscription>>> = OnceLock::new();
static STORE_PATH: OnceLock<RwLock<Option<PathBuf>>> = OnceLock::new();

fn log() -> &'static RwLock<VecDeque<ProcessWatchEvent>> {
    LOG.get_or_init(|| RwLock::new(VecDeque::with_capacity(64)))
}

fn subs() -> &'static RwLock<Vec<ProcessWatchSubscription>> {
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

pub async fn push_event(ev: ProcessWatchEvent) {
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

async fn maybe_fire_subscriptions(ev: &ProcessWatchEvent) {
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
        if !sub.cmd_contains.is_empty()
            && !ev
                .cmd_line
                .to_lowercase()
                .contains(&sub.cmd_contains.to_lowercase())
        {
            continue;
        }
        let matched = if let Some(code) = sub.exit_code {
            ev.exit_code == Some(code)
        } else if sub.on_failure {
            !ev.success
        } else {
            true
        };
        if !matched {
            continue;
        }
        if let Err(e) = insert_wakeup_now(&store_path, &sub, ev) {
            tracing::warn!(error = %e, sub_id = %sub.id, "process watch wakeup insert failed");
        }
    }
}

fn insert_wakeup_now(
    store_path: &Path,
    sub: &ProcessWatchSubscription,
    ev: &ProcessWatchEvent,
) -> anyhow::Result<()> {
    let store = WakeupStore::open(store_path)?;
    let now = Utc::now();
    let msg = format!(
        "[process watch] {} — cmd={} exit={:?} success={}",
        sub.message, ev.cmd_line, ev.exit_code, ev.success
    );
    let w = Wakeup {
        id: Uuid::new_v4(),
        session_id: if sub.session_id.trim().is_empty() {
            ev.session_id.clone()
        } else {
            sub.session_id.clone()
        },
        fire_at: now,
        message: msg,
        status: "pending".to_string(),
        created_by_task_id: None,
        rrule: None,
        created_at: now,
    };
    store.insert(&w)?;
    Ok(())
}

pub async fn recent(limit: usize) -> Vec<ProcessWatchEvent> {
    let g = log();
    let r = g.read().await;
    let lim = limit.clamp(1, MAX);
    r.iter().rev().take(lim).cloned().collect()
}

pub async fn list_subscriptions() -> Vec<ProcessWatchSubscription> {
    subs().read().await.clone()
}

pub async fn add_subscription(sub: ProcessWatchSubscription) {
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
