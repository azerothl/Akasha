//! Structured inter-agent messaging (request/reply/info) with depth and rate limits.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterAgentKind {
    Request,
    Reply,
    Info,
}

impl InterAgentKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Reply => "reply",
            Self::Info => "info",
        }
    }
}

#[derive(Debug, Clone)]
pub struct InterAgentMessage {
    pub correlation_id: Uuid,
    pub parent_task_id: Uuid,
    pub kind: InterAgentKind,
    pub payload: String,
    pub depth: u8,
}

pub fn max_inter_agent_depth() -> u8 {
    std::env::var("AKASHA_INTER_AGENT_MAX_DEPTH")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3)
}

pub fn inter_agent_rate_limit_per_minute() -> u32 {
    std::env::var("AKASHA_INTER_AGENT_RATE_LIMIT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20)
}

static RATE: Mutex<Option<(Instant, HashMap<Uuid, u32>)>> = Mutex::new(None);

pub fn check_rate_limit(parent_task_id: Uuid) -> bool {
    let limit = inter_agent_rate_limit_per_minute();
    let mut g = RATE.lock().unwrap();
    let now = Instant::now();
    let entry = g.get_or_insert_with(|| (now, HashMap::new()));
    if now.duration_since(entry.0) > Duration::from_secs(60) {
        *entry = (now, HashMap::new());
    }
    let count = entry.1.entry(parent_task_id).or_insert(0);
    if *count >= limit {
        return false;
    }
    *count += 1;
    true
}

pub fn validate_message(msg: &InterAgentMessage) -> Result<(), &'static str> {
    if msg.depth > max_inter_agent_depth() {
        return Err("inter_agent_max_depth_exceeded");
    }
    if !check_rate_limit(msg.parent_task_id) {
        return Err("inter_agent_rate_limit");
    }
    if msg.kind == InterAgentKind::Reply {
        // Replies must be informational downstream — orchestrator converts to Info for parent.
    }
    Ok(())
}
