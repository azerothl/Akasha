//! Episodic memory store (Phase 2 — Mémoire 4 couches): journal of significant events.

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use std::path::Path;
use uuid::Uuid;

/// One event in the episodic store.
#[derive(Debug, Clone)]
pub struct EpisodicEvent {
    pub id: Uuid,
    pub event_type: String,
    pub payload: String,
    pub created_at: DateTime<Utc>,
    pub entity_id: Option<String>,
    pub process_id: Option<String>,
    pub session_id: Option<String>,
    pub task_id: Option<String>,
    pub importance: Option<i64>,
    pub scope: Option<String>,
    pub tags: Option<String>,
}

pub struct EpisodicStore {
    conn: Connection,
}

/// Filter for querying episodic events.
#[derive(Debug, Clone, Default)]
pub struct EpisodicFilter {
    pub entity_id: Option<String>,
    pub process_id: Option<String>,
    pub session_id: Option<String>,
    pub scope: Option<String>,
    pub event_type: Option<String>,
    pub min_importance: Option<i64>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
}

impl EpisodicStore {
    /// Open the episodic store. Uses the same DB path as long-term memory; creates episodic_events table if missing.
    pub fn open<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS episodic_events (
                id TEXT PRIMARY KEY,
                event_type TEXT NOT NULL,
                payload TEXT NOT NULL,
                created_at TEXT NOT NULL,
                entity_id TEXT,
                process_id TEXT,
                session_id TEXT,
                task_id TEXT,
                importance INTEGER,
                scope TEXT,
                tags TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_episodic_events_created ON episodic_events(created_at);
            CREATE INDEX IF NOT EXISTS idx_episodic_events_entity ON episodic_events(entity_id);
            CREATE INDEX IF NOT EXISTS idx_episodic_events_session ON episodic_events(session_id);
            CREATE INDEX IF NOT EXISTS idx_episodic_events_type ON episodic_events(event_type);
            "#,
        )?;
        Ok(Self { conn })
    }

    pub fn insert_event(
        &self,
        event_type: &str,
        payload: &str,
        entity_id: Option<&str>,
        process_id: Option<&str>,
        session_id: Option<&str>,
        task_id: Option<&str>,
        importance: Option<i64>,
        scope: Option<&str>,
        tags: Option<&str>,
    ) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        let now = Utc::now();
        self.conn.execute(
            r#"
            INSERT INTO episodic_events (id, event_type, payload, created_at, entity_id, process_id, session_id, task_id, importance, scope, tags)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            "#,
            rusqlite::params![
                id.to_string(),
                event_type,
                payload,
                now.to_rfc3339(),
                entity_id,
                process_id,
                session_id,
                task_id,
                importance,
                scope,
                tags,
            ],
        )?;
        Ok(id)
    }

    /// Get events matching the filter, ordered by created_at desc, limited.
    pub fn get_events_filtered(
        &self,
        filter: &EpisodicFilter,
        limit: usize,
    ) -> anyhow::Result<Vec<EpisodicEvent>> {
        let mut conditions = Vec::new();
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(ref e) = filter.entity_id {
            conditions.push("entity_id = ?");
            params.push(Box::new(e.clone()));
        }
        if let Some(ref p) = filter.process_id {
            conditions.push("process_id = ?");
            params.push(Box::new(p.clone()));
        }
        if let Some(ref s) = filter.session_id {
            conditions.push("session_id = ?");
            params.push(Box::new(s.clone()));
        }
        if let Some(ref sc) = filter.scope {
            conditions.push("scope = ?");
            params.push(Box::new(sc.clone()));
        }
        if let Some(ref t) = filter.event_type {
            conditions.push("event_type = ?");
            params.push(Box::new(t.clone()));
        }
        if let Some(mi) = filter.min_importance {
            conditions.push("COALESCE(importance, 0) >= ?");
            params.push(Box::new(mi));
        }
        if let Some(ref from) = filter.from {
            conditions.push("created_at >= ?");
            params.push(Box::new(from.to_rfc3339()));
        }
        if let Some(ref to) = filter.to {
            conditions.push("created_at <= ?");
            params.push(Box::new(to.to_rfc3339()));
        }

        let where_clause = if conditions.is_empty() {
            "".to_string()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };

        let sql = format!(
            "SELECT id, event_type, payload, created_at, entity_id, process_id, session_id, task_id, importance, scope, tags FROM episodic_events {} ORDER BY created_at DESC LIMIT ?",
            where_clause
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let limit_i = limit as i64;
        let mut param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();
        param_refs.push(&limit_i);

        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            let created_at_s: String = row.get(3)?;
            let created_at = DateTime::parse_from_rfc3339(&created_at_s)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            Ok(EpisodicEvent {
                id: Uuid::parse_str(&row.get::<_, String>(0)?).unwrap_or_else(|_| Uuid::nil()),
                event_type: row.get(1)?,
                payload: row.get(2)?,
                created_at,
                entity_id: row.get(4)?,
                process_id: row.get(5)?,
                session_id: row.get(6)?,
                task_id: row.get(7)?,
                importance: row.get(8)?,
                scope: row.get(9)?,
                tags: row.get(10)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Get the N most recent events, optionally filtered by scope.
    pub fn get_recent_events(
        &self,
        limit: usize,
        scope: Option<&str>,
    ) -> anyhow::Result<Vec<EpisodicEvent>> {
        let filter = EpisodicFilter {
            scope: scope.map(String::from),
            ..Default::default()
        };
        self.get_events_filtered(&filter, limit)
    }
}
