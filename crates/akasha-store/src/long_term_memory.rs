//! Long-term memory store (spec 06): persistent entries with embeddings for similarity search.

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::Path;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: Uuid,
    pub content: String,
    /// Embedding as little-endian f32 bytes (e.g. from akasha_embeddings::embedding_to_bytes).
    #[serde(skip_serializing)]
    pub embedding: Vec<u8>,
    pub created_at: DateTime<Utc>,
    pub source: String, // e.g. "compaction", "promote", "explicit"
}

/// Cosine similarity between two vectors (result in [-1, 1]). Public for hybrid rerank in daemon.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a <= 0.0 || norm_b <= 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

/// Decode embedding bytes (little-endian f32) to vector. Public for hybrid rerank in daemon.
pub fn decode_embedding_bytes(b: &[u8]) -> Vec<f32> {
    let n = b.len() / 4;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let mut bytes = [0u8; 4];
        bytes.copy_from_slice(&b[i * 4..(i + 1) * 4]);
        out.push(f32::from_le_bytes(bytes));
    }
    out
}

pub struct LongTermStore {
    conn: Connection,
}

/// Optional attribution filter for search (plan: court terme 3).
#[derive(Debug, Clone, Default)]
pub struct MemorySearchFilter {
    pub entity_id: Option<String>,
    pub process_id: Option<String>,
    pub session_id: Option<String>,
}

impl LongTermStore {
    pub fn open<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS memory_entries (
                id TEXT PRIMARY KEY,
                content TEXT NOT NULL,
                embedding BLOB NOT NULL,
                created_at TEXT NOT NULL,
                source TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_memory_entries_created ON memory_entries(created_at);
            "#,
        )?;
        // Migration: add attribution columns if missing (plan court terme 3).
        let has_col = |name: &str| -> anyhow::Result<bool> {
            let mut stmt = conn.prepare("SELECT name FROM pragma_table_info('memory_entries') WHERE name = ?1")?;
            let exists: bool = stmt.exists(rusqlite::params![name])?;
            Ok(exists)
        };
        for col in ["entity_id", "process_id", "session_id", "title", "tags"] {
            if !has_col(col)? {
                conn.execute(&format!("ALTER TABLE memory_entries ADD COLUMN {} TEXT", col), [])?;
            }
        }
        Ok(Self { conn })
    }

    pub fn insert(
        &self,
        content: &str,
        embedding: &[u8],
        source: &str,
    ) -> anyhow::Result<Uuid> {
        self.insert_with_attribution(content, embedding, source, None, None, None, None, None)
    }

    /// Insert with optional attribution and metadata (plan long terme 4: title, tags).
    pub fn insert_with_attribution(
        &self,
        content: &str,
        embedding: &[u8],
        source: &str,
        entity_id: Option<&str>,
        process_id: Option<&str>,
        session_id: Option<&str>,
        title: Option<&str>,
        tags: Option<&str>,
    ) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        let now = Utc::now();
        self.conn.execute(
            r#"
            INSERT INTO memory_entries (id, content, embedding, created_at, source, entity_id, process_id, session_id, title, tags)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            "#,
            rusqlite::params![
                id.to_string(),
                content,
                embedding,
                now.to_rfc3339(),
                source,
                entity_id,
                process_id,
                session_id,
                title,
                tags,
            ],
        )?;
        Ok(id)
    }

    /// Delete an entry by id. Returns true if a row was deleted.
    pub fn delete_by_id(&self, id: Uuid) -> anyhow::Result<bool> {
        let n = self.conn.execute("DELETE FROM memory_entries WHERE id = ?1", rusqlite::params![id.to_string()])?;
        Ok(n > 0)
    }

    /// Delete entries matching a keyword query (same logic as search_by_keywords). Returns number of deleted rows (plan moyen terme 9).
    pub fn delete_by_keywords(&self, query: &str) -> anyhow::Result<u64> {
        let words: Vec<String> = query
            .split_whitespace()
            .map(|s| s.trim())
            .filter(|s| s.len() >= 2)
            .map(|s| {
                s.replace('%', "\\%")
                    .replace('_', "\\_")
                    .replace('\\', "\\\\")
            })
            .collect();
        if words.is_empty() {
            return Ok(0);
        }
        let mut sql = String::from("DELETE FROM memory_entries WHERE ");
        for (i, _) in words.iter().enumerate() {
            if i > 0 {
                sql.push_str(" AND ");
            }
            sql.push_str("content LIKE ? ESCAPE '\\'");
        }
        let patterns: Vec<String> = words.iter().map(|w| format!("%{}%", w)).collect();
        let param_refs: Vec<&dyn rusqlite::ToSql> = patterns.iter().map(|p| p as &dyn rusqlite::ToSql).collect();
        let n = self.conn.execute(&sql, param_refs.as_slice())?;
        Ok(n as u64)
    }

    /// Return (entry_count, approximate_size_bytes) for the memory DB (plan moyen terme 9).
    pub fn stats(&self) -> anyhow::Result<(u64, u64)> {
        let count: u64 = self.conn.query_row(
            "SELECT COUNT(*) FROM memory_entries",
            [],
            |row| row.get(0),
        )?;
        let size: Option<i64> = self.conn.query_row(
            "SELECT SUM(LENGTH(content) + LENGTH(embedding) + LENGTH(created_at) + LENGTH(source)) FROM memory_entries",
            [],
            |row| row.get(0),
        )?;
        Ok((count, size.unwrap_or(0) as u64))
    }

    /// Delete entries older than retention_days. If protect_sources is Some(list), never delete rows with source in list. Returns deleted count (plan moyen terme 9).
    pub fn gc(&self, retention_days: u32, protect_sources: Option<&[String]>) -> anyhow::Result<u64> {
        let cutoff = Utc::now() - chrono::Duration::days(retention_days as i64);
        let cutoff_s = cutoff.to_rfc3339();
        let n = if let Some(protect) = protect_sources {
            if protect.is_empty() {
                self.conn.execute("DELETE FROM memory_entries WHERE created_at < ?1", rusqlite::params![cutoff_s])?
            } else {
                let placeholders = protect.iter().enumerate().map(|(i, _)| format!("?{}", i + 2)).collect::<Vec<_>>().join(", ");
                let sql = format!("DELETE FROM memory_entries WHERE created_at < ?1 AND source NOT IN ({})", placeholders);
                let mut params: Vec<&dyn rusqlite::ToSql> = vec![&cutoff_s];
                for s in protect.iter() {
                    params.push(s);
                }
                self.conn.execute(&sql, params.as_slice())?
            }
        } else {
            self.conn.execute("DELETE FROM memory_entries WHERE created_at < ?1", rusqlite::params![cutoff_s])?
        };
        Ok(n as u64)
    }

    /// Return true if an entry with the exact same content already exists.
    pub fn content_exists(&self, content: &str) -> anyhow::Result<bool> {
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM memory_entries WHERE content = ?1)",
            rusqlite::params![content],
            |row| row.get(0),
        )?;
        Ok(exists)
    }

    /// Return true if a daily summary (source "daily_summary") already exists for the given date (YYYY-MM-DD).
    /// Content format is "Résumé du {date} : ...".
    pub fn has_daily_summary_for_date(&self, date: &str) -> anyhow::Result<bool> {
        let pattern = format!("Résumé du {} :%", date);
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM memory_entries WHERE source = 'daily_summary' AND content LIKE ?1)",
            rusqlite::params![pattern],
            |row| row.get(0),
        )?;
        Ok(exists)
    }

    /// Retrieve all entries with their embeddings for similarity search in memory.
    /// When filter is provided, only rows matching entity_id/process_id/session_id (or NULL) are returned.
    fn get_all_with_embedding(
        &self,
        filter: Option<&MemorySearchFilter>,
    ) -> anyhow::Result<Vec<(String, String, Vec<u8>, String, String)>> {
        let (where_clause, params) = Self::filter_clause(filter);
        let sql = format!(
            "SELECT id, content, embedding, created_at, source FROM memory_entries {} ORDER BY created_at",
            where_clause
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();
        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Vec<u8>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn filter_clause(filter: Option<&MemorySearchFilter>) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        let mut conditions = Vec::new();
        if let Some(f) = filter {
            if let Some(ref e) = f.entity_id {
                conditions.push("(entity_id IS NULL OR entity_id = ?)");
                params.push(Box::new(e.clone()));
            }
            if let Some(ref p) = f.process_id {
                conditions.push("(process_id IS NULL OR process_id = ?)");
                params.push(Box::new(p.clone()));
            }
            if let Some(ref s) = f.session_id {
                conditions.push("(session_id IS NULL OR session_id = ?)");
                params.push(Box::new(s.clone()));
            }
        }
        let where_clause = if conditions.is_empty() {
            "".to_string()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };
        (where_clause, params)
    }

    /// List most recent entries (no embedding). For display in UI. Returns (id, content, created_at_rfc3339, source).
    pub fn list_recent(
        &self,
        limit: usize,
    ) -> anyhow::Result<Vec<(String, String, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, content, created_at, source FROM memory_entries ORDER BY created_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Load (id, content, embedding) for given ids (for hybrid rerank, plan moyen terme 2).
    pub fn get_entries_with_embeddings_by_ids(
        &self,
        ids: &[String],
    ) -> anyhow::Result<Vec<(String, String, Vec<u8>)>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = ids.iter().enumerate().map(|(i, _)| format!("?{}", i + 1)).collect::<Vec<_>>().join(", ");
        let sql = format!(
            "SELECT id, content, embedding FROM memory_entries WHERE id IN ({})",
            placeholders
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::ToSql> = ids.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Vec<u8>>(2)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Search by keywords (lexical fallback when embedder is unavailable).
    /// Optionally filter by entity_id/process_id/session_id.
    pub fn search_by_keywords(
        &self,
        query: &str,
        top_k: usize,
        filter: Option<&MemorySearchFilter>,
    ) -> anyhow::Result<Vec<(String, String)>> {
        let words: Vec<String> = query
            .split_whitespace()
            .map(|s| s.trim())
            .filter(|s| s.len() >= 2)
            .map(|s| {
                s.replace('%', "\\%")
                    .replace('_', "\\_")
                    .replace('\\', "\\\\")
            })
            .collect();
        if words.is_empty() {
            return Ok(Vec::new());
        }
        let (filter_where, filter_params) = Self::filter_clause(filter);
        let like_conditions: String = words
            .iter()
            .enumerate()
            .map(|(i, _)| {
                if i > 0 {
                    " AND content LIKE ? ESCAPE '\\'"
                } else {
                    "content LIKE ? ESCAPE '\\'"
                }
            })
            .collect();
        let filter_conditions = filter_where.trim_start_matches("WHERE ").trim();
        let where_sql = if filter_conditions.is_empty() {
            like_conditions
        } else {
            format!("{} AND {}", like_conditions, filter_conditions)
        };
        let sql = format!(
            "SELECT id, content FROM memory_entries WHERE {} ORDER BY created_at DESC LIMIT ?",
            where_sql
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let patterns: Vec<String> = words.iter().map(|w| format!("%{}%", w)).collect();
        let limit = top_k as i64;
        let mut param_refs: Vec<&dyn rusqlite::ToSql> = patterns.iter().map(|p| p as &dyn rusqlite::ToSql).collect();
        for p in &filter_params {
            param_refs.push(p.as_ref());
        }
        param_refs.push(&limit);
        let mut rows = stmt.query(param_refs.as_slice())?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let content: String = row.get(1)?;
            out.push((id, content));
        }
        Ok(out)
    }

    /// Search by embedding: returns up to `top_k` entries ordered by cosine similarity (desc).
    /// Optionally filter by entity_id/process_id/session_id.
    pub fn search_by_embedding(
        &self,
        query_embedding: &[f32],
        top_k: usize,
        filter: Option<&MemorySearchFilter>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let rows = self.get_all_with_embedding(filter)?;
        let mut scored: Vec<(f32, (String, String, Vec<u8>, String, String))> = rows
            .into_iter()
            .map(|row| {
                let vec = decode_embedding_bytes(&row.2);
                let sim = cosine_similarity(query_embedding, &vec);
                (sim, row)
            })
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let out: Vec<MemoryEntry> = scored
            .into_iter()
            .take(top_k)
            .map(|(_, (id, content, embedding, created_at, source))| {
                let created_at = DateTime::parse_from_rfc3339(&created_at)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now());
                MemoryEntry {
                    id: Uuid::parse_str(&id).unwrap_or_else(|_| Uuid::nil()),
                    content,
                    embedding,
                    created_at,
                    source,
                }
            })
            .collect();
        Ok(out)
    }
}
