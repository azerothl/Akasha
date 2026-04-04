//! Autonomous mission snapshot + append-only events (same SQLite DB as tasks).

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::Path;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissionHorizon {
    Short,
    Medium,
    Long,
}

impl Default for MissionHorizon {
    fn default() -> Self {
        Self::Medium
    }
}

impl MissionHorizon {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Short => "short",
            Self::Medium => "medium",
            Self::Long => "long",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "short" => Some(Self::Short),
            "medium" => Some(Self::Medium),
            "long" => Some(Self::Long),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissionStatus {
    Active,
    Paused,
    Completed,
}

impl Default for MissionStatus {
    fn default() -> Self {
        Self::Paused
    }
}

impl MissionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Completed => "completed",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "active" => Some(Self::Active),
            "paused" => Some(Self::Paused),
            "completed" => Some(Self::Completed),
            _ => None,
        }
    }
}

/// Persisted mirror of YAML config + runtime meta (single mission).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutonomousMissionSnapshot {
    pub enabled: bool,
    pub global_context: String,
    pub horizon: MissionHorizon,
    pub objective: String,
    pub heartbeat_interval_minutes: u64,
    pub report_dir: String,
    pub session_id: String,
    pub status: MissionStatus,
    pub updated_at: DateTime<Utc>,
}

impl Default for AutonomousMissionSnapshot {
    fn default() -> Self {
        Self {
            enabled: false,
            global_context: String::new(),
            horizon: MissionHorizon::default(),
            objective: String::new(),
            heartbeat_interval_minutes: 120,
            report_dir: "autonomous_mission/reports".to_string(),
            session_id: "autonomous:default".to_string(),
            status: MissionStatus::Paused,
            updated_at: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutonomousMissionEvent {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub event_type: String,
    pub payload: Option<serde_json::Value>,
}

pub struct AutonomousMissionStore {
    conn: Connection,
}

impl AutonomousMissionStore {
    pub fn open<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS autonomous_mission_snapshot (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                enabled INTEGER NOT NULL,
                global_context TEXT NOT NULL DEFAULT '',
                horizon TEXT NOT NULL DEFAULT 'medium',
                objective TEXT NOT NULL DEFAULT '',
                heartbeat_interval_minutes INTEGER NOT NULL DEFAULT 120,
                report_dir TEXT NOT NULL DEFAULT 'autonomous_mission/reports',
                session_id TEXT NOT NULL DEFAULT 'autonomous:default',
                status TEXT NOT NULL DEFAULT 'paused',
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS autonomous_mission_meta (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                last_heartbeat_at TEXT,
                last_task_id TEXT,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS autonomous_mission_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                at TEXT NOT NULL,
                event_type TEXT NOT NULL,
                payload_json TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_autonomous_mission_events_at ON autonomous_mission_events(at);
            "#,
        )?;
        Ok(Self { conn })
    }

    pub fn upsert_snapshot(&self, s: &AutonomousMissionSnapshot) -> anyhow::Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO autonomous_mission_snapshot (
                id, enabled, global_context, horizon, objective, heartbeat_interval_minutes,
                report_dir, session_id, status, updated_at
            ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(id) DO UPDATE SET
                enabled = excluded.enabled,
                global_context = excluded.global_context,
                horizon = excluded.horizon,
                objective = excluded.objective,
                heartbeat_interval_minutes = excluded.heartbeat_interval_minutes,
                report_dir = excluded.report_dir,
                session_id = excluded.session_id,
                status = excluded.status,
                updated_at = excluded.updated_at
            "#,
            rusqlite::params![
                if s.enabled { 1i32 } else { 0 },
                &s.global_context,
                s.horizon.as_str(),
                &s.objective,
                s.heartbeat_interval_minutes as i64,
                &s.report_dir,
                &s.session_id,
                s.status.as_str(),
                s.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn get_snapshot(&self) -> anyhow::Result<Option<AutonomousMissionSnapshot>> {
        let mut stmt = self.conn.prepare(
            r#"SELECT enabled, global_context, horizon, objective, heartbeat_interval_minutes,
                      report_dir, session_id, status, updated_at
               FROM autonomous_mission_snapshot WHERE id = 1"#,
        )?;
        let mut rows = stmt.query([])?;
        if let Some(row) = rows.next()? {
            let horizon_s: String = row.get(2)?;
            let status_s: String = row.get(7)?;
            let heartbeat_interval_minutes_i64: i64 = row.get(4)?;
            let heartbeat_interval_minutes =
                u64::try_from(heartbeat_interval_minutes_i64).unwrap_or(0);
            return Ok(Some(AutonomousMissionSnapshot {
                enabled: row.get::<_, i32>(0)? != 0,
                global_context: row.get(1)?,
                horizon: MissionHorizon::from_str(&horizon_s).unwrap_or_default(),
                objective: row.get(3)?,
                heartbeat_interval_minutes,
                report_dir: row.get(5)?,
                session_id: row.get(6)?,
                status: MissionStatus::from_str(&status_s).unwrap_or_default(),
                updated_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(8)?)
                    .map(|d| d.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now()),
            }));
        }
        Ok(None)
    }

    pub fn upsert_meta(
        &self,
        last_heartbeat_at: Option<DateTime<Utc>>,
        last_task_id: Option<Uuid>,
    ) -> anyhow::Result<()> {
        let now = Utc::now();
        self.conn.execute(
            r#"
            INSERT INTO autonomous_mission_meta (id, last_heartbeat_at, last_task_id, updated_at)
            VALUES (1, ?1, ?2, ?3)
            ON CONFLICT(id) DO UPDATE SET
                last_heartbeat_at = excluded.last_heartbeat_at,
                last_task_id = excluded.last_task_id,
                updated_at = excluded.updated_at
            "#,
            rusqlite::params![
                last_heartbeat_at.map(|t| t.to_rfc3339()),
                last_task_id.map(|u| u.to_string()),
                now.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn get_meta(&self) -> anyhow::Result<(Option<DateTime<Utc>>, Option<Uuid>)> {
        let mut stmt = self
            .conn
            .prepare("SELECT last_heartbeat_at, last_task_id FROM autonomous_mission_meta WHERE id = 1")?;
        let mut rows = stmt.query([])?;
        if let Some(row) = rows.next()? {
            let hb: Option<String> = row.get(0)?;
            let tid: Option<String> = row.get(1)?;
            let last_hb = hb.and_then(|s| DateTime::parse_from_rfc3339(&s).ok().map(|d| d.with_timezone(&Utc)));
            let last_task = tid.and_then(|s| Uuid::parse_str(&s).ok());
            return Ok((last_hb, last_task));
        }
        Ok((None, None))
    }

    pub fn insert_event(
        &self,
        event_type: &str,
        payload: Option<&serde_json::Value>,
    ) -> anyhow::Result<i64> {
        let at = Utc::now().to_rfc3339();
        let payload_json = payload.map(serde_json::to_string).transpose()?;
        self.conn.execute(
            "INSERT INTO autonomous_mission_events (at, event_type, payload_json) VALUES (?1, ?2, ?3)",
            rusqlite::params![at, event_type, payload_json],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn list_events_since(
        &self,
        since: Option<DateTime<Utc>>,
        limit: usize,
    ) -> anyhow::Result<Vec<AutonomousMissionEvent>> {
        let limit = limit.max(1).min(10_000) as i64;
        let out: Vec<AutonomousMissionEvent> = if let Some(s) = since {
            let mut stmt = self.conn.prepare(
                "SELECT id, at, event_type, payload_json FROM autonomous_mission_events \
                 WHERE at >= ?1 ORDER BY id ASC LIMIT ?2",
            )?;
            let rows = stmt.query_map(rusqlite::params![s.to_rfc3339(), limit], |row| {
                Self::map_event_row(row)
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        } else {
            let mut stmt = self.conn.prepare(
                "SELECT id, at, event_type, payload_json FROM autonomous_mission_events \
                 ORDER BY id DESC LIMIT ?1",
            )?;
            let rows = stmt.query_map([limit], |row| Self::map_event_row(row))?;
            let mut v: Vec<_> = rows.collect::<Result<Vec<_>, _>>()?;
            v.reverse();
            v
        };
        Ok(out)
    }

    fn map_event_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AutonomousMissionEvent> {
        let payload_json: Option<String> = row.get(3)?;
        let payload = payload_json
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok());
        let at_s: String = row.get(1)?;
        let at = DateTime::parse_from_rfc3339(&at_s)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());
        Ok(AutonomousMissionEvent {
            id: row.get(0)?,
            at,
            event_type: row.get(2)?,
            payload,
        })
    }
}
