//! Schedule and task_run storage — recurring tasks and runs (spec 10_data_model.yaml, 37_scheduler_design)

use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::Path;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleExceptionType {
    Skip,
    Override,
}

impl ScheduleExceptionType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Skip => "skip",
            Self::Override => "override",
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "override" => Self::Override,
            _ => Self::Skip,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schedule {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub timezone: String,
    pub rrule: String,
    /// Optional interval in seconds for scheduler (MVP: next run = last + interval).
    pub interval_seconds: Option<u64>,
    pub start_at: DateTime<Utc>,
    pub end_at: Option<DateTime<Utc>>,
    pub channel_context: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleException {
    pub id: Uuid,
    pub schedule_id: Uuid,
    pub type_: ScheduleExceptionType,
    pub date: NaiveDate,
    pub override_payload: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskRunStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Skipped,
    Cancelled,
}

impl TaskRunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::Cancelled => "cancelled",
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "running" => Self::Running,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "skipped" => Self::Skipped,
            "cancelled" => Self::Cancelled,
            _ => Self::Queued,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRun {
    pub id: Uuid,
    pub schedule_id: Option<Uuid>,
    pub task_id: Uuid,
    pub status: TaskRunStatus,
    pub planned_for: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub dedup_key: String,
}

pub struct ScheduleStore {
    conn: Connection,
}

impl ScheduleStore {
    pub fn open<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS schedules (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT NOT NULL,
                enabled INTEGER NOT NULL,
                timezone TEXT NOT NULL,
                rrule TEXT NOT NULL,
                interval_seconds INTEGER,
                start_at TEXT NOT NULL,
                end_at TEXT,
                channel_context TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_schedules_enabled ON schedules(enabled);

            CREATE TABLE IF NOT EXISTS schedule_exceptions (
                id TEXT PRIMARY KEY,
                schedule_id TEXT NOT NULL,
                type TEXT NOT NULL,
                date TEXT NOT NULL,
                override_payload TEXT,
                FOREIGN KEY (schedule_id) REFERENCES schedules(id)
            );
            CREATE INDEX IF NOT EXISTS idx_schedule_exceptions_schedule_date ON schedule_exceptions(schedule_id, date);

            CREATE TABLE IF NOT EXISTS task_runs (
                id TEXT PRIMARY KEY,
                schedule_id TEXT,
                task_id TEXT NOT NULL,
                status TEXT NOT NULL,
                planned_for TEXT NOT NULL,
                started_at TEXT,
                ended_at TEXT,
                dedup_key TEXT NOT NULL UNIQUE
            );
            CREATE INDEX IF NOT EXISTS idx_task_runs_schedule ON task_runs(schedule_id);
            CREATE INDEX IF NOT EXISTS idx_task_runs_planned ON task_runs(planned_for);
            CREATE INDEX IF NOT EXISTS idx_task_runs_dedup ON task_runs(dedup_key);
            "#,
        )?;
        // Migration: add interval_seconds if table existed without it
        let _ = conn.execute("ALTER TABLE schedules ADD COLUMN interval_seconds INTEGER", []);
        Ok(Self { conn })
    }

    pub fn insert_schedule(&self, s: &Schedule) -> anyhow::Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO schedules (id, name, description, enabled, timezone, rrule, interval_seconds, start_at, end_at, channel_context, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            "#,
            rusqlite::params![
                s.id.to_string(),
                s.name,
                s.description,
                s.enabled as i32,
                s.timezone,
                s.rrule,
                s.interval_seconds.map(|n| n as i64),
                s.start_at.to_rfc3339(),
                s.end_at.map(|t| t.to_rfc3339()),
                s.channel_context,
                s.created_at.to_rfc3339(),
                s.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn update_schedule(&self, s: &Schedule) -> anyhow::Result<()> {
        let now = Utc::now();
        self.conn.execute(
            r#"
            UPDATE schedules SET name = ?1, description = ?2, enabled = ?3, timezone = ?4, rrule = ?5, interval_seconds = ?6, start_at = ?7, end_at = ?8, channel_context = ?9, updated_at = ?10
            WHERE id = ?11
            "#,
            rusqlite::params![
                s.name,
                s.description,
                s.enabled as i32,
                s.timezone,
                s.rrule,
                s.interval_seconds.map(|n| n as i64),
                s.start_at.to_rfc3339(),
                s.end_at.map(|t| t.to_rfc3339()),
                s.channel_context,
                now.to_rfc3339(),
                s.id.to_string(),
            ],
        )?;
        Ok(())
    }

    pub fn delete_schedule(&self, id: Uuid) -> anyhow::Result<()> {
        self.conn.execute("DELETE FROM schedule_exceptions WHERE schedule_id = ?1", [id.to_string()])?;
        self.conn.execute("DELETE FROM schedules WHERE id = ?1", [id.to_string()])?;
        Ok(())
    }

    pub fn get_schedule(&self, id: Uuid) -> anyhow::Result<Option<Schedule>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, description, enabled, timezone, rrule, interval_seconds, start_at, end_at, channel_context, created_at, updated_at FROM schedules WHERE id = ?1",
        )?;
        let mut rows = stmt.query([id.to_string()])?;
        if let Some(row) = rows.next()? {
            return Ok(Some(row_to_schedule(row)?));
        }
        Ok(None)
    }

    pub fn list_schedules(&self) -> anyhow::Result<Vec<Schedule>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, description, enabled, timezone, rrule, interval_seconds, start_at, end_at, channel_context, created_at, updated_at FROM schedules ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], row_to_schedule)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn list_enabled_schedules(&self) -> anyhow::Result<Vec<Schedule>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, description, enabled, timezone, rrule, interval_seconds, start_at, end_at, channel_context, created_at, updated_at FROM schedules WHERE enabled = 1 ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], row_to_schedule)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn insert_exception(&self, e: &ScheduleException) -> anyhow::Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO schedule_exceptions (id, schedule_id, type, date, override_payload)
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
            rusqlite::params![
                e.id.to_string(),
                e.schedule_id.to_string(),
                e.type_.as_str(),
                e.date.format("%Y-%m-%d").to_string(),
                e.override_payload,
            ],
        )?;
        Ok(())
    }

    pub fn get_exceptions_for_schedule(&self, schedule_id: Uuid) -> anyhow::Result<Vec<ScheduleException>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, schedule_id, type, date, override_payload FROM schedule_exceptions WHERE schedule_id = ?1",
        )?;
        let rows = stmt.query_map([schedule_id.to_string()], |row| {
            let type_str: String = row.get(2)?;
            let date_str: String = row.get(3)?;
            let date = NaiveDate::parse_from_str(&date_str, "%Y-%m-%d").unwrap_or(NaiveDate::MIN);
            Ok(ScheduleException {
                id: Uuid::parse_str(&row.get::<_, String>(0)?).unwrap_or(Uuid::nil()),
                schedule_id: Uuid::parse_str(&row.get::<_, String>(1)?).unwrap_or(Uuid::nil()),
                type_: ScheduleExceptionType::from_str(&type_str),
                date,
                override_payload: row.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn insert_task_run(&self, r: &TaskRun) -> anyhow::Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO task_runs (id, schedule_id, task_id, status, planned_for, started_at, ended_at, dedup_key)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
            rusqlite::params![
                r.id.to_string(),
                r.schedule_id.map(|u| u.to_string()),
                r.task_id.to_string(),
                r.status.as_str(),
                r.planned_for.to_rfc3339(),
                r.started_at.map(|t| t.to_rfc3339()),
                r.ended_at.map(|t| t.to_rfc3339()),
                r.dedup_key,
            ],
        )?;
        Ok(())
    }

    /// Returns true if a task_run with this dedup_key already exists (for at-least-once dedup).
    pub fn dedup_key_exists(&self, dedup_key: &str) -> anyhow::Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(1) FROM task_runs WHERE dedup_key = ?1",
            [dedup_key],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn update_task_run_status(
        &self,
        id: Uuid,
        status: TaskRunStatus,
        started_at: Option<DateTime<Utc>>,
        ended_at: Option<DateTime<Utc>>,
    ) -> anyhow::Result<()> {
        let started_ts = started_at.map(|t| t.to_rfc3339());
        let ended_ts = ended_at.map(|t| t.to_rfc3339());
        self.conn.execute(
            "UPDATE task_runs SET status = ?1, started_at = COALESCE(?2, started_at), ended_at = COALESCE(?3, ended_at) WHERE id = ?4",
            rusqlite::params![status.as_str(), started_ts, ended_ts, id.to_string()],
        )?;
        Ok(())
    }

    pub fn get_task_run(&self, id: Uuid) -> anyhow::Result<Option<TaskRun>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, schedule_id, task_id, status, planned_for, started_at, ended_at, dedup_key FROM task_runs WHERE id = ?1",
        )?;
        let mut rows = stmt.query([id.to_string()])?;
        if let Some(row) = rows.next()? {
            return Ok(Some(row_to_task_run(row)?));
        }
        Ok(None)
    }

    pub fn list_task_runs(
        &self,
        schedule_id: Option<Uuid>,
        limit: usize,
    ) -> anyhow::Result<Vec<TaskRun>> {
        if let Some(sid) = schedule_id {
            let mut stmt = self.conn.prepare(
                "SELECT id, schedule_id, task_id, status, planned_for, started_at, ended_at, dedup_key FROM task_runs WHERE schedule_id = ?1 ORDER BY planned_for DESC LIMIT ?2",
            )?;
            let rows = stmt.query_map(rusqlite::params![sid.to_string(), limit as i64], row_to_task_run)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
        } else {
            let mut stmt = self.conn.prepare(
                "SELECT id, schedule_id, task_id, status, planned_for, started_at, ended_at, dedup_key FROM task_runs ORDER BY planned_for DESC LIMIT ?1",
            )?;
            let rows = stmt.query_map(rusqlite::params![limit as i64], row_to_task_run)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
        }
    }

    pub fn list_upcoming_task_runs(&self, from: DateTime<Utc>, limit: usize) -> anyhow::Result<Vec<TaskRun>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, schedule_id, task_id, status, planned_for, started_at, ended_at, dedup_key FROM task_runs WHERE planned_for >= ?1 AND status = 'queued' ORDER BY planned_for ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![from.to_rfc3339(), limit as i64], row_to_task_run)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// List task runs with planned_for or started_at in the given range (for calendar view).
    pub fn list_task_runs_between(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        limit: usize,
    ) -> anyhow::Result<Vec<TaskRun>> {
        let from_s = from.to_rfc3339();
        let to_s = to.to_rfc3339();
        let mut stmt = self.conn.prepare(
            "SELECT id, schedule_id, task_id, status, planned_for, started_at, ended_at, dedup_key FROM task_runs \
             WHERE (planned_for >= ?1 AND planned_for <= ?2) OR (started_at >= ?1 AND started_at <= ?2) \
             ORDER BY planned_for ASC LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![from_s, to_s, limit as i64],
            row_to_task_run,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}

fn row_to_schedule(row: &rusqlite::Row) -> rusqlite::Result<Schedule> {
    Ok(Schedule {
        id: Uuid::parse_str(&row.get::<_, String>(0)?).unwrap_or(Uuid::nil()),
        name: row.get(1)?,
        description: row.get(2)?,
        enabled: row.get::<_, i32>(3)? != 0,
        timezone: row.get(4)?,
        rrule: row.get(5)?,
        interval_seconds: row.get::<_, Option<i64>>(6)?.map(|n| n as u64),
        start_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(7)?)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(
                7, rusqlite::types::Type::Text, Box::new(e),
            ))?
            .with_timezone(&Utc),
        end_at: row
            .get::<_, Option<String>>(8)?
            .and_then(|s| DateTime::parse_from_rfc3339(&s).ok().map(|t| t.with_timezone(&Utc))),
        channel_context: row.get(9)?,
        created_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(10)?)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(
                10, rusqlite::types::Type::Text, Box::new(e),
            ))?
            .with_timezone(&Utc),
        updated_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(11)?)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(
                11, rusqlite::types::Type::Text, Box::new(e),
            ))?
            .with_timezone(&Utc),
    })
}

fn row_to_task_run(row: &rusqlite::Row) -> rusqlite::Result<TaskRun> {
    let status_str: String = row.get(3)?;
    Ok(TaskRun {
        id: Uuid::parse_str(&row.get::<_, String>(0)?).unwrap_or(Uuid::nil()),
        schedule_id: row.get::<_, Option<String>>(1)?.and_then(|s| Uuid::parse_str(&s).ok()),
        task_id: Uuid::parse_str(&row.get::<_, String>(2)?).unwrap_or(Uuid::nil()),
        status: TaskRunStatus::from_str(&status_str),
        planned_for: DateTime::parse_from_rfc3339(&row.get::<_, String>(4)?)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(
                4, rusqlite::types::Type::Text, Box::new(e),
            ))?
            .with_timezone(&Utc),
        started_at: row
            .get::<_, Option<String>>(5)?
            .and_then(|s| DateTime::parse_from_rfc3339(&s).ok().map(|t| t.with_timezone(&Utc))),
        ended_at: row
            .get::<_, Option<String>>(6)?
            .and_then(|s| DateTime::parse_from_rfc3339(&s).ok().map(|t| t.with_timezone(&Utc))),
        dedup_key: row.get(7)?,
    })
}
