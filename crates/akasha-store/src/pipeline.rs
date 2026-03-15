//! Pipeline state for orchestrated mode (Plan: Architecture agents et pipeline — Phase 3).
//! States: Cadrage → Planification → Production → Vérification → Consolidation → Validation → Livraison.

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use std::path::Path;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(non_camel_case_types)]
pub enum PipelineState {
    Cadrage,
    Planification,
    Production,
    Verification,
    Consolidation,
    Validation,
    Livraison,
}

impl PipelineState {
    pub fn as_str(&self) -> &'static str {
        match self {
            PipelineState::Cadrage => "cadrage",
            PipelineState::Planification => "planification",
            PipelineState::Production => "production",
            PipelineState::Verification => "verification",
            PipelineState::Consolidation => "consolidation",
            PipelineState::Validation => "validation",
            PipelineState::Livraison => "livraison",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "cadrage" => Some(PipelineState::Cadrage),
            "planification" => Some(PipelineState::Planification),
            "production" => Some(PipelineState::Production),
            "verification" => Some(PipelineState::Verification),
            "consolidation" => Some(PipelineState::Consolidation),
            "validation" => Some(PipelineState::Validation),
            "livraison" => Some(PipelineState::Livraison),
            _ => None,
        }
    }

    pub fn next(self) -> Option<Self> {
        match self {
            PipelineState::Cadrage => Some(PipelineState::Planification),
            PipelineState::Planification => Some(PipelineState::Production),
            PipelineState::Production => Some(PipelineState::Verification),
            PipelineState::Verification => Some(PipelineState::Consolidation),
            PipelineState::Consolidation => Some(PipelineState::Validation),
            PipelineState::Validation => Some(PipelineState::Livraison),
            PipelineState::Livraison => None,
        }
    }
}

/// Persisted pipeline context for a root task in orchestrated mode.
#[derive(Debug, Clone)]
pub struct PipelineContext {
    pub root_task_id: Uuid,
    pub state: PipelineState,
    pub outputs_json: Option<String>,
    pub updated_at: DateTime<Utc>,
    /// Number of rework/retry attempts (Phase 5: escalation after threshold).
    pub attempt_count: u32,
    /// Checkpoint for resume: JSON with steps, last_subtask_index, aggregated_so_far (optional).
    pub checkpoint_json: Option<String>,
}

pub struct PipelineStore {
    conn: Connection,
}

impl PipelineStore {
    pub fn open<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS pipeline_context (
                root_task_id TEXT PRIMARY KEY,
                state TEXT NOT NULL,
                outputs_json TEXT,
                updated_at TEXT NOT NULL,
                attempt_count INTEGER NOT NULL DEFAULT 0
            );
            "#,
        )?;
        let has_col: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('pipeline_context') WHERE name='attempt_count'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if has_col == 0 {
            let _ = conn.execute("ALTER TABLE pipeline_context ADD COLUMN attempt_count INTEGER NOT NULL DEFAULT 0", []);
        }
        let has_ck: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('pipeline_context') WHERE name='checkpoint_json'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if has_ck == 0 {
            let _ = conn.execute("ALTER TABLE pipeline_context ADD COLUMN checkpoint_json TEXT", []);
        }
        Ok(Self { conn })
    }

    pub fn get(&self, root_task_id: Uuid) -> anyhow::Result<Option<PipelineContext>> {
        let mut stmt = self.conn.prepare(
            "SELECT root_task_id, state, outputs_json, updated_at, COALESCE(attempt_count, 0), checkpoint_json FROM pipeline_context WHERE root_task_id = ?1",
        )?;
        let mut rows = stmt.query([root_task_id.to_string()])?;
        if let Some(row) = rows.next()? {
            let state_str: String = row.get(1)?;
            let state = PipelineState::from_str(&state_str).unwrap_or(PipelineState::Cadrage);
            let updated_at: String = row.get(3)?;
            let updated_at = DateTime::parse_from_rfc3339(&updated_at).map(|d| d.with_timezone(&Utc)).unwrap_or_else(|_| Utc::now());
            let attempt_count: i32 = row.get(4).unwrap_or(0);
            let checkpoint_json: Option<String> = row.get(5).ok();
            return Ok(Some(PipelineContext {
                root_task_id,
                state,
                outputs_json: row.get(2)?,
                updated_at,
                attempt_count: attempt_count as u32,
                checkpoint_json,
            }));
        }
        Ok(None)
    }

    pub fn set_state(&self, root_task_id: Uuid, state: PipelineState, outputs_json: Option<&str>) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            r#"
            INSERT INTO pipeline_context (root_task_id, state, outputs_json, updated_at, attempt_count)
            VALUES (?1, ?2, ?3, ?4, 0)
            ON CONFLICT(root_task_id) DO UPDATE SET state = excluded.state, outputs_json = excluded.outputs_json, updated_at = excluded.updated_at
            "#,
            rusqlite::params![root_task_id.to_string(), state.as_str(), outputs_json, now],
        )?;
        Ok(())
    }

    pub fn increment_attempt(&self, root_task_id: Uuid) -> anyhow::Result<u32> {
        self.init_if_missing(root_task_id)?;
        self.conn.execute(
            "UPDATE pipeline_context SET attempt_count = COALESCE(attempt_count, 0) + 1, updated_at = ?1 WHERE root_task_id = ?2",
            rusqlite::params![Utc::now().to_rfc3339(), root_task_id.to_string()],
        )?;
        Ok(self.get(root_task_id)?.map(|c| c.attempt_count).unwrap_or(1))
    }

    pub fn init_if_missing(&self, root_task_id: Uuid) -> anyhow::Result<()> {
        if self.get(root_task_id)?.is_none() {
            self.set_state(root_task_id, PipelineState::Cadrage, None)?;
        }
        Ok(())
    }

    /// Save checkpoint for resume (steps, last_subtask_index, aggregated_so_far). Overwrites checkpoint_json.
    pub fn set_checkpoint(&self, root_task_id: Uuid, checkpoint_json: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE pipeline_context SET checkpoint_json = ?1, updated_at = ?2 WHERE root_task_id = ?3",
            rusqlite::params![checkpoint_json, Utc::now().to_rfc3339(), root_task_id.to_string()],
        )?;
        Ok(())
    }
}
