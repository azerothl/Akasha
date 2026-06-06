//! Platform tables: wakeups, contacts, notifications, conversation archive (KinBot-inspired roadmap).

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;
use uuid::Uuid;

fn open_conn<P: AsRef<Path>>(path: P) -> anyhow::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS wakeups (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            fire_at TEXT NOT NULL,
            message TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'pending',
            created_by_task_id TEXT,
            rrule TEXT,
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_wakeups_fire ON wakeups(status, fire_at);

        CREATE TABLE IF NOT EXISTS contacts (
            id TEXT PRIMARY KEY,
            display_name TEXT NOT NULL,
            identifiers_json TEXT NOT NULL DEFAULT '{}',
            notes TEXT NOT NULL DEFAULT '',
            tags_json TEXT NOT NULL DEFAULT '[]',
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_contacts_name ON contacts(display_name);

        CREATE TABLE IF NOT EXISTS notifications (
            id TEXT PRIMARY KEY,
            type TEXT NOT NULL,
            title TEXT NOT NULL,
            body TEXT NOT NULL,
            read INTEGER NOT NULL DEFAULT 0,
            task_id TEXT,
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_notifications_created ON notifications(created_at DESC);

        CREATE TABLE IF NOT EXISTS conversation_archive (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            turn_index INTEGER NOT NULL,
            role TEXT NOT NULL,
            content TEXT NOT NULL,
            archived_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_conv_archive_session ON conversation_archive(session_id);
        "#,
    )?;
    Ok(conn)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wakeup {
    pub id: Uuid,
    pub session_id: String,
    pub fire_at: DateTime<Utc>,
    pub message: String,
    pub status: String,
    pub created_by_task_id: Option<Uuid>,
    pub rrule: Option<String>,
    pub created_at: DateTime<Utc>,
}

pub struct WakeupStore {
    conn: Connection,
}

impl WakeupStore {
    pub fn open<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        Ok(Self {
            conn: open_conn(path)?,
        })
    }

    pub fn insert(&self, w: &Wakeup) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO wakeups (id, session_id, fire_at, message, status, created_by_task_id, rrule, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                w.id.to_string(),
                w.session_id,
                w.fire_at.to_rfc3339(),
                w.message,
                w.status,
                w.created_by_task_id.map(|u| u.to_string()),
                w.rrule,
                w.created_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn list_pending_before(&self, before: DateTime<Utc>) -> anyhow::Result<Vec<Wakeup>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, fire_at, message, status, created_by_task_id, rrule, created_at
             FROM wakeups WHERE status = 'pending' AND fire_at <= ?1 ORDER BY fire_at",
        )?;
        let rows = stmt.query_map(params![before.to_rfc3339()], |row| {
            Ok(Wakeup {
                id: Uuid::parse_str(&row.get::<_, String>(0)?).unwrap_or_else(|_| Uuid::nil()),
                session_id: row.get(1)?,
                fire_at: row.get::<_, String>(2)?.parse().unwrap_or_else(|_| Utc::now()),
                message: row.get(3)?,
                status: row.get(4)?,
                created_by_task_id: row
                    .get::<_, Option<String>>(5)?
                    .and_then(|s| Uuid::parse_str(&s).ok()),
                rrule: row.get(6)?,
                created_at: row.get::<_, String>(7)?.parse().unwrap_or_else(|_| Utc::now()),
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn list_all(&self) -> anyhow::Result<Vec<Wakeup>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, fire_at, message, status, created_by_task_id, rrule, created_at
             FROM wakeups ORDER BY fire_at",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Wakeup {
                id: Uuid::parse_str(&row.get::<_, String>(0)?).unwrap_or_else(|_| Uuid::nil()),
                session_id: row.get(1)?,
                fire_at: row.get::<_, String>(2)?.parse().unwrap_or_else(|_| Utc::now()),
                message: row.get(3)?,
                status: row.get(4)?,
                created_by_task_id: row
                    .get::<_, Option<String>>(5)?
                    .and_then(|s| Uuid::parse_str(&s).ok()),
                rrule: row.get(6)?,
                created_at: row.get::<_, String>(7)?.parse().unwrap_or_else(|_| Utc::now()),
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn mark_fired(&self, id: &Uuid) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE wakeups SET status = 'fired' WHERE id = ?1",
            params![id.to_string()],
        )?;
        Ok(())
    }

    pub fn delete(&self, id: &Uuid) -> anyhow::Result<bool> {
        let n = self.conn.execute(
            "DELETE FROM wakeups WHERE id = ?1",
            params![id.to_string()],
        )?;
        Ok(n > 0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contact {
    pub id: Uuid,
    pub display_name: String,
    pub identifiers: serde_json::Value,
    pub notes: String,
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub struct ContactStore {
    conn: Connection,
}

impl ContactStore {
    pub fn open<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        Ok(Self {
            conn: open_conn(path)?,
        })
    }

    pub fn insert(&self, c: &Contact) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO contacts (id, display_name, identifiers_json, notes, tags_json, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                c.id.to_string(),
                c.display_name,
                c.identifiers.to_string(),
                c.notes,
                serde_json::to_string(&c.tags)?,
                c.created_at.to_rfc3339(),
                c.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn update(&self, c: &Contact) -> anyhow::Result<bool> {
        let n = self.conn.execute(
            "UPDATE contacts SET display_name=?2, identifiers_json=?3, notes=?4, tags_json=?5, updated_at=?6 WHERE id=?1",
            params![
                c.id.to_string(),
                c.display_name,
                c.identifiers.to_string(),
                c.notes,
                serde_json::to_string(&c.tags)?,
                c.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(n > 0)
    }

    pub fn delete(&self, id: &Uuid) -> anyhow::Result<bool> {
        let n = self.conn.execute(
            "DELETE FROM contacts WHERE id = ?1",
            params![id.to_string()],
        )?;
        Ok(n > 0)
    }

    pub fn get(&self, id: &Uuid) -> anyhow::Result<Option<Contact>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, display_name, identifiers_json, notes, tags_json, created_at, updated_at FROM contacts WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id.to_string()])?;
        if let Some(row) = rows.next()? {
            return Ok(Some(row_to_contact(row)?));
        }
        Ok(None)
    }

    pub fn search(&self, query: &str, limit: usize) -> anyhow::Result<Vec<Contact>> {
        let q = format!("%{}%", query.to_lowercase());
        let mut stmt = self.conn.prepare(
            "SELECT id, display_name, identifiers_json, notes, tags_json, created_at, updated_at
             FROM contacts WHERE lower(display_name) LIKE ?1 OR lower(notes) LIKE ?1 OR lower(identifiers_json) LIKE ?1
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![q, limit as i64], row_to_contact)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn list(&self, limit: usize) -> anyhow::Result<Vec<Contact>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, display_name, identifiers_json, notes, tags_json, created_at, updated_at
             FROM contacts ORDER BY display_name LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], row_to_contact)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }
}

fn row_to_contact(row: &rusqlite::Row<'_>) -> rusqlite::Result<Contact> {
    let tags: Vec<String> = row
        .get::<_, String>(4)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let identifiers: serde_json::Value = row
        .get::<_, String>(2)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::json!({}));
    Ok(Contact {
        id: Uuid::parse_str(&row.get::<_, String>(0)?).unwrap_or_else(|_| Uuid::nil()),
        display_name: row.get(1)?,
        identifiers,
        notes: row.get(3)?,
        tags,
        created_at: row
            .get::<_, String>(5)?
            .parse()
            .unwrap_or_else(|_| Utc::now()),
        updated_at: row
            .get::<_, String>(6)?
            .parse()
            .unwrap_or_else(|_| Utc::now()),
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationRow {
    pub id: Uuid,
    pub type_: String,
    pub title: String,
    pub body: String,
    pub read: bool,
    pub task_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

pub struct NotificationStore {
    conn: Connection,
}

impl NotificationStore {
    pub fn open<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        Ok(Self {
            conn: open_conn(path)?,
        })
    }

    pub fn insert(&self, n: &NotificationRow) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO notifications (id, type, title, body, read, task_id, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                n.id.to_string(),
                n.type_,
                n.title,
                n.body,
                if n.read { 1 } else { 0 },
                n.task_id.map(|u| u.to_string()),
                n.created_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn list(&self, unread_only: bool, limit: usize) -> anyhow::Result<Vec<NotificationRow>> {
        let sql = if unread_only {
            "SELECT id, type, title, body, read, task_id, created_at FROM notifications WHERE read = 0 ORDER BY created_at DESC LIMIT ?1"
        } else {
            "SELECT id, type, title, body, read, task_id, created_at FROM notifications ORDER BY created_at DESC LIMIT ?1"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok(NotificationRow {
                id: Uuid::parse_str(&row.get::<_, String>(0)?).unwrap_or_else(|_| Uuid::nil()),
                type_: row.get(1)?,
                title: row.get(2)?,
                body: row.get(3)?,
                read: row.get::<_, i64>(4)? != 0,
                task_id: row
                    .get::<_, Option<String>>(5)?
                    .and_then(|s| Uuid::parse_str(&s).ok()),
                created_at: row
                    .get::<_, String>(6)?
                    .parse()
                    .unwrap_or_else(|_| Utc::now()),
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn mark_read(&self, id: &Uuid) -> anyhow::Result<bool> {
        let n = self.conn.execute(
            "UPDATE notifications SET read = 1 WHERE id = ?1",
            params![id.to_string()],
        )?;
        Ok(n > 0)
    }

    pub fn mark_all_read(&self) -> anyhow::Result<()> {
        self.conn.execute("UPDATE notifications SET read = 1", [])?;
        Ok(())
    }

    pub fn purge_older_than_days(&self, days: i64) -> anyhow::Result<u64> {
        let cutoff = Utc::now() - chrono::Duration::days(days);
        let n = self.conn.execute(
            "DELETE FROM notifications WHERE created_at < ?1",
            params![cutoff.to_rfc3339()],
        )?;
        Ok(n as u64)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchivedTurn {
    pub session_id: String,
    pub turn_index: i64,
    pub role: String,
    pub content: String,
    pub archived_at: DateTime<Utc>,
}

pub struct ConversationArchiveStore {
    conn: Connection,
}

impl ConversationArchiveStore {
    pub fn open<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        Ok(Self {
            conn: open_conn(path)?,
        })
    }

    pub fn archive_turns(&self, session_id: &str, turns: &[(usize, String, String)]) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        for (idx, role, content) in turns {
            self.conn.execute(
                "INSERT INTO conversation_archive (session_id, turn_index, role, content, archived_at)
                 VALUES (?1,?2,?3,?4,?5)",
                params![session_id, *idx as i64, role, content, now],
            )?;
        }
        Ok(())
    }

    pub fn search(&self, session_id: Option<&str>, query: &str, limit: usize) -> anyhow::Result<Vec<ArchivedTurn>> {
        let q = format!("%{}%", query.to_lowercase());
        let (sql, sid) = match session_id {
            Some(s) => (
                "SELECT session_id, turn_index, role, content, archived_at FROM conversation_archive
                 WHERE session_id = ?1 AND lower(content) LIKE ?2 ORDER BY archived_at DESC LIMIT ?3",
                Some(s.to_string()),
            ),
            None => (
                "SELECT session_id, turn_index, role, content, archived_at FROM conversation_archive
                 WHERE lower(content) LIKE ?1 ORDER BY archived_at DESC LIMIT ?2",
                None,
            ),
        };
        let mut stmt = self.conn.prepare(sql)?;
        let map_row = |row: &rusqlite::Row<'_>| {
            Ok(ArchivedTurn {
                session_id: row.get(0)?,
                turn_index: row.get(1)?,
                role: row.get(2)?,
                content: row.get(3)?,
                archived_at: row
                    .get::<_, String>(4)?
                    .parse()
                    .unwrap_or_else(|_| Utc::now()),
            })
        };
        let rows: Vec<ArchivedTurn> = if let Some(s) = sid {
            stmt.query_map(params![s, q, limit as i64], map_row)?
                .filter_map(|r| r.ok())
                .collect()
        } else {
            stmt.query_map(params![q, limit as i64], map_row)?
                .filter_map(|r| r.ok())
                .collect()
        };
        Ok(rows)
    }
}
