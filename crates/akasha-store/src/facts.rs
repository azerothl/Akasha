//! Facts and knowledge graph store (Phase 3 — Mémoire 4 couches, spec 46).

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use std::path::Path;
use uuid::Uuid;

/// One fact: subject --predicate--> object.
#[derive(Debug, Clone)]
pub struct Fact {
    pub id: Uuid,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub source_entry_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

pub struct FactsStore {
    conn: Connection,
}

impl FactsStore {
    /// Open the facts store. Uses the same DB path as long-term memory; creates facts table if missing.
    pub fn open<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS facts (
                id TEXT PRIMARY KEY,
                subject TEXT NOT NULL,
                predicate TEXT NOT NULL,
                object TEXT NOT NULL,
                source_entry_id TEXT,
                created_at TEXT NOT NULL,
                FOREIGN KEY (source_entry_id) REFERENCES memory_entries(id)
            );
            CREATE INDEX IF NOT EXISTS idx_facts_subject ON facts(subject);
            CREATE INDEX IF NOT EXISTS idx_facts_object ON facts(object);
            CREATE INDEX IF NOT EXISTS idx_facts_predicate ON facts(predicate);
            CREATE INDEX IF NOT EXISTS idx_facts_created ON facts(created_at);
            "#,
        )?;
        let has_col = |name: &str| -> anyhow::Result<bool> {
            let mut stmt =
                conn.prepare("SELECT name FROM pragma_table_info('facts') WHERE name = ?1")?;
            Ok(stmt.exists(rusqlite::params![name])?)
        };
        if !has_col("valid_from")? {
            conn.execute("ALTER TABLE facts ADD COLUMN valid_from TEXT", [])?;
        }
        if !has_col("recorded_at")? {
            conn.execute("ALTER TABLE facts ADD COLUMN recorded_at TEXT", [])?;
        }
        Ok(Self { conn })
    }

    pub fn insert_fact_with_temporal(
        &self,
        subject: &str,
        predicate: &str,
        object: &str,
        source_entry_id: Option<Uuid>,
        valid_from: Option<&str>,
        recorded_at: Option<&str>,
    ) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        let now = Utc::now();
        let now_s = now.to_rfc3339();
        let recorded = recorded_at.unwrap_or(&now_s);
        self.conn.execute(
            r#"
            INSERT INTO facts (id, subject, predicate, object, source_entry_id, created_at, valid_from, recorded_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
            rusqlite::params![
                id.to_string(),
                subject,
                predicate,
                object,
                source_entry_id.map(|u| u.to_string()),
                now.to_rfc3339(),
                valid_from,
                recorded,
            ],
        )?;
        Ok(id)
    }

    /// List facts ordered by created_at desc (export / admin).
    pub fn list_facts(&self, limit: usize) -> anyhow::Result<Vec<Fact>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, subject, predicate, object, source_entry_id, created_at
            FROM facts ORDER BY created_at DESC LIMIT ?1
            "#,
        )?;
        let rows = stmt.query_map(rusqlite::params![limit as i64], |row| {
            let created_at_s: String = row.get(5)?;
            let created_at = DateTime::parse_from_rfc3339(&created_at_s)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            Ok(Fact {
                id: Uuid::parse_str(&row.get::<_, String>(0)?).unwrap_or_else(|_| Uuid::nil()),
                subject: row.get(1)?,
                predicate: row.get(2)?,
                object: row.get(3)?,
                source_entry_id: row
                    .get::<_, Option<String>>(4)?
                    .and_then(|s| Uuid::parse_str(&s).ok()),
                created_at,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn insert_fact(
        &self,
        subject: &str,
        predicate: &str,
        object: &str,
        source_entry_id: Option<Uuid>,
    ) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        let now = Utc::now();
        self.conn.execute(
            r#"
            INSERT INTO facts (id, subject, predicate, object, source_entry_id, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
            rusqlite::params![
                id.to_string(),
                subject,
                predicate,
                object,
                source_entry_id.map(|u| u.to_string()),
                now.to_rfc3339(),
            ],
        )?;
        Ok(id)
    }

    /// Search facts by lexical match on subject, predicate, or object.
    pub fn search_facts(&self, query: &str, limit: usize) -> anyhow::Result<Vec<Fact>> {
        let pattern = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, subject, predicate, object, source_entry_id, created_at
            FROM facts
            WHERE subject LIKE ?1 ESCAPE '\' OR predicate LIKE ?1 ESCAPE '\' OR object LIKE ?1 ESCAPE '\'
            ORDER BY created_at DESC
            LIMIT ?2
            "#,
        )?;
        let rows = stmt.query_map(rusqlite::params![pattern, limit as i64], |row| {
            let created_at_s: String = row.get(5)?;
            let created_at = DateTime::parse_from_rfc3339(&created_at_s)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            Ok(Fact {
                id: Uuid::parse_str(&row.get::<_, String>(0)?).unwrap_or_else(|_| Uuid::nil()),
                subject: row.get(1)?,
                predicate: row.get(2)?,
                object: row.get(3)?,
                source_entry_id: row
                    .get::<_, Option<String>>(4)?
                    .and_then(|s| Uuid::parse_str(&s).ok()),
                created_at,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_facts_by_subject(&self, subject: &str, limit: usize) -> anyhow::Result<Vec<Fact>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, subject, predicate, object, source_entry_id, created_at
            FROM facts WHERE subject = ?1 ORDER BY created_at DESC LIMIT ?2
            "#,
        )?;
        let rows = stmt.query_map(rusqlite::params![subject, limit as i64], |row| {
            let created_at_s: String = row.get(5)?;
            let created_at = DateTime::parse_from_rfc3339(&created_at_s)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            Ok(Fact {
                id: Uuid::parse_str(&row.get::<_, String>(0)?).unwrap_or_else(|_| Uuid::nil()),
                subject: row.get(1)?,
                predicate: row.get(2)?,
                object: row.get(3)?,
                source_entry_id: row
                    .get::<_, Option<String>>(4)?
                    .and_then(|s| Uuid::parse_str(&s).ok()),
                created_at,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Get facts where entity_id appears as subject or object.
    pub fn get_facts_by_entity(&self, entity_id: &str, limit: usize) -> anyhow::Result<Vec<Fact>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, subject, predicate, object, source_entry_id, created_at
            FROM facts WHERE subject = ?1 OR object = ?1 ORDER BY created_at DESC LIMIT ?2
            "#,
        )?;
        let rows = stmt.query_map(rusqlite::params![entity_id, limit as i64], |row| {
            let created_at_s: String = row.get(5)?;
            let created_at = DateTime::parse_from_rfc3339(&created_at_s)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            Ok(Fact {
                id: Uuid::parse_str(&row.get::<_, String>(0)?).unwrap_or_else(|_| Uuid::nil()),
                subject: row.get(1)?,
                predicate: row.get(2)?,
                object: row.get(3)?,
                source_entry_id: row
                    .get::<_, Option<String>>(4)?
                    .and_then(|s| Uuid::parse_str(&s).ok()),
                created_at,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Get related entity ids (neighbours in the graph): entities that appear in a fact with the given entity.
    pub fn get_related_entities(&self, entity_id: &str, limit: usize) -> anyhow::Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT DISTINCT object FROM facts WHERE subject = ?1
            UNION
            SELECT DISTINCT subject FROM facts WHERE object = ?1
            LIMIT ?2
            "#,
        )?;
        let rows = stmt.query_map(rusqlite::params![entity_id, limit as i64], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Get subgraph around entity: facts where entity is subject or object, up to depth (1 = direct neighbours).
    pub fn get_entity_graph(&self, entity_id: &str, depth: usize, limit: usize) -> anyhow::Result<Vec<Fact>> {
        if depth == 0 {
            return Ok(Vec::new());
        }
        self.get_facts_by_entity(entity_id, limit)
    }

    pub fn get_graph_stats(&self) -> anyhow::Result<(u64, u64)> {
        let count: u64 = self.conn.query_row("SELECT COUNT(*) FROM facts", [], |row| row.get(0))?;
        let entities: u64 = self.conn.query_row(
            "SELECT COUNT(*) FROM (SELECT subject AS e FROM facts UNION SELECT object FROM facts)",
            [],
            |row| row.get(0),
        )?;
        Ok((count, entities))
    }
}

/// Simple pattern-based fact extraction (V1). Returns (subject, predicate, object) triplets.
/// Used after promote to populate the facts table.
pub fn extract_facts_simple(text: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let lower = text.to_lowercase();
    // "user préfère X", "l'utilisateur préfère X", "prefers X"
    for (pred, pred_en) in [
        ("préfère", "prefers"),
        ("prefer", "prefers"),
        ("utilise", "uses"),
        ("utiliser", "uses"),
        ("veut", "wants"),
        ("want", "wants"),
        ("aime", "likes"),
        ("like", "likes"),
    ] {
        if lower.contains(pred) || lower.contains(pred_en) {
            let subject = "user".to_string();
            let object = text
                .split_whitespace()
                .skip_while(|w| !w.to_lowercase().contains(&pred.to_string()) && !w.to_lowercase().contains(pred_en))
                .skip(1)
                .take(5)
                .collect::<Vec<_>>()
                .join(" ");
            if !object.is_empty() && object.len() < 200 {
                out.push((subject, pred_en.to_string(), object.trim().to_string()));
            }
        }
    }
    // "X est un projet" -> (X, "is_type", "project")
    if lower.contains("projet") || lower.contains("project") {
        let words: Vec<&str> = text.split_whitespace().collect();
        for (i, w) in words.iter().enumerate() {
            if w.to_lowercase().contains("projet") || w.to_lowercase().contains("project") {
                if i > 0 {
                    let subject = words[..i].join(" ").chars().take(100).collect::<String>();
                    if !subject.is_empty() {
                        out.push((subject, "is_type".to_string(), "project".to_string()));
                    }
                }
                break;
            }
        }
    }
    out
}
