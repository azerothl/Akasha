//! Per-task MCP invocation counters (policy budget from `tools_policy.yaml`).

use std::collections::HashMap;
use std::sync::Mutex;
use uuid::Uuid;

static MCP_CALLS: std::sync::OnceLock<Mutex<HashMap<Uuid, u32>>> = std::sync::OnceLock::new();

fn map() -> &'static Mutex<HashMap<Uuid, u32>> {
    MCP_CALLS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn record_call(task_id: Uuid) -> u32 {
    let mut g = map().lock().expect("mcp_budget lock");
    let n = g.entry(task_id).or_insert(0);
    *n = n.saturating_add(1);
    *n
}

pub fn count(task_id: Uuid) -> u32 {
    map()
        .lock()
        .expect("mcp_budget lock")
        .get(&task_id)
        .copied()
        .unwrap_or(0)
}

pub fn clear_task(task_id: Uuid) {
    let mut g = map().lock().expect("mcp_budget lock");
    g.remove(&task_id);
}
