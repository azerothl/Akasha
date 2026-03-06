//! Task storage - SQLite persistence per spec 10_data_model.yaml

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::Path;
use uuid::Uuid;

/// Maximum number of progress entries retained per task (in both store and in-memory cache).
pub const MAX_PROGRESS_PER_TASK: usize = 32;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Queued,
    Running,
    Completed,
    Failed,
    Paused,
    Cancelled,
    WaitingUserInput,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Paused => "paused",
            Self::Cancelled => "cancelled",
            Self::WaitingUserInput => "waiting_user_input",
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "paused" => Self::Paused,
            "cancelled" => Self::Cancelled,
            "waiting_user_input" => Self::WaitingUserInput,
            _ => Self::Pending,
        }
    }
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: Uuid,
    pub parent_task_id: Option<Uuid>,
    pub status: TaskStatus,
    pub assigned_agent: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub struct TaskStore {
    conn: Connection,
}

impl TaskStore {
    pub fn open<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS tasks (
                id TEXT PRIMARY KEY,
                parent_task_id TEXT,
                status TEXT NOT NULL,
                assigned_agent TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
            CREATE INDEX IF NOT EXISTS idx_tasks_parent ON tasks(parent_task_id);
            CREATE TABLE IF NOT EXISTS task_progress (
                task_id TEXT NOT NULL,
                seq INTEGER NOT NULL,
                progress_pct INTEGER NOT NULL,
                message TEXT NOT NULL,
                created_at TEXT NOT NULL,
                PRIMARY KEY (task_id, seq)
            );
            CREATE INDEX IF NOT EXISTS idx_task_progress_task_id ON task_progress(task_id);
            "#,
        )?;
        Ok(Self { conn })
    }

    /// Append a progress entry for a task (used by daemon to persist progress for fast GET /api/tasks/:id).
    pub fn insert_progress(&self, task_id: Uuid, progress_pct: u8, message: &str) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        let seq: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM task_progress WHERE task_id = ?1",
            [task_id.to_string()],
            |row| row.get(0),
        )?;
        self.conn.execute(
            "INSERT INTO task_progress (task_id, seq, progress_pct, message, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![task_id.to_string(), seq, progress_pct as i32, message, now],
        )?;
        let max_progress: i64 = MAX_PROGRESS_PER_TASK as i64;
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM task_progress WHERE task_id = ?1",
            [task_id.to_string()],
            |row| row.get(0),
        )?;
        if count > max_progress {
            self.conn.execute(
                "DELETE FROM task_progress WHERE task_id = ?1 AND seq IN (SELECT seq FROM task_progress WHERE task_id = ?1 ORDER BY seq ASC LIMIT ?2)",
                rusqlite::params![task_id.to_string(), count - max_progress],
            )?;
        }
        Ok(())
    }

    /// Get persisted progress entries for a task (chronological order).
    pub fn get_progress(&self, task_id: Uuid) -> anyhow::Result<Vec<(u8, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT progress_pct, message FROM task_progress WHERE task_id = ?1 ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map([task_id.to_string()], |row| {
            Ok((row.get::<_, i32>(0)? as u8, row.get::<_, String>(1)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn insert(&self, task: &Task) -> anyhow::Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO tasks (id, parent_task_id, status, assigned_agent, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
            rusqlite::params![
                task.id.to_string(),
                task.parent_task_id.map(|u| u.to_string()),
                task.status.as_str(),
                task.assigned_agent,
                task.created_at.to_rfc3339(),
                task.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn update_status(&self, id: Uuid, status: TaskStatus) -> anyhow::Result<()> {
        let now = Utc::now();
        self.conn.execute(
            "UPDATE tasks SET status = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![status.as_str(), now.to_rfc3339(), id.to_string()],
        )?;
        Ok(())
    }

    pub fn get_all(&self) -> anyhow::Result<Vec<Task>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, parent_task_id, status, assigned_agent, created_at, updated_at FROM tasks ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], |row| {
            let status_str: String = row.get(2)?;
            let status = TaskStatus::from_str(&status_str);
            Ok(Task {
                id: Uuid::parse_str(&row.get::<_, String>(0)?).unwrap_or(Uuid::nil()),
                parent_task_id: row.get::<_, Option<String>>(1)?.and_then(|s| Uuid::parse_str(&s).ok()),
                status,
                assigned_agent: row.get(3)?,
                created_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(4)?)
                    .unwrap()
                    .with_timezone(&Utc),
                updated_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(5)?)
                    .unwrap()
                    .with_timezone(&Utc),
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_pending_or_running(&self) -> anyhow::Result<Vec<Task>> {
        self.get_all().map(|tasks| {
            tasks
                .into_iter()
                .filter(|t| {
                    matches!(
                        t.status,
                        TaskStatus::Pending | TaskStatus::Queued | TaskStatus::Running
                    )
                })
                .collect()
        })
    }

    pub fn get(&self, id: Uuid) -> anyhow::Result<Option<Task>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, parent_task_id, status, assigned_agent, created_at, updated_at FROM tasks WHERE id = ?1",
        )?;
        let mut rows = stmt.query([id.to_string()])?;
        if let Some(row) = rows.next()? {
            let status_str: String = row.get(2)?;
            let status = TaskStatus::from_str(&status_str);
            return Ok(Some(Task {
                id: Uuid::parse_str(&row.get::<_, String>(0)?).unwrap_or(Uuid::nil()),
                parent_task_id: row.get::<_, Option<String>>(1)?.and_then(|s| Uuid::parse_str(&s).ok()),
                status,
                assigned_agent: row.get(3)?,
                created_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(4)?)
                    .unwrap()
                    .with_timezone(&Utc),
                updated_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(5)?)
                    .unwrap()
                    .with_timezone(&Utc),
            }));
        }
        Ok(None)
    }

    pub fn get_children(&self, parent_id: Uuid) -> anyhow::Result<Vec<Task>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, parent_task_id, status, assigned_agent, created_at, updated_at FROM tasks WHERE parent_task_id = ?1 ORDER BY created_at",
        )?;
        let rows = stmt.query_map([parent_id.to_string()], |row| {
            let status_str: String = row.get(2)?;
            let status = TaskStatus::from_str(&status_str);
            Ok(Task {
                id: Uuid::parse_str(&row.get::<_, String>(0)?).unwrap_or(Uuid::nil()),
                parent_task_id: Some(parent_id),
                status,
                assigned_agent: row.get(3)?,
                created_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(4)?)
                    .unwrap()
                    .with_timezone(&Utc),
                updated_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(5)?)
                    .unwrap()
                    .with_timezone(&Utc),
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}
