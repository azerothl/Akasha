//! Multi-workspace project knowledge graphs (Graphify-style).
//! Each workspace has a name, root path, nodes/edges scoped by `workspace_id`.

use crate::memory_fusion::{reciprocal_rank_fusion, DEFAULT_RRF_K};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Provenance of an edge (aligned with spec/46: EXTRACTED vs INFERRED).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EdgeOrigin {
    Extracted,
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
pub struct WgWorkspace {
    pub id: String,
    pub name: String,
    pub root_path: String,
    pub created_at: String,
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
    pub workspace_id: String,
    pub root_path: String,
    pub built_at: String,
    pub file_count: i64,
}

pub struct WorkspaceGraphStore {
    conn: Connection,
}

fn table_has_column(conn: &Connection, table: &str, col: &str) -> anyhow::Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", table))?;
    let rows = stmt.query_map([], |row| {
        let name: String = row.get(1)?;
        Ok(name)
    })?;
    for r in rows {
        if r? == col {
            return Ok(true);
        }
    }
    Ok(false)
}

fn migrate_legacy_if_needed(conn: &Connection) -> anyhow::Result<()> {
    let nodes_exist = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='workspace_graph_nodes'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
        > 0;
    if !nodes_exist {
        return Ok(());
    }
    if table_has_column(conn, "workspace_graph_nodes", "workspace_id")? {
        return Ok(());
    }

    // Legacy single-workspace schema → one workspace "Default"
    let wid = uuid::Uuid::new_v4().to_string();
    let (root_path, built_at, file_count): (String, String, i64) = conn
        .query_row(
            "SELECT root_path, built_at, file_count FROM workspace_graph_build WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap_or_else(|_| {
            (
                String::new(),
                chrono::Utc::now().to_rfc3339(),
                0_i64,
            )
        });

    conn.execute("DROP TABLE IF EXISTS workspace_graph_workspaces", [])?;
    conn.execute(
        r#"
        CREATE TABLE workspace_graph_workspaces (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            root_path TEXT NOT NULL,
            created_at TEXT NOT NULL
        )
        "#,
        [],
    )?;
    conn.execute(
        "INSERT INTO workspace_graph_workspaces (id, name, root_path, created_at) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![wid, "Default", root_path, chrono::Utc::now().to_rfc3339()],
    )?;

    conn.execute(
        r#"
        CREATE TABLE workspace_graph_build_new (
            workspace_id TEXT PRIMARY KEY REFERENCES workspace_graph_workspaces(id) ON DELETE CASCADE,
            root_path TEXT NOT NULL,
            built_at TEXT NOT NULL,
            file_count INTEGER NOT NULL DEFAULT 0
        )
        "#,
        [],
    )?;
    conn.execute(
        "INSERT INTO workspace_graph_build_new (workspace_id, root_path, built_at, file_count) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![wid, root_path, built_at, file_count],
    )?;
    conn.execute("DROP TABLE IF EXISTS workspace_graph_build", [])?;
    conn.execute(
        "ALTER TABLE workspace_graph_build_new RENAME TO workspace_graph_build",
        [],
    )?;

    conn.execute(
        r#"
        CREATE TABLE workspace_graph_nodes_new (
            workspace_id TEXT NOT NULL REFERENCES workspace_graph_workspaces(id) ON DELETE CASCADE,
            id TEXT NOT NULL,
            kind TEXT NOT NULL,
            label TEXT NOT NULL,
            path TEXT,
            language TEXT,
            PRIMARY KEY (workspace_id, id)
        )
        "#,
        [],
    )?;
    conn.execute(
        "INSERT INTO workspace_graph_nodes_new (workspace_id, id, kind, label, path, language) SELECT ?1, id, kind, label, path, language FROM workspace_graph_nodes",
        [&wid],
    )?;
    conn.execute("DROP TABLE workspace_graph_nodes", [])?;
    conn.execute(
        "ALTER TABLE workspace_graph_nodes_new RENAME TO workspace_graph_nodes",
        [],
    )?;

    conn.execute(
        r#"
        CREATE TABLE workspace_graph_edges_new (
            workspace_id TEXT NOT NULL REFERENCES workspace_graph_workspaces(id) ON DELETE CASCADE,
            from_id TEXT NOT NULL,
            to_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            origin TEXT NOT NULL DEFAULT 'extracted',
            confidence REAL,
            PRIMARY KEY (workspace_id, from_id, to_id, kind)
        )
        "#,
        [],
    )?;
    conn.execute(
        "INSERT INTO workspace_graph_edges_new (workspace_id, from_id, to_id, kind, origin, confidence) SELECT ?1, from_id, to_id, kind, origin, confidence FROM workspace_graph_edges",
        [&wid],
    )?;
    conn.execute("DROP TABLE workspace_graph_edges", [])?;
    conn.execute(
        "ALTER TABLE workspace_graph_edges_new RENAME TO workspace_graph_edges",
        [],
    )?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_wg_edges_from ON workspace_graph_edges(workspace_id, from_id)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_wg_edges_to ON workspace_graph_edges(workspace_id, to_id)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_wg_nodes_ws ON workspace_graph_nodes(workspace_id)",
        [],
    )?;

    Ok(())
}

impl WorkspaceGraphStore {
    pub fn open<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;

        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS workspace_graph_workspaces (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                root_path TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            "#,
        )?;

        let nodes_exist = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='workspace_graph_nodes'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0)
            > 0;

        if !nodes_exist {
            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS workspace_graph_build (
                    workspace_id TEXT PRIMARY KEY REFERENCES workspace_graph_workspaces(id) ON DELETE CASCADE,
                    root_path TEXT NOT NULL,
                    built_at TEXT NOT NULL,
                    file_count INTEGER NOT NULL DEFAULT 0
                );
                CREATE TABLE IF NOT EXISTS workspace_graph_nodes (
                    workspace_id TEXT NOT NULL REFERENCES workspace_graph_workspaces(id) ON DELETE CASCADE,
                    id TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    label TEXT NOT NULL,
                    path TEXT,
                    language TEXT,
                    PRIMARY KEY (workspace_id, id)
                );
                CREATE TABLE IF NOT EXISTS workspace_graph_edges (
                    workspace_id TEXT NOT NULL REFERENCES workspace_graph_workspaces(id) ON DELETE CASCADE,
                    from_id TEXT NOT NULL,
                    to_id TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    origin TEXT NOT NULL DEFAULT 'extracted',
                    confidence REAL,
                    PRIMARY KEY (workspace_id, from_id, to_id, kind)
                );
                CREATE INDEX IF NOT EXISTS idx_wg_edges_from ON workspace_graph_edges(workspace_id, from_id);
                CREATE INDEX IF NOT EXISTS idx_wg_edges_to ON workspace_graph_edges(workspace_id, to_id);
                CREATE INDEX IF NOT EXISTS idx_wg_nodes_ws ON workspace_graph_nodes(workspace_id);
                "#,
            )?;
        } else {
            migrate_legacy_if_needed(&conn)?;
            // Ensure indexes exist post-migration
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_wg_edges_from ON workspace_graph_edges(workspace_id, from_id)",
                [],
            )?;
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_wg_edges_to ON workspace_graph_edges(workspace_id, to_id)",
                [],
            )?;
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_wg_nodes_ws ON workspace_graph_nodes(workspace_id)",
                [],
            )?;
        }

        Ok(Self { conn })
    }

    pub fn list_workspaces(&self) -> anyhow::Result<Vec<WgWorkspace>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, root_path, created_at FROM workspace_graph_workspaces ORDER BY name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(WgWorkspace {
                id: row.get(0)?,
                name: row.get(1)?,
                root_path: row.get(2)?,
                created_at: row.get(3)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_workspace(&self, id: &str) -> anyhow::Result<Option<WgWorkspace>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, root_path, created_at FROM workspace_graph_workspaces WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![id], |row| {
            Ok(WgWorkspace {
                id: row.get(0)?,
                name: row.get(1)?,
                root_path: row.get(2)?,
                created_at: row.get(3)?,
            })
        })?;
        Ok(rows.next().transpose()?)
    }

    /// Create workspace. Returns new id. `name` must be unique.
    pub fn create_workspace(&self, name: &str, root_path: &str) -> anyhow::Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO workspace_graph_workspaces (id, name, root_path, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![id, name.trim(), root_path, now],
        )?;
        Ok(id)
    }

    pub fn update_workspace_root(&self, id: &str, root_path: &str) -> anyhow::Result<bool> {
        let n = self.conn.execute(
            "UPDATE workspace_graph_workspaces SET root_path = ?1 WHERE id = ?2",
            rusqlite::params![root_path, id],
        )?;
        Ok(n > 0)
    }

    pub fn workspace_name_exists(&self, name: &str, except_id: Option<&str>) -> anyhow::Result<bool> {
        let count: i64 = match except_id {
            Some(eid) => self.conn.query_row(
                "SELECT COUNT(*) FROM workspace_graph_workspaces WHERE name = ?1 AND id != ?2",
                rusqlite::params![name.trim(), eid],
                |r| r.get(0),
            )?,
            None => self.conn.query_row(
                "SELECT COUNT(*) FROM workspace_graph_workspaces WHERE name = ?1",
                rusqlite::params![name.trim()],
                |r| r.get(0),
            )?,
        };
        Ok(count > 0)
    }

    /// Deletes workspace row; CASCADE removes build, nodes, edges.
    pub fn delete_workspace(&self, id: &str) -> anyhow::Result<bool> {
        let n = self
            .conn
            .execute("DELETE FROM workspace_graph_workspaces WHERE id = ?1", [id])?;
        Ok(n > 0)
    }

    /// Remove all graph data for one workspace (nodes, edges, build meta). Keeps workspace row and root_path.
    pub fn clear_workspace_graph(&self, workspace_id: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "DELETE FROM workspace_graph_edges WHERE workspace_id = ?1",
            [workspace_id],
        )?;
        self.conn.execute(
            "DELETE FROM workspace_graph_nodes WHERE workspace_id = ?1",
            [workspace_id],
        )?;
        self.conn.execute(
            "DELETE FROM workspace_graph_build WHERE workspace_id = ?1",
            [workspace_id],
        )?;
        Ok(())
    }

    pub fn set_build_info(
        &self,
        workspace_id: &str,
        root_path: &str,
        built_at: &str,
        file_count: i64,
    ) -> anyhow::Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO workspace_graph_build (workspace_id, root_path, built_at, file_count)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(workspace_id) DO UPDATE SET root_path = ?2, built_at = ?3, file_count = ?4
            "#,
            rusqlite::params![workspace_id, root_path, built_at, file_count],
        )?;
        Ok(())
    }

    pub fn insert_node(&self, workspace_id: &str, node: &WgNode) -> anyhow::Result<()> {
        self.conn.execute(
            r#"
            INSERT OR REPLACE INTO workspace_graph_nodes (workspace_id, id, kind, label, path, language)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
            rusqlite::params![
                workspace_id,
                node.id,
                node.kind,
                node.label,
                node.path,
                node.language,
            ],
        )?;
        Ok(())
    }

    pub fn insert_edge(&self, workspace_id: &str, edge: &WgEdge) -> anyhow::Result<()> {
        self.conn.execute(
            r#"
            INSERT OR REPLACE INTO workspace_graph_edges (workspace_id, from_id, to_id, kind, origin, confidence)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
            rusqlite::params![
                workspace_id,
                edge.from_id,
                edge.to_id,
                edge.kind,
                edge.origin.as_str(),
                edge.confidence,
            ],
        )?;
        Ok(())
    }

    pub fn get_build_info(&self, workspace_id: &str) -> anyhow::Result<Option<WgBuildInfo>> {
        let mut stmt = self.conn.prepare(
            "SELECT workspace_id, root_path, built_at, file_count FROM workspace_graph_build WHERE workspace_id = ?1",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![workspace_id], |row| {
            Ok(WgBuildInfo {
                workspace_id: row.get(0)?,
                root_path: row.get(1)?,
                built_at: row.get(2)?,
                file_count: row.get(3)?,
            })
        })?;
        Ok(rows.next().transpose()?)
    }

    pub fn export_graph(&self, workspace_id: &str) -> anyhow::Result<(Vec<WgNode>, Vec<WgEdge>)> {
        let mut nstmt = self.conn.prepare(
            "SELECT id, kind, label, path, language FROM workspace_graph_nodes WHERE workspace_id = ?1 ORDER BY id",
        )?;
        let nodes: Vec<WgNode> = nstmt
            .query_map(rusqlite::params![workspace_id], |row| {
                Ok(WgNode {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    label: row.get(2)?,
                    path: row.get(3)?,
                    language: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut estmt = self.conn.prepare(
            "SELECT from_id, to_id, kind, origin, confidence FROM workspace_graph_edges WHERE workspace_id = ?1",
        )?;
        let edges: Vec<WgEdge> = estmt
            .query_map(rusqlite::params![workspace_id], |row| {
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

    pub fn stats(&self, workspace_id: &str) -> anyhow::Result<(i64, i64)> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM workspace_graph_nodes WHERE workspace_id = ?1",
            [workspace_id],
            |r| r.get(0),
        )?;
        let e: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM workspace_graph_edges WHERE workspace_id = ?1",
            [workspace_id],
            |r| r.get(0),
        )?;
        Ok((n, e))
    }

    /// Global stats (all workspaces).
    pub fn stats_all(&self) -> anyhow::Result<(i64, i64)> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM workspace_graph_nodes", [], |r| r.get(0))?;
        let e: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM workspace_graph_edges", [], |r| r.get(0))?;
        Ok((n, e))
    }

    /// Lexical search across workspaces for agent RAG-style injection.
    /// `workspace_id` None = all workspaces. Returns short lines for the prompt.
    pub fn search_graph_context(
        &self,
        query: &str,
        limit: usize,
        workspace_id: Option<&str>,
    ) -> anyhow::Result<Vec<String>> {
        let terms: Vec<String> = query
            .to_lowercase()
            .split_whitespace()
            .filter(|s| s.len() > 1)
            .map(String::from)
            .collect();
        if limit == 0 {
            return Ok(Vec::new());
        }

        let mut label_hits: Vec<(String, f32)> = Vec::new();
        let mut context_hits: Vec<(String, f32)> = Vec::new();
        let mut line_by_id: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        let mut hit_samples: Vec<serde_json::Value> = Vec::new();
        let workspaces = if let Some(wid) = workspace_id {
            self.get_workspace(wid)?.into_iter().collect::<Vec<_>>()
        } else {
            self.list_workspaces()?
        };
        let workspace_count = workspaces.len();

        for ws in workspaces {
            if label_hits.len() + context_hits.len() >= limit.saturating_mul(4) {
                break;
            }
            let mut stmt = self.conn.prepare(
                "SELECT id, kind, label, path FROM workspace_graph_nodes WHERE workspace_id = ?1",
            )?;
            let rows = stmt.query_map(rusqlite::params![ws.id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })?;

            for row in rows {
                let (id, kind, label, path) = row?;
                let label_l = label.to_lowercase();
                let path_l = path.as_deref().unwrap_or("").to_lowercase();
                let kind_l = kind.to_lowercase();
                let ws_name_l = ws.name.to_lowercase();
                let ws_root_l = ws.root_path.to_lowercase();
                let p = path.as_deref().unwrap_or("");
                let line = format!(
                    "[Workspace \"{}\"] {} — {} ({})",
                    ws.name, kind, label, p
                );
                let node_key = format!("{}:{}", ws.id, id);
                if terms.is_empty() {
                    label_hits.push((node_key.clone(), 1.0));
                    line_by_id.insert(node_key, line);
                    continue;
                }
                let label_match = terms.iter().any(|t| label_l.contains(t.as_str()));
                let path_match = terms.iter().any(|t| path_l.contains(t.as_str()) || kind_l.contains(t.as_str()));
                let context_match = terms.iter().any(|t| {
                    ws_name_l.contains(t.as_str()) || ws_root_l.contains(t.as_str()) || id.to_lowercase().contains(t.as_str())
                });
                if !(label_match || path_match || context_match) {
                    continue;
                }
                line_by_id.insert(node_key.clone(), line);
                if hit_samples.len() < 5 {
                    let matched_terms: Vec<String> = terms
                        .iter()
                        .filter(|t| {
                            label_l.contains(t.as_str())
                                || path_l.contains(t.as_str())
                                || kind_l.contains(t.as_str())
                                || ws_name_l.contains(t.as_str())
                        })
                        .cloned()
                        .collect();
                    hit_samples.push(serde_json::json!({
                        "workspace": ws.name.as_str(),
                        "kind": kind.as_str(),
                        "label": label.as_str(),
                        "path": p,
                        "matched_terms": matched_terms
                    }));
                }
                if label_match {
                    let score = terms.iter().filter(|t| label_l.contains(t.as_str())).count() as f32;
                    label_hits.push((node_key.clone(), score));
                }
                if path_match || context_match {
                    let score = terms
                        .iter()
                        .filter(|t| {
                            path_l.contains(t.as_str())
                                || kind_l.contains(t.as_str())
                                || ws_name_l.contains(t.as_str())
                                || ws_root_l.contains(t.as_str())
                        })
                        .count() as f32;
                    context_hits.push((node_key, score));
                }
            }
        }

        let ranked: Vec<(String, f32)> = if label_hits.is_empty() && context_hits.is_empty() {
            Vec::new()
        } else if label_hits.is_empty() {
            let mut v = context_hits;
            v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            v
        } else if context_hits.is_empty() {
            let mut v = label_hits;
            v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            v
        } else {
            reciprocal_rank_fusion(&[label_hits, context_hits], DEFAULT_RRF_K)
        };
        let out: Vec<String> = ranked
            .into_iter()
            .take(limit)
            .filter_map(|(key, _)| line_by_id.get(&key).cloned())
            .collect();

        tracing::debug!(
            query = %query,
            terms = ?terms,
            limit = limit,
            workspace_filter = ?workspace_id,
            workspace_count = workspace_count,
            out_count = out.len(),
            hit_samples = ?hit_samples,
            "graph lexical search diagnostics"
        );

        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn open_empty_store() -> (WorkspaceGraphStore, NamedTempFile) {
        let f = NamedTempFile::new().unwrap();
        let s = WorkspaceGraphStore::open(f.path()).unwrap();
        (s, f)
    }

    #[test]
    fn workspace_isolation_and_search() {
        let (s, _tmp) = open_empty_store();
        let a = s.create_workspace("Alpha", "/tmp/a").unwrap();
        let b = s.create_workspace("Beta", "/tmp/b").unwrap();
        s.insert_node(
            &a,
            &WgNode {
                id: "n1".into(),
                kind: "symbol".into(),
                label: "frobnicate".into(),
                path: Some("src/lib.rs".into()),
                language: Some("rust".into()),
            },
        )
        .unwrap();
        s.insert_node(
            &b,
            &WgNode {
                id: "n2".into(),
                kind: "file".into(),
                label: "other.md".into(),
                path: Some("docs/other.md".into()),
                language: Some("md".into()),
            },
        )
        .unwrap();

        let all = s.search_graph_context("frobnicate", 10, None).unwrap();
        assert_eq!(all.len(), 1);
        assert!(all[0].contains("Alpha"));

        let scoped = s.search_graph_context("other", 10, Some(&a)).unwrap();
        assert!(scoped.is_empty());

        // Query terms match workspace name / root path, not only node labels.
        let by_name = s.search_graph_context("Beta", 10, None).unwrap();
        assert_eq!(by_name.len(), 1);
        assert!(by_name[0].contains("Beta"));

        assert!(s.delete_workspace(&a).unwrap());
        let after = s.search_graph_context("frobnicate", 10, None).unwrap();
        assert!(after.is_empty());
    }
}
