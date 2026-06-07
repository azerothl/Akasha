//! Minimal ICS parser/serializer (VEVENT subset).

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum IcsError {
    #[error("invalid datetime: {0}")]
    DateTime(String),
    #[error("missing required field: {0}")]
    MissingField(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IcsEvent {
    pub uid: String,
    pub summary: String,
    pub description: Option<String>,
    pub location: Option<String>,
    pub dtstart: DateTime<Utc>,
    pub dtend: Option<DateTime<Utc>>,
    pub rrule: Option<String>,
}

#[derive(Default)]
struct PartialEvent {
    uid: String,
    summary: String,
    description: Option<String>,
    location: Option<String>,
    dtstart: Option<DateTime<Utc>>,
    dtend: Option<DateTime<Utc>>,
    rrule: Option<String>,
}

impl PartialEvent {
    fn into_event(self) -> Result<IcsEvent, IcsError> {
        if self.uid.is_empty() {
            return Err(IcsError::MissingField("UID"));
        }
        let dtstart = self.dtstart.ok_or(IcsError::MissingField("DTSTART"))?;
        let summary = if self.summary.is_empty() {
            self.uid.clone()
        } else {
            self.summary
        };
        Ok(IcsEvent {
            uid: self.uid,
            summary,
            description: self.description,
            location: self.location,
            dtstart,
            dtend: self.dtend,
            rrule: self.rrule,
        })
    }
}

fn unfold_lines(input: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for line in input.lines() {
        let line = line.trim_end_matches('\r');
        if (line.starts_with(' ') || line.starts_with('\t')) && !lines.is_empty() {
            let last = lines.len() - 1;
            lines[last].push_str(line.trim_start());
        } else {
            lines.push(line.to_string());
        }
    }
    lines
}

fn parse_ics_datetime(raw: &str) -> Result<DateTime<Utc>, IcsError> {
    let s = raw.trim();
    if s.len() == 8 {
        let d = NaiveDate::parse_from_str(s, "%Y%m%d")
            .map_err(|_| IcsError::DateTime(s.to_string()))?;
        return Ok(d.and_hms_opt(0, 0, 0).unwrap().and_utc());
    }
    if s.ends_with('Z') && s.len() >= 16 {
        let nd = NaiveDateTime::parse_from_str(s.trim_end_matches('Z'), "%Y%m%dT%H%M%S")
            .map_err(|_| IcsError::DateTime(s.to_string()))?;
        return Ok(nd.and_utc());
    }
    if s.contains('T') && s.len() >= 15 {
        if let Ok(nd) = NaiveDateTime::parse_from_str(s, "%Y%m%dT%H%M%S") {
            return Ok(nd.and_utc());
        }
    }
    Err(IcsError::DateTime(s.to_string()))
}

fn field_value(line: &str) -> (&str, String) {
    let (key, val) = line.split_once(':').unwrap_or((line, ""));
    let key = key.split(';').next().unwrap_or(key);
    (key, val.trim().to_string())
}

/// Parse VEVENT blocks from ICS text.
pub fn parse_ics(input: &str) -> Result<Vec<IcsEvent>, IcsError> {
    let lines = unfold_lines(input);
    let mut events = Vec::new();
    let mut partial: Option<PartialEvent> = None;

    for line in lines {
        if line == "BEGIN:VEVENT" {
            partial = Some(PartialEvent::default());
            continue;
        }
        if line == "END:VEVENT" {
            if let Some(p) = partial.take() {
                events.push(p.into_event()?);
            }
            continue;
        }
        let Some(ref mut p) = partial else {
            continue;
        };
        let (key, val) = field_value(&line);
        match key {
            "UID" => p.uid = val,
            "SUMMARY" => p.summary = val,
            "DESCRIPTION" => p.description = Some(val),
            "LOCATION" => p.location = Some(val),
            "DTSTART" => p.dtstart = Some(parse_ics_datetime(&val)?),
            "DTEND" => p.dtend = Some(parse_ics_datetime(&val)?),
            "RRULE" => p.rrule = Some(val),
            _ => {}
        }
    }
    Ok(events)
}

fn format_ics_dt(dt: DateTime<Utc>) -> String {
    dt.format("%Y%m%dT%H%M%SZ").to_string()
}

fn escape_ics(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace(',', "\\,")
        .replace(';', "\\;")
}

/// Serialize events to ICS calendar text.
pub fn serialize_ics(events: &[IcsEvent], prodid: &str) -> String {
    let mut out = String::from("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//");
    out.push_str(prodid);
    out.push_str("//EN\r\nCALSCALE:GREGORIAN\r\n");
    for ev in events {
        out.push_str("BEGIN:VEVENT\r\n");
        out.push_str("UID:");
        out.push_str(&escape_ics(&ev.uid));
        out.push_str("\r\nDTSTART:");
        out.push_str(&format_ics_dt(ev.dtstart));
        out.push_str("\r\n");
        if let Some(end) = ev.dtend {
            out.push_str("DTEND:");
            out.push_str(&format_ics_dt(end));
            out.push_str("\r\n");
        }
        out.push_str("SUMMARY:");
        out.push_str(&escape_ics(&ev.summary));
        out.push_str("\r\n");
        if let Some(ref d) = ev.description {
            out.push_str("DESCRIPTION:");
            out.push_str(&escape_ics(d));
            out.push_str("\r\n");
        }
        if let Some(ref l) = ev.location {
            out.push_str("LOCATION:");
            out.push_str(&escape_ics(l));
            out.push_str("\r\n");
        }
        if let Some(ref r) = ev.rrule {
            out.push_str("RRULE:");
            out.push_str(r);
            out.push_str("\r\n");
        }
        out.push_str("END:VEVENT\r\n");
    }
    out.push_str("END:VCALENDAR\r\n");
    out
}
