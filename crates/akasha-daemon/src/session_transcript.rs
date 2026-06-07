//! Operator session transcripts (jcode RFC phase A).
//! Writes `data_dir/transcripts/{task_id}.json` when a root task reaches a terminal state.

use akasha_store::{TaskStore, TaskStatus};
use std::path::Path;
use uuid::Uuid;

pub fn write_task_transcript(data_dir: &Path, store_path: &Path, task_id: Uuid) -> anyhow::Result<()> {
    let store = TaskStore::open(store_path)?;
    let Some(task) = store.get(task_id)? else {
        return Ok(());
    };
    if !matches!(
        task.status,
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
    ) {
        return Ok(());
    }

    let events = store.get_events(task_id).unwrap_or_default();
    let progress = store.get_progress(task_id).unwrap_or_default();
    let started_at = task.created_at.clone();
    let ended_at = chrono::Utc::now().to_rfc3339();

    let mut actions: Vec<serde_json::Value> = Vec::new();
    for ev in &events {
        if ev.event_type == "tool_call_started" || ev.event_type == "tool_invoked" {
            let tool = ev
                .payload
                .as_ref()
                .and_then(|p| p.get("tool"))
                .and_then(|v| v.as_str())
                .unwrap_or("tool");
            actions.push(serde_json::json!({
                "type": "tool_call",
                "tool_id": tool,
                "summary": format!("{} ({})", tool, ev.event_type),
                "status": "ok",
                "timestamp": ev.at
            }));
        }
    }
    if actions.is_empty() {
        for (pct, msg) in progress.iter().filter(|(p, _)| *p > 0) {
            actions.push(serde_json::json!({
                "type": "progress",
                "summary": msg,
                "status": if *pct >= 100 { "ok" } else { "running" },
                "timestamp": ended_at
            }));
        }
    }

    let permission_requests: Vec<serde_json::Value> = events
        .iter()
        .filter(|e| e.event_type == "tool_approval_request")
        .map(|e| {
            serde_json::json!({
                "request_id": e.payload.as_ref().and_then(|p| p.get("request_id")).cloned().unwrap_or(serde_json::Value::Null),
                "status": "pending"
            })
        })
        .collect();

    let last_msg = progress
        .iter()
        .rev()
        .find(|(_, m)| !m.trim().is_empty())
        .map(|(_, m)| m.clone())
        .unwrap_or_default();

    let doc = serde_json::json!({
        "session_id": format!("task:{}", task_id),
        "task_id": task_id.to_string(),
        "started_at": started_at.to_rfc3339(),
        "ended_at": ended_at,
        "assigned_agent": task.assigned_agent,
        "token_usage": { "input": 0, "output": 0, "total": 0 },
        "actions": actions,
        "permission_requests": permission_requests,
        "suggested_actions": [
            { "id": "open_tasks", "label": "Open tasks", "kind": "ui", "ui_action": "open_tasks" }
        ],
        "summary": last_msg.chars().take(2000).collect::<String>(),
        "status": task.status.as_str()
    });

    let dir = data_dir.join("transcripts");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{task_id}.json"));
    std::fs::write(path, serde_json::to_string_pretty(&doc)?)?;
    Ok(())
}
