//! Ring buffer of recent background `run_command_background` completions (Hermes-style process watch / notifications baseline).

use serde::Serialize;
use std::collections::VecDeque;
use std::sync::OnceLock;
use tokio::sync::RwLock;

#[derive(Clone, Serialize)]
pub struct ProcessWatchEvent {
    pub ts_rfc3339: String,
    pub session_id: String,
    pub cmd_line: String,
    pub success: bool,
    pub exit_code: Option<i32>,
}

static LOG: OnceLock<RwLock<VecDeque<ProcessWatchEvent>>> = OnceLock::new();

fn log() -> &'static RwLock<VecDeque<ProcessWatchEvent>> {
    LOG.get_or_init(|| RwLock::new(VecDeque::with_capacity(64)))
}

const MAX: usize = 500;

pub async fn push_event(ev: ProcessWatchEvent) {
    let g = log();
    let mut w = g.write().await;
    while w.len() >= MAX {
        w.pop_front();
    }
    w.push_back(ev);
}

pub async fn recent(limit: usize) -> Vec<ProcessWatchEvent> {
    let g = log();
    let r = g.read().await;
    let lim = limit.clamp(1, MAX);
    r.iter().rev().take(lim).cloned().collect()
}
