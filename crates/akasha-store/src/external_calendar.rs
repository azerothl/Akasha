//! External calendar events, CalDAV accounts, and sync outbox.

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;
use uuid::Uuid;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS caldav_accounts (
    id TEXT PRIMARY KEY,
    label TEXT NOT NULL,
    url TEXT NOT NULL,
    username TEXT NOT NULL,
    calendar_path TEXT,
    enabled INTEGER NOT NULL DEFAULT 1,
    last_sync_at TEXT,
    last_sync_error TEXT,
    sync_token TEXT,
    provider_id TEXT,
    auth_method TEXT NOT NULL DEFAULT 'app_password',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS external_calendar_events (
    id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL,
    uid TEXT NOT NULL,
    href TEXT,
    etag TEXT,
    summary TEXT NOT NULL,
    description TEXT,
    location TEXT,
    dtstart TEXT NOT NULL,
    dtend TEXT,
    timezone TEXT,
    rrule TEXT,
    exdates_json TEXT NOT NULL DEFAULT '[]',
    source TEXT NOT NULL DEFAULT 'ics_import',
    synced_at TEXT,
    deleted INTEGER NOT NULL DEFAULT 0,
    UNIQUE(account_id, uid)
);
CREATE INDEX IF NOT EXISTS idx_ext_cal_account_start ON external_calendar_events(account_id, dtstart);
CREATE INDEX IF NOT EXISTS idx_ext_cal_range ON external_calendar_events(dtstart, dtend);

CREATE TABLE IF NOT EXISTS caldav_outbox (
    id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL,
    event_id TEXT,
    operation TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    applied_at TEXT,
    error TEXT
);
CREATE INDEX IF NOT EXISTS idx_caldav_outbox_pending ON caldav_outbox(applied_at);
"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalCalendarEvent {
    pub id: Uuid,
    pub account_id: Uuid,
    pub uid: String,
    pub href: Option<String>,
    pub etag: Option<String>,
    pub summary: String,
    pub description: Option<String>,
    pub location: Option<String>,
    pub dtstart: DateTime<Utc>,
    pub dtend: Option<DateTime<Utc>>,
    pub timezone: Option<String>,
    pub rrule: Option<String>,
    pub exdates_json: String,
    pub source: String,
    pub synced_at: Option<DateTime<Utc>>,
    pub deleted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalDavAccount {
    pub id: Uuid,
    pub label: String,
    pub url: String,
    pub username: String,
    pub calendar_path: Option<String>,
    pub enabled: bool,
    pub last_sync_at: Option<DateTime<Utc>>,
    pub last_sync_error: Option<String>,
    pub sync_token: Option<String>,
    pub provider_id: Option<String>,
    pub auth_method: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalDavOutboxRow {
    pub id: Uuid,
    pub account_id: Uuid,
    pub event_id: Option<Uuid>,
    pub operation: String,
    pub payload_json: String,
    pub created_at: DateTime<Utc>,
    pub applied_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

pub struct ExternalCalendarStore {
    conn: Connection,
}

impl ExternalCalendarStore {
    pub fn open<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        let _ = conn.execute(
            "ALTER TABLE caldav_accounts ADD COLUMN provider_id TEXT",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE caldav_accounts ADD COLUMN auth_method TEXT NOT NULL DEFAULT 'app_password'",
            [],
        );
        Ok(Self { conn })
    }

    pub fn list_accounts(&self) -> anyhow::Result<Vec<CalDavAccount>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, label, url, username, calendar_path, enabled, last_sync_at, last_sync_error, sync_token, provider_id, auth_method, created_at, updated_at FROM caldav_accounts ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], |row| Ok(row_to_account(row)?))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_account(&self, id: Uuid) -> anyhow::Result<Option<CalDavAccount>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, label, url, username, calendar_path, enabled, last_sync_at, last_sync_error, sync_token, provider_id, auth_method, created_at, updated_at FROM caldav_accounts WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id.to_string()])?;
        if let Some(row) = rows.next()? {
            return Ok(Some(row_to_account(row)?));
        }
        Ok(None)
    }

    pub fn upsert_account(&self, a: &CalDavAccount) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO caldav_accounts (id, label, url, username, calendar_path, enabled, last_sync_at, last_sync_error, sync_token, provider_id, auth_method, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
             ON CONFLICT(id) DO UPDATE SET
               label=excluded.label, url=excluded.url, username=excluded.username,
               calendar_path=excluded.calendar_path, enabled=excluded.enabled,
               last_sync_at=excluded.last_sync_at, last_sync_error=excluded.last_sync_error,
               sync_token=excluded.sync_token, provider_id=excluded.provider_id,
               auth_method=excluded.auth_method, updated_at=excluded.updated_at",
            params![
                a.id.to_string(),
                a.label,
                a.url,
                a.username,
                a.calendar_path,
                a.enabled as i32,
                a.last_sync_at.map(|t| t.to_rfc3339()),
                a.last_sync_error,
                a.sync_token,
                a.provider_id,
                a.auth_method,
                a.created_at.to_rfc3339(),
                a.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn delete_account(&self, id: Uuid) -> anyhow::Result<bool> {
        let n = self
            .conn
            .execute("DELETE FROM caldav_accounts WHERE id = ?1", params![id.to_string()])?;
        self.conn.execute(
            "DELETE FROM external_calendar_events WHERE account_id = ?1",
            params![id.to_string()],
        )?;
        Ok(n > 0)
    }

    pub fn update_account_sync_status(
        &self,
        id: Uuid,
        sync_token: Option<&str>,
        error: Option<&str>,
    ) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE caldav_accounts SET last_sync_at = ?1, last_sync_error = ?2, sync_token = COALESCE(?3, sync_token), updated_at = ?1 WHERE id = ?4",
            params![now, error, sync_token, id.to_string()],
        )?;
        Ok(())
    }

    pub fn list_events_between(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        account_id: Option<Uuid>,
    ) -> anyhow::Result<Vec<ExternalCalendarEvent>> {
        let from_s = from.to_rfc3339();
        let to_s = to.to_rfc3339();
        if let Some(aid) = account_id {
            let mut stmt = self.conn.prepare(
                "SELECT id, account_id, uid, href, etag, summary, description, location, dtstart, dtend, timezone, rrule, exdates_json, source, synced_at, deleted
                 FROM external_calendar_events
                 WHERE deleted = 0 AND account_id = ?3 AND dtstart <= ?2 AND (dtend IS NULL OR dtend >= ?1)
                 ORDER BY dtstart",
            )?;
            let rows = stmt.query_map(params![from_s, to_s, aid.to_string()], row_to_event)?;
            return rows.collect::<Result<Vec<_>, _>>().map_err(Into::into);
        }
        let mut stmt = self.conn.prepare(
            "SELECT id, account_id, uid, href, etag, summary, description, location, dtstart, dtend, timezone, rrule, exdates_json, source, synced_at, deleted
             FROM external_calendar_events
             WHERE deleted = 0 AND dtstart <= ?2 AND (dtend IS NULL OR dtend >= ?1)
             ORDER BY dtstart",
        )?;
        let rows = stmt.query_map(params![from_s, to_s], row_to_event)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_event(&self, id: Uuid) -> anyhow::Result<Option<ExternalCalendarEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, account_id, uid, href, etag, summary, description, location, dtstart, dtend, timezone, rrule, exdates_json, source, synced_at, deleted
             FROM external_calendar_events WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id.to_string()])?;
        if let Some(row) = rows.next()? {
            return Ok(Some(row_to_event(row)?));
        }
        Ok(None)
    }

    pub fn upsert_event(&self, e: &ExternalCalendarEvent) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO external_calendar_events (id, account_id, uid, href, etag, summary, description, location, dtstart, dtend, timezone, rrule, exdates_json, source, synced_at, deleted)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)
             ON CONFLICT(account_id, uid) DO UPDATE SET
               href=excluded.href, etag=excluded.etag, summary=excluded.summary,
               description=excluded.description, location=excluded.location,
               dtstart=excluded.dtstart, dtend=excluded.dtend, timezone=excluded.timezone,
               rrule=excluded.rrule, exdates_json=excluded.exdates_json, source=excluded.source,
               synced_at=excluded.synced_at, deleted=excluded.deleted, id=excluded.id",
            params![
                e.id.to_string(),
                e.account_id.to_string(),
                e.uid,
                e.href,
                e.etag,
                e.summary,
                e.description,
                e.location,
                e.dtstart.to_rfc3339(),
                e.dtend.map(|t| t.to_rfc3339()),
                e.timezone,
                e.rrule,
                e.exdates_json,
                e.source,
                e.synced_at.map(|t| t.to_rfc3339()),
                e.deleted as i32,
            ],
        )?;
        Ok(())
    }

    pub fn mark_deleted_by_hrefs(&self, account_id: Uuid, hrefs: &[String]) -> anyhow::Result<u64> {
        let mut n = 0u64;
        for href in hrefs {
            n += self.conn.execute(
                "UPDATE external_calendar_events SET deleted = 1, synced_at = ?1 WHERE account_id = ?2 AND href = ?3",
                params![Utc::now().to_rfc3339(), account_id.to_string(), href],
            )? as u64;
        }
        Ok(n)
    }

    pub fn soft_delete_event(&self, id: Uuid) -> anyhow::Result<bool> {
        let n = self.conn.execute(
            "UPDATE external_calendar_events SET deleted = 1, synced_at = ?1 WHERE id = ?2",
            params![Utc::now().to_rfc3339(), id.to_string()],
        )?;
        Ok(n > 0)
    }

    pub fn enqueue_outbox(&self, row: &CalDavOutboxRow) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO caldav_outbox (id, account_id, event_id, operation, payload_json, created_at, applied_at, error)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                row.id.to_string(),
                row.account_id.to_string(),
                row.event_id.map(|u| u.to_string()),
                row.operation,
                row.payload_json,
                row.created_at.to_rfc3339(),
                row.applied_at.map(|t| t.to_rfc3339()),
                row.error,
            ],
        )?;
        Ok(())
    }

    pub fn list_pending_outbox(&self, account_id: Option<Uuid>) -> anyhow::Result<Vec<CalDavOutboxRow>> {
        if let Some(aid) = account_id {
            let mut stmt = self.conn.prepare(
                "SELECT id, account_id, event_id, operation, payload_json, created_at, applied_at, error FROM caldav_outbox WHERE applied_at IS NULL AND account_id = ?1 ORDER BY created_at",
            )?;
            let rows = stmt.query_map(params![aid.to_string()], row_to_outbox)?;
            return rows.collect::<Result<Vec<_>, _>>().map_err(Into::into);
        }
        let mut stmt = self.conn.prepare(
            "SELECT id, account_id, event_id, operation, payload_json, created_at, applied_at, error FROM caldav_outbox WHERE applied_at IS NULL ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], row_to_outbox)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn mark_outbox_applied(&self, id: Uuid, error: Option<&str>) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE caldav_outbox SET applied_at = ?1, error = ?2 WHERE id = ?3",
            params![Utc::now().to_rfc3339(), error, id.to_string()],
        )?;
        Ok(())
    }

    pub fn list_outbox_all(&self, limit: usize) -> anyhow::Result<Vec<CalDavOutboxRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, account_id, event_id, operation, payload_json, created_at, applied_at, error FROM caldav_outbox ORDER BY created_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], row_to_outbox)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}

fn parse_ts(s: &str) -> anyhow::Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(s)?.with_timezone(&Utc))
}

fn row_to_account(row: &rusqlite::Row<'_>) -> rusqlite::Result<CalDavAccount> {
    Ok(CalDavAccount {
        id: Uuid::parse_str(row.get::<_, String>(0)?.as_str()).unwrap_or_else(|_| Uuid::nil()),
        label: row.get(1)?,
        url: row.get(2)?,
        username: row.get(3)?,
        calendar_path: row.get(4)?,
        enabled: row.get::<_, i32>(5)? != 0,
        last_sync_at: row
            .get::<_, Option<String>>(6)?
            .and_then(|s| parse_ts(&s).ok()),
        last_sync_error: row.get(7)?,
        sync_token: row.get(8)?,
        provider_id: row.get(9)?,
        auth_method: row
            .get::<_, Option<String>>(10)?
            .unwrap_or_else(|| "app_password".to_string()),
        created_at: parse_ts(&row.get::<_, String>(11)?).unwrap_or_else(|_| Utc::now()),
        updated_at: parse_ts(&row.get::<_, String>(12)?).unwrap_or_else(|_| Utc::now()),
    })
}

fn row_to_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<ExternalCalendarEvent> {
    Ok(ExternalCalendarEvent {
        id: Uuid::parse_str(row.get::<_, String>(0)?.as_str()).unwrap_or_else(|_| Uuid::nil()),
        account_id: Uuid::parse_str(row.get::<_, String>(1)?.as_str()).unwrap_or_else(|_| Uuid::nil()),
        uid: row.get(2)?,
        href: row.get(3)?,
        etag: row.get(4)?,
        summary: row.get(5)?,
        description: row.get(6)?,
        location: row.get(7)?,
        dtstart: parse_ts(&row.get::<_, String>(8)?).unwrap_or_else(|_| Utc::now()),
        dtend: row
            .get::<_, Option<String>>(9)?
            .and_then(|s| parse_ts(&s).ok()),
        timezone: row.get(10)?,
        rrule: row.get(11)?,
        exdates_json: row.get::<_, Option<String>>(12)?.unwrap_or_else(|| "[]".to_string()),
        source: row.get::<_, Option<String>>(13)?.unwrap_or_else(|| "ics_import".to_string()),
        synced_at: row
            .get::<_, Option<String>>(14)?
            .and_then(|s| parse_ts(&s).ok()),
        deleted: row.get::<_, i32>(15)? != 0,
    })
}

fn row_to_outbox(row: &rusqlite::Row<'_>) -> rusqlite::Result<CalDavOutboxRow> {
    Ok(CalDavOutboxRow {
        id: Uuid::parse_str(row.get::<_, String>(0)?.as_str()).unwrap_or_else(|_| Uuid::nil()),
        account_id: Uuid::parse_str(row.get::<_, String>(1)?.as_str()).unwrap_or_else(|_| Uuid::nil()),
        event_id: row
            .get::<_, Option<String>>(2)?
            .and_then(|s| Uuid::parse_str(&s).ok()),
        operation: row.get(3)?,
        payload_json: row.get(4)?,
        created_at: parse_ts(&row.get::<_, String>(5)?).unwrap_or_else(|_| Utc::now()),
        applied_at: row
            .get::<_, Option<String>>(6)?
            .and_then(|s| parse_ts(&s).ok()),
        error: row.get(7)?,
    })
}

/// Default local account for ICS imports without CalDAV account configured.
pub fn default_ics_account_id() -> Uuid {
    Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn temp_store() -> (tempfile::TempDir, ExternalCalendarStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("akasha.db");
        let store = ExternalCalendarStore::open(&path).expect("open");
        (dir, store)
    }

    #[test]
    fn upsert_and_list_events_in_range() {
        let (_dir, store) = temp_store();
        let account_id = default_ics_account_id();
        let start = Utc.with_ymd_and_hms(2026, 6, 10, 14, 0, 0).unwrap();
        let end = start + chrono::Duration::hours(1);
        let event = ExternalCalendarEvent {
            id: Uuid::new_v4(),
            account_id,
            uid: "evt-1".into(),
            href: None,
            etag: None,
            summary: "Team sync".into(),
            description: None,
            location: None,
            dtstart: start,
            dtend: Some(end),
            timezone: None,
            rrule: None,
            exdates_json: "[]".into(),
            source: "ics_import".into(),
            synced_at: None,
            deleted: false,
        };
        store.upsert_event(&event).expect("upsert");

        let from = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        let to = Utc.with_ymd_and_hms(2026, 6, 30, 0, 0, 0).unwrap();
        let listed = store.list_events_between(from, to, None).expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].summary, "Team sync");

        // Upsert same uid updates summary
        let mut updated = event.clone();
        updated.summary = "Updated".into();
        store.upsert_event(&updated).expect("upsert again");
        let listed2 = store.list_events_between(from, to, None).expect("list");
        assert_eq!(listed2.len(), 1);
        assert_eq!(listed2[0].summary, "Updated");
    }

    #[test]
    fn soft_delete_excludes_from_range_query() {
        let (_dir, store) = temp_store();
        let id = Uuid::new_v4();
        let start = Utc.with_ymd_and_hms(2026, 7, 1, 9, 0, 0).unwrap();
        store
            .upsert_event(&ExternalCalendarEvent {
                id,
                account_id: default_ics_account_id(),
                uid: "del-me".into(),
                href: None,
                etag: None,
                summary: "Gone".into(),
                description: None,
                location: None,
                dtstart: start,
                dtend: None,
                timezone: None,
                rrule: None,
                exdates_json: "[]".into(),
                source: "local".into(),
                synced_at: None,
                deleted: false,
            })
            .expect("upsert");
        assert!(store.soft_delete_event(id).expect("delete"));
        let from = start - chrono::Duration::days(1);
        let to = start + chrono::Duration::days(1);
        assert!(store.list_events_between(from, to, None).unwrap().is_empty());
    }
}
