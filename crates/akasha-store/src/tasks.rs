//! Task storage - SQLite persistence per spec 10_data_model.yaml

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::Path;
use uuid::Uuid;

/// Maximum number of progress entries retained per task (in both store and in-memory cache).
pub const MAX_PROGRESS_PER_TASK: usize = 32;
/// Maximum number of task event entries retained per task in SQLite.
pub const MAX_TASK_EVENTS_PER_TASK: usize = 64;

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
    /// Task was running when daemon restarted; can be resumed (Phase 2 AI OS).
    Interrupted,
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
            Self::Interrupted => "interrupted",
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
            "interrupted" => Self::Interrupted,
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
    /// User message or context that started the task (title/summary for calendar and lists).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_message: Option<String>,
    /// Code Studio: projet UUID (`studio-projects/<id>/`) pour retrouver le workspace disque si le registre mémoire a été vidé (redémarrage daemon).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub studio_project_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskEventRecord {
    pub event_type: String,
    pub payload: Option<serde_json::Value>,
    pub at: String,
}

pub struct TaskStore {
    conn: Connection,
}

impl TaskStore {
    pub fn open<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        // Multiple TaskStore connections hit the same file (API + persistence threads); wait on locks
        // instead of failing immediately with SQLITE_BUSY.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // WAL: readers (API) can proceed while the writer persists; reduces "database is locked" vs rollback journal.
        if let Err(err) = conn.execute_batch("PRAGMA journal_mode=WAL;") {
            eprintln!("warning: failed to enable SQLite WAL mode; continuing without WAL: {err}");
        }
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS tasks (
                id TEXT PRIMARY KEY,
                parent_task_id TEXT,
                status TEXT NOT NULL,
                assigned_agent TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                initial_message TEXT
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
            CREATE TABLE IF NOT EXISTS task_events (
                task_id TEXT NOT NULL,
                seq INTEGER NOT NULL,
                event_type TEXT NOT NULL,
                payload_json TEXT,
                created_at TEXT NOT NULL,
                PRIMARY KEY (task_id, seq)
            );
            CREATE INDEX IF NOT EXISTS idx_task_events_task_id ON task_events(task_id);
            CREATE TABLE IF NOT EXISTS task_leases (
                task_id TEXT PRIMARY KEY,
                owner TEXT NOT NULL,
                heartbeat_at TEXT NOT NULL,
                expires_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_task_leases_expires_at ON task_leases(expires_at);
            "#,
        )?;
        // Migration: add initial_message if missing (existing DBs).
        let has_col: i32 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('tasks') WHERE name='initial_message'",
            [],
            |r| r.get(0),
        ).unwrap_or(0);
        if has_col == 0 {
            let _ = conn.execute("ALTER TABLE tasks ADD COLUMN initial_message TEXT", []);
        }
        let has_studio_pid: i32 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('tasks') WHERE name='studio_project_id'",
            [],
            |r| r.get(0),
        ).unwrap_or(0);
        if has_studio_pid == 0 {
            let _ = conn.execute("ALTER TABLE tasks ADD COLUMN studio_project_id TEXT", []);
        }
        crate::todos::create_task_todos_table(&conn)?;
        Ok(Self { conn })
    }

    /// Walk parents up to the root task id (same lineage as `workspace_lineage_root_task_id`).
    pub fn lineage_root_task_id(&self, mut task_id: Uuid) -> anyhow::Result<Uuid> {
        for _ in 0..32 {
            let t = self
                .get(task_id)?
                .ok_or_else(|| anyhow::anyhow!("task not found"))?;
            match t.parent_task_id {
                Some(p) => task_id = p,
                None => return Ok(task_id),
            }
        }
        Ok(task_id)
    }

    /// Code Studio project UUID stored on the lineage root, if any.
    pub fn lineage_root_studio_project_id(&self, task_id: Uuid) -> anyhow::Result<Option<String>> {
        let root_id = self.lineage_root_task_id(task_id)?;
        Ok(self
            .get(root_id)?
            .and_then(|t| t.studio_project_id.clone()))
    }

    /// Acquire or refresh a lease for a running task.
    pub fn upsert_lease(&self, task_id: Uuid, owner: &str, ttl_secs: u64) -> anyhow::Result<()> {
        let now = Utc::now();
        let expires_at = now + chrono::Duration::seconds(ttl_secs as i64);
        self.conn.execute(
            "INSERT INTO task_leases (task_id, owner, heartbeat_at, expires_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(task_id) DO UPDATE SET owner = excluded.owner, heartbeat_at = excluded.heartbeat_at, expires_at = excluded.expires_at",
            rusqlite::params![task_id.to_string(), owner, now.to_rfc3339(), expires_at.to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn clear_lease(&self, task_id: Uuid) -> anyhow::Result<()> {
        self.conn
            .execute("DELETE FROM task_leases WHERE task_id = ?1", [task_id.to_string()])?;
        Ok(())
    }

    /// Return task ids with expired leases.
    pub fn expired_leases(&self, now: DateTime<Utc>, limit: usize) -> anyhow::Result<Vec<Uuid>> {
        let mut stmt = self.conn.prepare(
            "SELECT task_id FROM task_leases WHERE datetime(expires_at) <= datetime(?1) ORDER BY expires_at ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![now.to_rfc3339(), limit as i64], |row| {
            let id: String = row.get(0)?;
            Ok(Uuid::parse_str(&id).ok())
        })?;
        let mut out = Vec::new();
        for row in rows {
            if let Some(id) = row? {
                out.push(id);
            }
        }
        Ok(out)
    }

    /// Get the todo list for a task (Deep Agents-style write_todos).
    pub fn get_todos(&self, task_id: Uuid) -> anyhow::Result<Vec<crate::todos::TodoItem>> {
        crate::todos::get_todos(&self.conn, task_id)
    }

    pub fn get_todos_with_updated_at(
        &self,
        task_id: Uuid,
    ) -> anyhow::Result<(Vec<crate::todos::TodoItem>, Option<String>)> {
        crate::todos::get_todos_with_updated_at(&self.conn, task_id)
    }

    pub fn merge_todos_from_payload(
        &self,
        task_id: Uuid,
        payload: &str,
    ) -> anyhow::Result<Vec<crate::todos::TodoItem>> {
        crate::todos::merge_todos_from_payload(&self.conn, task_id, payload)
    }

    /// Set the todo list for a task (replaces entire list). Emit TodoListUpdated from daemon after this.
    pub fn set_todos(&self, task_id: Uuid, todos: &[crate::todos::TodoItem]) -> anyhow::Result<()> {
        crate::todos::set_todos(&self.conn, task_id, todos)
    }

    /// Append a progress entry for a task (used by daemon to persist progress for fast GET /api/tasks/:id).
    pub fn insert_progress(&self, task_id: Uuid, progress_pct: u8, message: &str) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        // Single statement: avoid SELECT-then-INSERT races across connections (PK (task_id, seq) collision).
        self.conn.execute(
            "INSERT INTO task_progress (task_id, seq, progress_pct, message, created_at) \
             VALUES (?1, (SELECT COALESCE(MAX(seq), 0) + 1 FROM task_progress WHERE task_id = ?1), ?2, ?3, ?4)",
            rusqlite::params![task_id.to_string(), progress_pct as i32, message, now],
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

    /// Append a task event for a task and retain only the most recent entries.
    pub fn insert_event(
        &self,
        task_id: Uuid,
        event_type: &str,
        payload: Option<&serde_json::Value>,
        at: &str,
    ) -> anyhow::Result<()> {
        let payload_json = payload.map(serde_json::to_string).transpose()?;
        // Use a scalar subquery so seq assignment and INSERT are a single atomic statement,
        // eliminating the SELECT-then-INSERT race that could produce a (task_id, seq) PK collision.
        self.conn.execute(
            "INSERT INTO task_events (task_id, seq, event_type, payload_json, created_at) \
             VALUES (?1, (SELECT COALESCE(MAX(seq), 0) + 1 FROM task_events WHERE task_id = ?1), ?2, ?3, ?4)",
            rusqlite::params![task_id.to_string(), event_type, payload_json, at],
        )?;
        let max_events: i64 = MAX_TASK_EVENTS_PER_TASK as i64;
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM task_events WHERE task_id = ?1",
            [task_id.to_string()],
            |row| row.get(0),
        )?;
        if count > max_events {
            self.conn.execute(
                "DELETE FROM task_events WHERE task_id = ?1 AND seq IN (SELECT seq FROM task_events WHERE task_id = ?1 ORDER BY seq ASC LIMIT ?2)",
                rusqlite::params![task_id.to_string(), count - max_events],
            )?;
        }
        Ok(())
    }

    /// Get persisted task events for a task in chronological order.
    pub fn get_events(&self, task_id: Uuid) -> anyhow::Result<Vec<TaskEventRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT event_type, payload_json, created_at FROM task_events WHERE task_id = ?1 ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map([task_id.to_string()], |row| {
            let payload_json: Option<String> = row.get(1)?;
            let payload = payload_json
                .as_deref()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok());
            Ok(TaskEventRecord {
                event_type: row.get(0)?,
                payload,
                at: row.get(2)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn insert(&self, task: &Task) -> anyhow::Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO tasks (id, parent_task_id, status, assigned_agent, created_at, updated_at, initial_message, studio_project_id)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
            rusqlite::params![
                task.id.to_string(),
                task.parent_task_id.map(|u| u.to_string()),
                task.status.as_str(),
                task.assigned_agent,
                task.created_at.to_rfc3339(),
                task.updated_at.to_rfc3339(),
                task.initial_message.as_deref(),
                task.studio_project_id.as_deref(),
            ],
        )?;
        Ok(())
    }

    pub fn update_assigned_agent(&self, id: Uuid, agent: &str) -> anyhow::Result<()> {
        let now = Utc::now();
        self.conn.execute(
            "UPDATE tasks SET assigned_agent = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![agent, now.to_rfc3339(), id.to_string()],
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
            "SELECT id, parent_task_id, status, assigned_agent, created_at, updated_at, initial_message, studio_project_id FROM tasks ORDER BY created_at",
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
                initial_message: row.get(6).ok(),
                studio_project_id: row.get(7).ok(),
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
            "SELECT id, parent_task_id, status, assigned_agent, created_at, updated_at, initial_message, studio_project_id FROM tasks WHERE id = ?1",
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
                initial_message: row.get(6).ok(),
                studio_project_id: row.get(7).ok(),
            }));
        }
        Ok(None)
    }

    /// List tasks with created_at in the given range (for calendar view of ad-hoc tasks).
    pub fn list_tasks_created_between(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        limit: usize,
    ) -> anyhow::Result<Vec<Task>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, parent_task_id, status, assigned_agent, created_at, updated_at, initial_message, studio_project_id FROM tasks \
             WHERE created_at >= ?1 AND created_at <= ?2 ORDER BY created_at ASC LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![from.to_rfc3339(), to.to_rfc3339(), limit as i64],
            |row| {
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
                    initial_message: row.get(6).ok(),
                    studio_project_id: row.get(7).ok(),
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_children(&self, parent_id: Uuid) -> anyhow::Result<Vec<Task>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, parent_task_id, status, assigned_agent, created_at, updated_at, initial_message, studio_project_id FROM tasks WHERE parent_task_id = ?1 ORDER BY created_at",
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
                initial_message: row.get(6).ok(),
                studio_project_id: row.get(7).ok(),
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lease_lifecycle_and_expiry() {
        let tmp = tempfile::NamedTempFile::new().expect("temp db");
        let store = TaskStore::open(tmp.path()).expect("open");
        let id = Uuid::new_v4();

        store.upsert_lease(id, "orchestrator", 1).expect("upsert lease");
        let now = Utc::now();
        let expired_now = store.expired_leases(now, 10).expect("expired now");
        assert!(expired_now.is_empty(), "fresh lease should not be expired immediately");

        let future = now + chrono::Duration::seconds(2);
        let expired_future = store.expired_leases(future, 10).expect("expired future");
        assert!(expired_future.contains(&id), "lease should expire after ttl");

        store.clear_lease(id).expect("clear lease");
        let expired_after_clear = store.expired_leases(future, 10).expect("expired after clear");
        assert!(!expired_after_clear.contains(&id), "cleared lease must not be listed");
    }
}
