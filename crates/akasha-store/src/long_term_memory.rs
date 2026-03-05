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

/// Cosine similarity between two unit-normalized vectors (or any vectors; result in [-1, 1]).
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
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

fn bytes_to_f32_slice(b: &[u8]) -> Vec<f32> {
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
        Ok(Self { conn })
    }

    pub fn insert(
        &self,
        content: &str,
        embedding: &[u8],
        source: &str,
    ) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        let now = Utc::now();
        self.conn.execute(
            r#"
            INSERT INTO memory_entries (id, content, embedding, created_at, source)
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
            rusqlite::params![id.to_string(), content, embedding, now.to_rfc3339(), source],
        )?;
        Ok(id)
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

    /// Retrieve all entries with their embeddings for similarity search in memory.
    fn get_all_with_embedding(
        &self,
    ) -> anyhow::Result<Vec<(String, String, Vec<u8>, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, content, embedding, created_at, source FROM memory_entries ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], |row| {
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

    /// List most recent entries (no embedding). For display in UI. Returns (content, created_at_rfc3339, source).
    pub fn list_recent(
        &self,
        limit: usize,
    ) -> anyhow::Result<Vec<(String, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT content, created_at, source FROM memory_entries ORDER BY created_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Search by embedding: returns up to `top_k` entries ordered by cosine similarity (desc).
    pub fn search_by_embedding(
        &self,
        query_embedding: &[f32],
        top_k: usize,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let rows = self.get_all_with_embedding()?;
        let mut scored: Vec<(f32, (String, String, Vec<u8>, String, String))> = rows
            .into_iter()
            .map(|row| {
                let vec = bytes_to_f32_slice(&row.2);
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
