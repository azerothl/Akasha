//! Workspace knowledge graph (Graphify-style): nodes and edges for a user-selected directory.
//! Stored in the main SQLite DB alongside other Akasha data.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Provenance of an edge (aligned with spec/46: EXTRACTED vs INFERRED).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EdgeOrigin {
    /// From AST or explicit structure (markdown `#` headings).
    Extracted,
    /// Reserved for future LLM-derived edges.
    Inferred,
}

impl EdgeOrigin {
    pub fn as_str(&self) -> &'static str {
        match self {
            EdgeOrigin::Extracted => "extracted",
            EdgeOrigin::Inferred => "inferred",
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "inferred" => EdgeOrigin::Inferred,
            _ => EdgeOrigin::Extracted,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WgNode {
    pub id: String,
    pub kind: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WgEdge {
    pub from_id: String,
    pub to_id: String,
    pub kind: String,
    pub origin: EdgeOrigin,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WgBuildInfo {
    pub root_path: String,
    pub built_at: String,
    pub file_count: i64,
}

pub struct WorkspaceGraphStore {
    conn: Connection,
}

impl WorkspaceGraphStore {
    pub fn open<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS workspace_graph_build (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                root_path TEXT NOT NULL,
                built_at TEXT NOT NULL,
                file_count INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS workspace_graph_nodes (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                label TEXT NOT NULL,
                path TEXT,
                language TEXT
            );
            CREATE TABLE IF NOT EXISTS workspace_graph_edges (
                from_id TEXT NOT NULL,
                to_id TEXT NOT NULL,
                kind TEXT NOT NULL,
                origin TEXT NOT NULL DEFAULT 'extracted',
                confidence REAL,
                PRIMARY KEY (from_id, to_id, kind)
            );
            CREATE INDEX IF NOT EXISTS idx_wg_edges_from ON workspace_graph_edges(from_id);
            CREATE INDEX IF NOT EXISTS idx_wg_edges_to ON workspace_graph_edges(to_id);
            "#,
        )?;
        Ok(Self { conn })
    }

    pub fn clear(&self) -> anyhow::Result<()> {
        self.conn.execute_batch(
            "DELETE FROM workspace_graph_edges; DELETE FROM workspace_graph_nodes;",
        )?;
        Ok(())
    }

    pub fn set_build_info(&self, root_path: &str, built_at: &str, file_count: i64) -> anyhow::Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO workspace_graph_build (id, root_path, built_at, file_count)
            VALUES (1, ?1, ?2, ?3)
            ON CONFLICT(id) DO UPDATE SET root_path = ?1, built_at = ?2, file_count = ?3
            "#,
            rusqlite::params![root_path, built_at, file_count],
        )?;
        Ok(())
    }

    pub fn insert_node(&self, node: &WgNode) -> anyhow::Result<()> {
        self.conn.execute(
            r#"
            INSERT OR REPLACE INTO workspace_graph_nodes (id, kind, label, path, language)
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
            rusqlite::params![
                node.id,
                node.kind,
                node.label,
                node.path,
                node.language,
            ],
        )?;
        Ok(())
    }

    pub fn insert_edge(&self, edge: &WgEdge) -> anyhow::Result<()> {
        self.conn.execute(
            r#"
            INSERT OR REPLACE INTO workspace_graph_edges (from_id, to_id, kind, origin, confidence)
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
            rusqlite::params![
                edge.from_id,
                edge.to_id,
                edge.kind,
                edge.origin.as_str(),
                edge.confidence,
            ],
        )?;
        Ok(())
    }

    pub fn get_build_info(&self) -> anyhow::Result<Option<WgBuildInfo>> {
        let mut stmt = self.conn.prepare(
            "SELECT root_path, built_at, file_count FROM workspace_graph_build WHERE id = 1",
        )?;
        let mut rows = stmt.query_map([], |row| {
            Ok(WgBuildInfo {
                root_path: row.get(0)?,
                built_at: row.get(1)?,
                file_count: row.get(2)?,
            })
        })?;
        Ok(rows.next().transpose()?)
    }

    pub fn export_graph(&self) -> anyhow::Result<(Vec<WgNode>, Vec<WgEdge>)> {
        let mut nstmt = self.conn.prepare(
            "SELECT id, kind, label, path, language FROM workspace_graph_nodes ORDER BY id",
        )?;
        let nodes: Vec<WgNode> = nstmt
            .query_map([], |row| {
                Ok(WgNode {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    label: row.get(2)?,
                    path: row.get(3)?,
                    language: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut estmt = self
            .conn
            .prepare("SELECT from_id, to_id, kind, origin, confidence FROM workspace_graph_edges")?;
        let edges: Vec<WgEdge> = estmt
            .query_map([], |row| {
                let origin_s: String = row.get(3)?;
                Ok(WgEdge {
                    from_id: row.get(0)?,
                    to_id: row.get(1)?,
                    kind: row.get(2)?,
                    origin: EdgeOrigin::from_str(&origin_s),
                    confidence: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok((nodes, edges))
    }

    pub fn stats(&self) -> anyhow::Result<(i64, i64)> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM workspace_graph_nodes", [], |r| r.get(0))?;
        let e: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM workspace_graph_edges", [], |r| r.get(0))?;
        Ok((n, e))
    }
}
