//! Event-triggered task definitions and run history.

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::Path;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TriggerType {
    Webhook,
    TaskFailed,
    Filesystem,
    ModelAvailable,
    DaemonError,
}

impl TriggerType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Webhook => "webhook",
            Self::TaskFailed => "task_failed",
            Self::Filesystem => "filesystem",
            Self::ModelAvailable => "model_available",
            Self::DaemonError => "daemon_error",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "task_failed" => Self::TaskFailed,
            "filesystem" => Self::Filesystem,
            "model_available" => Self::ModelAvailable,
            "daemon_error" => Self::DaemonError,
            _ => Self::Webhook,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TriggerExecutionMode {
    Direct,
    Guided,
    Orchestrated,
}

impl TriggerExecutionMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Guided => "guided",
            Self::Orchestrated => "orchestrated",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "direct" => Some(Self::Direct),
            "guided" => Some(Self::Guided),
            "orchestrated" => Some(Self::Orchestrated),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventTrigger {
    pub id: Uuid,
    pub name: String,
    pub enabled: bool,
    pub trigger_type: TriggerType,
    pub filter: serde_json::Value,
    pub prompt_template: String,
    pub assigned_agent: Option<String>,
    pub execution_mode: Option<TriggerExecutionMode>,
    pub cooldown_seconds: u64,
    pub last_fired_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventTriggerRun {
    pub id: Uuid,
    pub trigger_id: Uuid,
    pub task_id: Uuid,
    pub fired_at: DateTime<Utc>,
    pub match_payload: Option<String>,
}

pub struct EventTriggerStore {
    conn: Connection,
}

impl EventTriggerStore {
    pub fn open<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS event_triggers (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                enabled INTEGER NOT NULL,
                trigger_type TEXT NOT NULL,
                filter_json TEXT NOT NULL DEFAULT '{}',
                prompt_template TEXT NOT NULL,
                assigned_agent TEXT,
                execution_mode TEXT,
                cooldown_seconds INTEGER NOT NULL DEFAULT 60,
                last_fired_at TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_event_triggers_enabled_type ON event_triggers(enabled, trigger_type);

            CREATE TABLE IF NOT EXISTS event_trigger_runs (
                id TEXT PRIMARY KEY,
                trigger_id TEXT NOT NULL,
                task_id TEXT NOT NULL,
                fired_at TEXT NOT NULL,
                match_payload TEXT,
                FOREIGN KEY (trigger_id) REFERENCES event_triggers(id)
            );
            CREATE INDEX IF NOT EXISTS idx_event_trigger_runs_trigger ON event_trigger_runs(trigger_id);
            "#,
        )?;
        Ok(Self { conn })
    }

    pub fn insert(&self, t: &EventTrigger) -> anyhow::Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO event_triggers (
                id, name, enabled, trigger_type, filter_json, prompt_template,
                assigned_agent, execution_mode, cooldown_seconds, last_fired_at, created_at, updated_at
            ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
            "#,
            rusqlite::params![
                t.id.to_string(),
                t.name,
                t.enabled as i32,
                t.trigger_type.as_str(),
                t.filter.to_string(),
                t.prompt_template,
                t.assigned_agent,
                t.execution_mode.as_ref().map(TriggerExecutionMode::as_str),
                t.cooldown_seconds as i64,
                t.last_fired_at.map(|d| d.to_rfc3339()),
                t.created_at.to_rfc3339(),
                t.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn update(&self, t: &EventTrigger) -> anyhow::Result<()> {
        self.conn.execute(
            r#"
            UPDATE event_triggers SET
                name=?1, enabled=?2, trigger_type=?3, filter_json=?4, prompt_template=?5,
                assigned_agent=?6, execution_mode=?7, cooldown_seconds=?8, last_fired_at=?9, updated_at=?10
            WHERE id=?11
            "#,
            rusqlite::params![
                t.name,
                t.enabled as i32,
                t.trigger_type.as_str(),
                t.filter.to_string(),
                t.prompt_template,
                t.assigned_agent,
                t.execution_mode.as_ref().map(TriggerExecutionMode::as_str),
                t.cooldown_seconds as i64,
                t.last_fired_at.map(|d| d.to_rfc3339()),
                t.updated_at.to_rfc3339(),
                t.id.to_string(),
            ],
        )?;
        Ok(())
    }

    pub fn delete(&self, id: Uuid) -> anyhow::Result<bool> {
        let n = self.conn.execute("DELETE FROM event_triggers WHERE id = ?1", [id.to_string()])?;
        Ok(n > 0)
    }

    pub fn get(&self, id: Uuid) -> anyhow::Result<Option<EventTrigger>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, name, enabled, trigger_type, filter_json, prompt_template,
                   assigned_agent, execution_mode, cooldown_seconds, last_fired_at, created_at, updated_at
            FROM event_triggers WHERE id = ?1
            "#,
        )?;
        let mut rows = stmt.query([id.to_string()])?;
        if let Some(row) = rows.next()? {
            return Ok(Some(row_to_trigger(row)?));
        }
        Ok(None)
    }

    pub fn list(&self) -> anyhow::Result<Vec<EventTrigger>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, name, enabled, trigger_type, filter_json, prompt_template,
                   assigned_agent, execution_mode, cooldown_seconds, last_fired_at, created_at, updated_at
            FROM event_triggers ORDER BY created_at DESC
            "#,
        )?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(row_to_trigger(row)?);
        }
        Ok(out)
    }

    pub fn list_enabled_by_type(&self, trigger_type: TriggerType) -> anyhow::Result<Vec<EventTrigger>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, name, enabled, trigger_type, filter_json, prompt_template,
                   assigned_agent, execution_mode, cooldown_seconds, last_fired_at, created_at, updated_at
            FROM event_triggers WHERE enabled = 1 AND trigger_type = ?1
            ORDER BY created_at ASC
            "#,
        )?;
        let mut rows = stmt.query([trigger_type.as_str()])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(row_to_trigger(row)?);
        }
        Ok(out)
    }

    pub fn set_last_fired(&self, id: Uuid, at: DateTime<Utc>) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE event_triggers SET last_fired_at = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![at.to_rfc3339(), at.to_rfc3339(), id.to_string()],
        )?;
        Ok(())
    }

    pub fn insert_run(&self, run: &EventTriggerRun) -> anyhow::Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO event_trigger_runs (id, trigger_id, task_id, fired_at, match_payload)
            VALUES (?1,?2,?3,?4,?5)
            "#,
            rusqlite::params![
                run.id.to_string(),
                run.trigger_id.to_string(),
                run.task_id.to_string(),
                run.fired_at.to_rfc3339(),
                run.match_payload,
            ],
        )?;
        Ok(())
    }

    pub fn list_runs(&self, trigger_id: Option<Uuid>, limit: usize) -> anyhow::Result<Vec<EventTriggerRun>> {
        let lim = limit.min(500) as i64;
        let mut out = Vec::new();
        if let Some(tid) = trigger_id {
            let mut stmt = self.conn.prepare(
                r#"
                SELECT id, trigger_id, task_id, fired_at, match_payload
                FROM event_trigger_runs WHERE trigger_id = ?1
                ORDER BY fired_at DESC LIMIT ?2
                "#,
            )?;
            let mut rows = stmt.query(rusqlite::params![tid.to_string(), lim])?;
            while let Some(row) = rows.next()? {
                out.push(row_to_run(row)?);
            }
        } else {
            let mut stmt = self.conn.prepare(
                r#"
                SELECT id, trigger_id, task_id, fired_at, match_payload
                FROM event_trigger_runs ORDER BY fired_at DESC LIMIT ?1
                "#,
            )?;
            let mut rows = stmt.query([lim])?;
            while let Some(row) = rows.next()? {
                out.push(row_to_run(row)?);
            }
        }
        Ok(out)
    }
}

fn row_to_trigger(row: &rusqlite::Row<'_>) -> anyhow::Result<EventTrigger> {
    let id: String = row.get(0)?;
    let filter_raw: String = row.get(4)?;
    let filter: serde_json::Value = serde_json::from_str(&filter_raw).unwrap_or(serde_json::json!({}));
    let exec_raw: Option<String> = row.get(7)?;
    Ok(EventTrigger {
        id: Uuid::parse_str(&id)?,
        name: row.get(1)?,
        enabled: row.get::<_, i32>(2)? != 0,
        trigger_type: TriggerType::from_str(&row.get::<_, String>(3)?),
        filter,
        prompt_template: row.get(5)?,
        assigned_agent: row.get(6)?,
        execution_mode: exec_raw
            .as_deref()
            .and_then(TriggerExecutionMode::from_str_opt),
        cooldown_seconds: row.get::<_, i64>(8)? as u64,
        last_fired_at: row
            .get::<_, Option<String>>(9)?
            .map(|s| DateTime::parse_from_rfc3339(&s).map(|d| d.with_timezone(&Utc)))
            .transpose()?,
        created_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(10)?)?.with_timezone(&Utc),
        updated_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(11)?)?.with_timezone(&Utc),
    })
}

fn row_to_run(row: &rusqlite::Row<'_>) -> anyhow::Result<EventTriggerRun> {
    Ok(EventTriggerRun {
        id: Uuid::parse_str(&row.get::<_, String>(0)?)?,
        trigger_id: Uuid::parse_str(&row.get::<_, String>(1)?)?,
        task_id: Uuid::parse_str(&row.get::<_, String>(2)?)?,
        fired_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(3)?)?.with_timezone(&Utc),
        match_payload: row.get(4)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn sample_trigger() -> EventTrigger {
        let now = Utc::now();
        EventTrigger {
            id: Uuid::new_v4(),
            name: "test".into(),
            enabled: true,
            trigger_type: TriggerType::Webhook,
            filter: serde_json::json!({}),
            prompt_template: "Hello {{payload.msg}}".into(),
            assigned_agent: None,
            execution_mode: None,
            cooldown_seconds: 60,
            last_fired_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn crud_roundtrip() {
        let file = NamedTempFile::new().unwrap();
        let store = EventTriggerStore::open(file.path()).unwrap();
        let t = sample_trigger();
        store.insert(&t).unwrap();
        let got = store.get(t.id).unwrap().unwrap();
        assert_eq!(got.name, "test");
        store.delete(t.id).unwrap();
        assert!(store.get(t.id).unwrap().is_none());
    }
}
