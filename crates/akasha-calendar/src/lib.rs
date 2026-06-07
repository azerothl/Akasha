//! ICS (RFC 5545 subset) and shared calendar event types.

mod ics;
mod providers;

pub use ics::{parse_ics, serialize_ics, IcsEvent};
pub use providers::{caldav_provider_presets, preset_by_id, CalDavProviderPreset};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventSource {
    IcsImport,
    Caldav,
    Local,
}

impl EventSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::IcsImport => "ics_import",
            Self::Caldav => "caldav",
            Self::Local => "local",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "caldav" => Self::Caldav,
            "local" => Self::Local,
            _ => Self::IcsImport,
        }
    }
}

/// Portable event DTO shared between store, API, and ICS layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarEventDto {
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
    pub exdates: Vec<String>,
    pub source: EventSource,
    pub synced_at: Option<DateTime<Utc>>,
    pub deleted: bool,
}

impl CalendarEventDto {
    pub fn to_ics_event(&self) -> IcsEvent {
        IcsEvent {
            uid: self.uid.clone(),
            summary: self.summary.clone(),
            description: self.description.clone(),
            location: self.location.clone(),
            dtstart: self.dtstart,
            dtend: self.dtend,
            rrule: self.rrule.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalDavAccountDto {
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
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboxOperation {
    Create,
    Update,
    Delete,
}

impl OutboxOperation {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Delete => "delete",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "update" => Self::Update,
            "delete" => Self::Delete,
            _ => Self::Create,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalDavOutboxEntry {
    pub id: Uuid,
    pub account_id: Uuid,
    pub event_id: Option<Uuid>,
    pub operation: OutboxOperation,
    pub payload_json: String,
    pub created_at: DateTime<Utc>,
    pub applied_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ics_roundtrip_simple_event() {
        let raw = r#"BEGIN:VCALENDAR
VERSION:2.0
PRODID:-//Akasha//EN
BEGIN:VEVENT
UID:test-uid-1
DTSTART:20260615T100000Z
DTEND:20260615T110000Z
SUMMARY:Team standup
DESCRIPTION:Daily sync
END:VEVENT
END:VCALENDAR
"#;
        let events = parse_ics(raw).expect("parse");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].summary, "Team standup");
        let out = serialize_ics(&events, "Akasha Test");
        assert!(out.contains("BEGIN:VEVENT"));
        assert!(out.contains("Team standup"));
    }
}
