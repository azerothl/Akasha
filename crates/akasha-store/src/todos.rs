//! Task todo list storage (Deep Agents-style write_todos).
//! Persists agent-defined step lists per task for UI and orchestration.

use chrono::Utc;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    Done,
    Cancelled,
}

impl TodoStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Done => "done",
            Self::Cancelled => "cancelled",
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "done" => Self::Done,
            "cancelled" => Self::Cancelled,
            _ => Self::Pending,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub title: String,
    pub status: TodoStatus,
}

/// Todos are stored in the same SQLite DB as tasks (table task_todos).
pub fn create_task_todos_table(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS task_todos (
            task_id TEXT PRIMARY KEY,
            todos_json TEXT NOT NULL DEFAULT '[]',
            updated_at TEXT NOT NULL
        );
        "#,
    )?;
    Ok(())
}

pub fn get_todos(conn: &Connection, task_id: Uuid) -> anyhow::Result<Vec<TodoItem>> {
    let mut stmt = conn.prepare("SELECT todos_json FROM task_todos WHERE task_id = ?1")?;
    let mut rows = stmt.query([task_id.to_string()])?;
    if let Some(row) = rows.next()? {
        let json: String = row.get(0)?;
        let todos: Vec<TodoItem> = serde_json::from_str(&json).unwrap_or_default();
        return Ok(todos);
    }
    Ok(Vec::new())
}

pub fn set_todos(conn: &Connection, task_id: Uuid, todos: &[TodoItem]) -> anyhow::Result<()> {
    let now = Utc::now().to_rfc3339();
    let json = serde_json::to_string(todos).unwrap_or_else(|_| "[]".to_string());
    conn.execute(
        r#"
        INSERT INTO task_todos (task_id, todos_json, updated_at) VALUES (?1, ?2, ?3)
        ON CONFLICT(task_id) DO UPDATE SET todos_json = excluded.todos_json, updated_at = excluded.updated_at
        "#,
        rusqlite::params![task_id.to_string(), json, now],
    )?;
    Ok(())
}

/// Parse todos from agent payload: JSON array of { id?, title, status? } or line-based "title" per line.
pub fn parse_todos_from_payload(payload: &str) -> Vec<TodoItem> {
    let trimmed = payload.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    // Helper to parse a JSON array of objects, with status optional (defaulting to Pending).
    let parse_json_array = |arr: &[serde_json::Value]| -> Vec<TodoItem> {
        arr.iter()
            .filter_map(|v| {
                let title = v.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string();
                let status = v
                    .get("status")
                    .and_then(|s| s.as_str())
                    .map(TodoStatus::from_str)
                    .unwrap_or(TodoStatus::Pending);
                let id = v.get("id").and_then(|i| i.as_str()).map(String::from);
                if title.is_empty() {
                    None
                } else {
                    Some(TodoItem { id, title, status })
                }
            })
            .collect()
    };
    // Try JSON array first (status optional)
    if let Ok(serde_json::Value::Array(arr)) = serde_json::from_str::<serde_json::Value>(trimmed) {
        let items = parse_json_array(&arr);
        if !items.is_empty() {
            return items;
        }
    }
    // Try JSON object with "todos" key
    if let Ok(obj) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Some(arr) = obj.get("todos").and_then(|v| v.as_array()) {
            let items = parse_json_array(arr);
            if !items.is_empty() {
                return items;
            }
        }
    }
    // Line-based: one title per line
    trimmed
        .lines()
        .map(|l| l.trim().trim_start_matches('-').trim())
        .filter(|l| !l.is_empty())
        .enumerate()
        .map(|(i, title)| TodoItem {
            id: Some(format!("{}", i + 1)),
            title: title.to_string(),
            status: TodoStatus::Pending,
        })
        .collect()
}
