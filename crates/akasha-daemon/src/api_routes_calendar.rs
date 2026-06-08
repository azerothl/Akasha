//! Calendar routes: external events, ICS, CalDAV sync, accounts.

use crate::api_http::json_response;
use akasha_calendar::{parse_ics, serialize_ics, IcsEvent};
use akasha_store::{
    default_ics_account_id, CalDavAccount, CalDavOutboxRow, ExternalCalendarEvent,
    ExternalCalendarStore, ScheduleStore, TaskStore,
};
use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub struct CalendarRouteCtx<'a> {
    pub store_path: &'a Path,
    pub data_dir: &'a Path,
}

fn parse_json(body: Option<&[u8]>) -> Option<serde_json::Value> {
    body.and_then(|b| serde_json::from_slice(b).ok())
}

fn parse_query(qs: &str, key: &str) -> Option<String> {
    qs.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        if k == key {
            Some(
                urlencoding::decode(v)
                    .map(|s| s.into_owned())
                    .unwrap_or_else(|_| v.to_string()),
            )
        } else {
            None
        }
    })
}

fn parse_range(query_str: Option<&str>) -> (DateTime<Utc>, DateTime<Utc>) {
    let qs = query_str.unwrap_or("");
    let from_ts = parse_query(qs, "from")
        .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
        .map(|dt| dt.with_timezone(&Utc));
    let to_ts = parse_query(qs, "to")
        .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
        .map(|dt| dt.with_timezone(&Utc));
    match (from_ts, to_ts) {
        (Some(f), Some(t)) if f <= t => (f, t),
        _ => {
            let now = Utc::now();
            (now - chrono::Duration::days(7), now)
        }
    }
}

fn open_cal_store(store_path: &Path) -> Option<ExternalCalendarStore> {
    ExternalCalendarStore::open(store_path).ok()
}

fn sync_status_path(data_dir: &Path) -> PathBuf {
    data_dir.join("calendar_sync_status.json")
}

fn calendar_sync_token_ok(headers: &[(String, String)]) -> bool {
    let expected = std::env::var("AKASHA_CALENDAR_SYNC_TOKEN").unwrap_or_default();
    if expected.is_empty() {
        return true;
    }
    headers.iter().any(|(k, v)| {
        k.eq_ignore_ascii_case("x-akasha-calendar-token") && v == &expected
    })
}

fn task_label(initial_message: Option<&String>, task_id: &Uuid) -> String {
    if let Some(msg) = initial_message {
        let trimmed = msg.trim();
        if !trimmed.is_empty() {
            let one_line = trimmed.lines().next().unwrap_or(trimmed);
            let short: String = one_line.chars().take(80).collect();
            return short;
        }
    }
    let s = task_id.to_string();
    let suffix = &s[s.len().saturating_sub(8)..];
    format!("Tâche …{suffix}")
}

fn event_to_json(e: &ExternalCalendarEvent) -> serde_json::Value {
    serde_json::json!({
        "at": e.dtstart.to_rfc3339(),
        "dtend": e.dtend.map(|t| t.to_rfc3339()),
        "type": "external",
        "status": if e.deleted { "deleted" } else { "confirmed" },
        "label": e.summary,
        "summary": e.summary,
        "description": e.description,
        "location": e.location,
        "event_id": e.id.to_string(),
        "account_id": e.account_id.to_string(),
        "uid": e.uid,
        "source": e.source,
        "task_id": "",
    })
}

fn ensure_default_ics_account(store: &ExternalCalendarStore) -> anyhow::Result<()> {
    let id = default_ics_account_id();
    if store.get_account(id)?.is_none() {
        let now = Utc::now();
        store.upsert_account(&CalDavAccount {
            id,
            label: "ICS import".to_string(),
            url: String::new(),
            username: String::new(),
            calendar_path: None,
            enabled: true,
            last_sync_at: None,
            last_sync_error: None,
            sync_token: None,
            provider_id: None,
            auth_method: "app_password".to_string(),
            created_at: now,
            updated_at: now,
        })?;
    }
    Ok(())
}

pub fn query_external_events(
    store_path: &Path,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    account_id: Option<Uuid>,
) -> Vec<serde_json::Value> {
    let Some(cal) = open_cal_store(store_path) else {
        return Vec::new();
    };
    cal.list_events_between(from, to, account_id)
        .unwrap_or_default()
        .iter()
        .map(event_to_json)
        .collect()
}

pub async fn get_merged_calendar_events(store_path: &Path, query_str: Option<&str>) -> String {
    let (from_ts, to_ts) = parse_range(query_str);
    let mut events: Vec<serde_json::Value> = Vec::new();

    events.extend(query_external_events(store_path, from_ts, to_ts, None));

    let task_store = TaskStore::open(store_path);
    if let Ok(schedule_store) = ScheduleStore::open(store_path) {
        if let Ok(runs) = schedule_store.list_task_runs_between(from_ts, to_ts, 500) {
            for r in runs {
                let at = r.started_at.unwrap_or(r.planned_for);
                let label = task_store
                    .as_ref()
                    .ok()
                    .and_then(|ts| ts.get(r.task_id).ok().flatten())
                    .map(|t| task_label(t.initial_message.as_ref(), &r.task_id))
                    .unwrap_or_else(|| task_label(None, &r.task_id));
                events.push(serde_json::json!({
                    "at": at.to_rfc3339(),
                    "task_id": r.task_id.to_string(),
                    "type": "run",
                    "status": r.status.as_str(),
                    "run_id": r.id.to_string(),
                    "planned_for": r.planned_for.to_rfc3339(),
                    "label": label,
                    "schedule_id": r.schedule_id.map(|u| u.to_string()),
                }));
            }
        }
    }
    if let Ok(ref task_store) = task_store {
        if let Ok(tasks) = task_store.list_tasks_created_between(from_ts, to_ts, 500) {
            for t in tasks {
                if t.parent_task_id.is_none() {
                    let label = task_label(t.initial_message.as_ref(), &t.id);
                    events.push(serde_json::json!({
                        "at": t.created_at.to_rfc3339(),
                        "task_id": t.id.to_string(),
                        "type": "ad_hoc",
                        "status": t.status.as_str(),
                        "label": label,
                    }));
                }
            }
        }
    }
    events.sort_by(|a, b| {
        let a_at = a.get("at").and_then(|v| v.as_str()).unwrap_or("");
        let b_at = b.get("at").and_then(|v| v.as_str()).unwrap_or("");
        a_at.cmp(b_at)
    });
    json_response("200 OK", &serde_json::json!({ "events": events }).to_string())
}

pub async fn try_handle(
    method: &str,
    path: &str,
    query_str: Option<&str>,
    body: Option<&[u8]>,
    headers: &[(String, String)],
    ctx: &CalendarRouteCtx<'_>,
) -> Option<String> {
    if !path.starts_with("/api/calendar") {
        return None;
    }

    if method == "GET" && path.starts_with("/api/calendar/events") {
        return Some(get_merged_calendar_events(ctx.store_path, query_str).await);
    }

    if method == "GET" && path == "/api/calendar/providers" {
        let presets = akasha_calendar::caldav_provider_presets();
        let oauth_cfg = crate::calendar_oauth::oauth_config_json(ctx.data_dir);
        let body = serde_json::json!({
            "providers": presets,
            "oauth": oauth_cfg,
        });
        return Some(json_response("200 OK", &body.to_string()));
    }

    if method == "GET" && path == "/api/calendar/oauth/config" {
        let body = crate::calendar_oauth::oauth_config_json(ctx.data_dir);
        return Some(json_response("200 OK", &body.to_string()));
    }

    if method == "POST" && path == "/api/calendar/oauth/start" {
        let Some(j) = parse_json(body) else {
            return Some(json_response("400 Bad Request", r#"{"error":"invalid_json"}"#));
        };
        let provider_id = j
            .get("provider_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let label = j.get("label").and_then(|v| v.as_str()).map(String::from);
        match crate::calendar_oauth::oauth_start(ctx.data_dir, provider_id, label).await {
            Ok(out) => return Some(json_response("200 OK", &out.to_string())),
            Err(e) => {
                return Some(json_response(
                    "400 Bad Request",
                    &serde_json::json!({ "error": e }).to_string(),
                ))
            }
        }
    }

    if method == "GET" && path == "/api/calendar/oauth/status" {
        let state = query_str
            .and_then(|qs| parse_query(qs, "state"))
            .unwrap_or_default();
        let body = crate::calendar_oauth::oauth_status(ctx.data_dir, &state).await;
        return Some(json_response("200 OK", &body.to_string()));
    }

    if method == "GET" && path == "/api/calendar/oauth/callback" {
        let qs = query_str.unwrap_or("");
        let code = parse_query(qs, "code").unwrap_or_default();
        let state = parse_query(qs, "state").unwrap_or_default();
        let locale = parse_query(qs, "locale").unwrap_or_else(|| "fr".to_string());
        if code.is_empty() || state.is_empty() {
            let html = crate::calendar_oauth::oauth_error_html("Paramètres manquants.");
            return Some(format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                html.len(),
                html
            ));
        }
        match crate::calendar_oauth::oauth_callback(ctx.data_dir, ctx.store_path, &code, &state).await
        {
            Ok(_) => {
                let html = crate::calendar_oauth::oauth_success_html(&locale);
                return Some(format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    html.len(),
                    html
                ));
            }
            Err(e) => {
                let html = crate::calendar_oauth::oauth_error_html(&e);
                return Some(format!(
                    "HTTP/1.1 400 Bad Request\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    html.len(),
                    html
                ));
            }
        }
    }

    if method == "GET" && path == "/api/calendar/accounts" {
        let Some(store) = open_cal_store(ctx.store_path) else {
            return Some(json_response(
                "500 Internal Server Error",
                r#"{"error":"store"}"#,
            ));
        };
        let accounts = store.list_accounts().unwrap_or_default();
        let body = serde_json::json!({ "accounts": accounts });
        return Some(json_response("200 OK", &body.to_string()));
    }

    if method == "POST" && path == "/api/calendar/accounts" {
        let j = parse_json(body)?;
        let now = Utc::now();
        let id = j
            .get("id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
            .unwrap_or_else(Uuid::new_v4);
        let provider_id = j
            .get("provider_id")
            .and_then(|v| v.as_str())
            .map(String::from);
        let default_label = provider_id
            .as_deref()
            .and_then(akasha_calendar::preset_by_id)
            .map(|p| p.name_en.clone())
            .unwrap_or_else(|| "CalDAV".to_string());
        let account = CalDavAccount {
            id,
            label: j
                .get("label")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
                .map(String::from)
                .unwrap_or(default_label),
            url: j.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            username: j
                .get("username")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            calendar_path: j
                .get("calendar_path")
                .and_then(|v| v.as_str())
                .map(String::from),
            enabled: j.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true),
            last_sync_at: None,
            last_sync_error: None,
            sync_token: None,
            provider_id,
            auth_method: j
                .get("auth_method")
                .and_then(|v| v.as_str())
                .unwrap_or("app_password")
                .to_string(),
            created_at: now,
            updated_at: now,
        };
        if account.url.is_empty() || account.username.is_empty() {
            return Some(json_response(
                "400 Bad Request",
                r#"{"error":"url_and_username_required"}"#,
            ));
        }
        if account.auth_method == "oauth" {
            return Some(json_response(
                "400 Bad Request",
                r#"{"error":"use_oauth_flow_for_oauth_accounts"}"#,
            ));
        }
        let Some(store) = open_cal_store(ctx.store_path) else {
            return Some(json_response(
                "500 Internal Server Error",
                r#"{"error":"store"}"#,
            ));
        };
        if store.upsert_account(&account).is_err() {
            return Some(json_response(
                "500 Internal Server Error",
                r#"{"error":"upsert_failed"}"#,
            ));
        }
        return Some(json_response(
            "200 OK",
            &serde_json::to_string(&account).unwrap_or_default(),
        ));
    }

    if method == "DELETE" && path.starts_with("/api/calendar/accounts/") {
        let id_str = path.trim_start_matches("/api/calendar/accounts/").trim();
        let Ok(id) = Uuid::parse_str(id_str) else {
            return Some(json_response("400 Bad Request", r#"{"error":"invalid_id"}"#));
        };
        let Some(store) = open_cal_store(ctx.store_path) else {
            return Some(json_response(
                "500 Internal Server Error",
                r#"{"error":"store"}"#,
            ));
        };
        let ok = store.delete_account(id).unwrap_or(false);
        return Some(if ok {
            json_response("200 OK", r#"{"ok":true}"#)
        } else {
            json_response("404 Not Found", r#"{"error":"not_found"}"#)
        });
    }

    if method == "GET" && path == "/api/calendar/sync-status" {
        let path = sync_status_path(ctx.data_dir);
        if path.is_file() {
            if let Ok(text) = std::fs::read_to_string(&path) {
                return Some(json_response("200 OK", &text));
            }
        }
        let body = serde_json::json!({
            "connected": false,
            "sidecar_enabled": std::env::var("CALDAV_SIDECAR_ENABLED").ok().as_deref()
                .map(|s| matches!(s, "1" | "true" | "yes" | "on")).unwrap_or(false),
            "last_sync_at": null,
            "last_error": null,
        });
        return Some(json_response("200 OK", &body.to_string()));
    }

    if method == "GET" && path.starts_with("/api/calendar/ics") {
        let (from, to) = parse_range(query_str);
        let account_id = query_str
            .and_then(|qs| parse_query(qs, "account_id"))
            .and_then(|s| Uuid::parse_str(&s).ok());
        let Some(store) = open_cal_store(ctx.store_path) else {
            return Some(json_response(
                "500 Internal Server Error",
                r#"{"error":"store"}"#,
            ));
        };
        let events = store.list_events_between(from, to, account_id).unwrap_or_default();
        let ics_events: Vec<IcsEvent> = events.iter().map(|e| {
            IcsEvent {
                uid: e.uid.clone(),
                summary: e.summary.clone(),
                description: e.description.clone(),
                location: e.location.clone(),
                dtstart: e.dtstart,
                dtend: e.dtend,
                rrule: e.rrule.clone(),
            }
        }).collect();
        let text = serialize_ics(&ics_events, "Akasha");
        return Some(format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/calendar; charset=utf-8\r\nContent-Length: {}\r\n\r\n{}",
            text.len(),
            text
        ));
    }

    if method == "POST" && path == "/api/calendar/ics" {
        let text = body
            .and_then(|b| std::str::from_utf8(b).ok().map(String::from))
            .or_else(|| {
                parse_json(body).and_then(|j| {
                    j.get("ics")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                })
            })
            .unwrap_or_default();
        if text.trim().is_empty() {
            return Some(json_response(
                "400 Bad Request",
                r#"{"error":"ics_body_required"}"#,
            ));
        }
        let account_id = parse_json(body)
            .and_then(|j| j.get("account_id").and_then(|v| v.as_str()).map(String::from))
            .and_then(|s| Uuid::parse_str(&s).ok())
            .unwrap_or_else(default_ics_account_id);
        let parsed = match parse_ics(&text) {
            Ok(v) => v,
            Err(e) => {
                return Some(json_response(
                    "400 Bad Request",
                    &serde_json::json!({ "error": e.to_string() }).to_string(),
                ));
            }
        };
        let Some(store) = open_cal_store(ctx.store_path) else {
            return Some(json_response(
                "500 Internal Server Error",
                r#"{"error":"store"}"#,
            ));
        };
        let _ = ensure_default_ics_account(&store);
        let now = Utc::now();
        let mut imported = 0u32;
        for ev in parsed {
            let row = ExternalCalendarEvent {
                id: Uuid::new_v4(),
                account_id,
                uid: ev.uid,
                href: None,
                etag: None,
                summary: ev.summary,
                description: ev.description,
                location: ev.location,
                dtstart: ev.dtstart,
                dtend: ev.dtend,
                timezone: None,
                rrule: ev.rrule,
                exdates_json: "[]".to_string(),
                source: "ics_import".to_string(),
                synced_at: Some(now),
                deleted: false,
            };
            if store.upsert_event(&row).is_ok() {
                imported += 1;
            }
        }
        return Some(json_response(
            "200 OK",
            &serde_json::json!({ "imported": imported }).to_string(),
        ));
    }

    if method == "POST" && path == "/api/calendar/external/sync" {
        if !calendar_sync_token_ok(headers) {
            return Some(json_response("401 Unauthorized", r#"{"error":"invalid_sync_token"}"#));
        }
        let j = parse_json(body)?;
        let account_id = j
            .get("account_id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
            .unwrap_or_else(default_ics_account_id);
        let Some(store) = open_cal_store(ctx.store_path) else {
            return Some(json_response(
                "500 Internal Server Error",
                r#"{"error":"store"}"#,
            ));
        };
        let now = Utc::now();
        if let Some(arr) = j.get("events").and_then(|v| v.as_array()) {
            for item in arr {
                let uid = item
                    .get("uid")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if uid.is_empty() {
                    continue;
                }
                let dtstart = item
                    .get("dtstart")
                    .and_then(|v| v.as_str())
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|d| d.with_timezone(&Utc))
                    .unwrap_or(now);
                let dtend = item
                    .get("dtend")
                    .and_then(|v| v.as_str())
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|d| d.with_timezone(&Utc));
                let row = ExternalCalendarEvent {
                    id: Uuid::new_v4(),
                    account_id,
                    uid,
                    href: item.get("href").and_then(|v| v.as_str()).map(String::from),
                    etag: item.get("etag").and_then(|v| v.as_str()).map(String::from),
                    summary: item
                        .get("summary")
                        .and_then(|v| v.as_str())
                        .unwrap_or("(event)")
                        .to_string(),
                    description: item
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    location: item
                        .get("location")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    dtstart,
                    dtend,
                    timezone: item
                        .get("timezone")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    rrule: item.get("rrule").and_then(|v| v.as_str()).map(String::from),
                    exdates_json: "[]".to_string(),
                    source: "caldav".to_string(),
                    synced_at: Some(now),
                    deleted: false,
                };
                let _ = store.upsert_event(&row);
            }
        }
        if let Some(arr) = j.get("deleted_hrefs").and_then(|v| v.as_array()) {
            let hrefs: Vec<String> = arr
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            let _ = store.mark_deleted_by_hrefs(account_id, &hrefs);
        }
        let sync_token = j.get("sync_token").and_then(|v| v.as_str());
        let sync_error = j.get("error").and_then(|v| v.as_str());
        let _ = store.update_account_sync_status(account_id, sync_token, sync_error);
        let status = serde_json::json!({
            "connected": sync_error.is_none(),
            "account_id": account_id.to_string(),
            "last_sync_at": now.to_rfc3339(),
            "last_error": sync_error,
            "sync_token": sync_token,
        });
        let _ = std::fs::write(
            sync_status_path(ctx.data_dir),
            serde_json::to_string_pretty(&status).unwrap_or_default(),
        );
        return Some(json_response(
            "200 OK",
            &serde_json::json!({ "ok": true, "synced_at": now.to_rfc3339() }).to_string(),
        ));
    }

    if method == "GET" && path == "/api/calendar/external/outbox" {
        let Some(store) = open_cal_store(ctx.store_path) else {
            return Some(json_response(
                "500 Internal Server Error",
                r#"{"error":"store"}"#,
            ));
        };
        let rows = store.list_outbox_all(100).unwrap_or_default();
        return Some(json_response(
            "200 OK",
            &serde_json::json!({ "outbox": rows }).to_string(),
        ));
    }

    if method == "GET" && path == "/api/calendar/external/outbox/pending" {
        if !calendar_sync_token_ok(headers) {
            return Some(json_response("401 Unauthorized", r#"{"error":"invalid_sync_token"}"#));
        }
        let account_id = query_str
            .and_then(|qs| parse_query(qs, "account_id"))
            .and_then(|s| Uuid::parse_str(&s).ok());
        let Some(store) = open_cal_store(ctx.store_path) else {
            return Some(json_response(
                "500 Internal Server Error",
                r#"{"error":"store"}"#,
            ));
        };
        let rows = store.list_pending_outbox(account_id).unwrap_or_default();
        return Some(json_response(
            "200 OK",
            &serde_json::json!({ "outbox": rows }).to_string(),
        ));
    }

    if method == "POST" && path == "/api/calendar/external/outbox/ack" {
        if !calendar_sync_token_ok(headers) {
            return Some(json_response("401 Unauthorized", r#"{"error":"invalid_sync_token"}"#));
        }
        let j = parse_json(body)?;
        let id = j
            .get("id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())?;
        let error = j.get("error").and_then(|v| v.as_str());
        let Some(store) = open_cal_store(ctx.store_path) else {
            return Some(json_response(
                "500 Internal Server Error",
                r#"{"error":"store"}"#,
            ));
        };
        let _ = store.mark_outbox_applied(id, error);
        return Some(json_response("200 OK", r#"{"ok":true}"#));
    }

    if method == "POST" && path == "/api/calendar/events" {
        let j = parse_json(body)?;
        let Some(store) = open_cal_store(ctx.store_path) else {
            return Some(json_response(
                "500 Internal Server Error",
                r#"{"error":"store"}"#,
            ));
        };
        let account_id = j
            .get("account_id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
            .unwrap_or_else(default_ics_account_id);
        let _ = ensure_default_ics_account(&store);
        let summary = j
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("Event")
            .to_string();
        let dtstart = j
            .get("dtstart")
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);
        let dtend = j
            .get("dtend")
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc));
        let uid = j
            .get("uid")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| format!("akasha-{}", Uuid::new_v4()));
        let id = Uuid::new_v4();
        let row = ExternalCalendarEvent {
            id,
            account_id,
            uid: uid.clone(),
            href: None,
            etag: None,
            summary,
            description: j.get("description").and_then(|v| v.as_str()).map(String::from),
            location: j.get("location").and_then(|v| v.as_str()).map(String::from),
            dtstart,
            dtend,
            timezone: None,
            rrule: j.get("rrule").and_then(|v| v.as_str()).map(String::from),
            exdates_json: "[]".to_string(),
            source: "local".to_string(),
            synced_at: Some(Utc::now()),
            deleted: false,
        };
        if store.upsert_event(&row).is_err() {
            return Some(json_response(
                "500 Internal Server Error",
                r#"{"error":"create_failed"}"#,
            ));
        }
        let payload = serde_json::to_string(&row).unwrap_or_default();
        let _ = store.enqueue_outbox(&CalDavOutboxRow {
            id: Uuid::new_v4(),
            account_id,
            event_id: Some(id),
            operation: "create".to_string(),
            payload_json: payload,
            created_at: Utc::now(),
            applied_at: None,
            error: None,
        });
        return Some(json_response(
            "200 OK",
            &serde_json::to_string(&row).unwrap_or_default(),
        ));
    }

    if method == "PUT" && path.starts_with("/api/calendar/events/") {
        let id_str = path.trim_start_matches("/api/calendar/events/").trim();
        let Ok(id) = Uuid::parse_str(id_str) else {
            return Some(json_response("400 Bad Request", r#"{"error":"invalid_id"}"#));
        };
        let j = parse_json(body)?;
        let Some(store) = open_cal_store(ctx.store_path) else {
            return Some(json_response(
                "500 Internal Server Error",
                r#"{"error":"store"}"#,
            ));
        };
        let Some(mut row) = store.get_event(id).ok().flatten() else {
            return Some(json_response("404 Not Found", r#"{"error":"not_found"}"#));
        };
        if let Some(s) = j.get("summary").and_then(|v| v.as_str()) {
            row.summary = s.to_string();
        }
        if let Some(s) = j.get("description").and_then(|v| v.as_str()) {
            row.description = Some(s.to_string());
        }
        if let Some(s) = j.get("location").and_then(|v| v.as_str()) {
            row.location = Some(s.to_string());
        }
        if let Some(s) = j.get("dtstart").and_then(|v| v.as_str()) {
            if let Ok(d) = DateTime::parse_from_rfc3339(s) {
                row.dtstart = d.with_timezone(&Utc);
            }
        }
        if let Some(s) = j.get("dtend").and_then(|v| v.as_str()) {
            if let Ok(d) = DateTime::parse_from_rfc3339(s) {
                row.dtend = Some(d.with_timezone(&Utc));
            }
        }
        row.synced_at = Some(Utc::now());
        if store.upsert_event(&row).is_err() {
            return Some(json_response(
                "500 Internal Server Error",
                r#"{"error":"update_failed"}"#,
            ));
        }
        let payload = serde_json::to_string(&row).unwrap_or_default();
        let _ = store.enqueue_outbox(&CalDavOutboxRow {
            id: Uuid::new_v4(),
            account_id: row.account_id,
            event_id: Some(id),
            operation: "update".to_string(),
            payload_json: payload,
            created_at: Utc::now(),
            applied_at: None,
            error: None,
        });
        return Some(json_response(
            "200 OK",
            &serde_json::to_string(&row).unwrap_or_default(),
        ));
    }

    if method == "DELETE" && path.starts_with("/api/calendar/events/") {
        let id_str = path.trim_start_matches("/api/calendar/events/").trim();
        let Ok(id) = Uuid::parse_str(id_str) else {
            return Some(json_response("400 Bad Request", r#"{"error":"invalid_id"}"#));
        };
        let Some(store) = open_cal_store(ctx.store_path) else {
            return Some(json_response(
                "500 Internal Server Error",
                r#"{"error":"store"}"#,
            ));
        };
        let row = store.get_event(id).ok().flatten();
        if store.soft_delete_event(id).unwrap_or(false) {
            if let Some(ref r) = row {
                let payload = serde_json::json!({ "event_id": id.to_string(), "href": r.href, "uid": r.uid });
                let _ = store.enqueue_outbox(&CalDavOutboxRow {
                    id: Uuid::new_v4(),
                    account_id: r.account_id,
                    event_id: Some(id),
                    operation: "delete".to_string(),
                    payload_json: payload.to_string(),
                    created_at: Utc::now(),
                    applied_at: None,
                    error: None,
                });
            }
            return Some(json_response("200 OK", r#"{"ok":true}"#));
        }
        return Some(json_response("404 Not Found", r#"{"error":"not_found"}"#));
    }

    None
}

/// Compact listing for agent `calendar_query` tool.
pub fn calendar_query_compact(
    store_path: &Path,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    account_id: Option<Uuid>,
) -> String {
    let Some(store) = open_cal_store(store_path) else {
        return "[calendar_query] store unavailable".to_string();
    };
    let events = store
        .list_events_between(from, to, account_id)
        .unwrap_or_default();
    if events.is_empty() {
        return format!(
            "[calendar_query] 0 external events between {} and {}",
            from.to_rfc3339(),
            to.to_rfc3339()
        );
    }
    let lines: Vec<String> = events
        .iter()
        .take(30)
        .map(|e| {
            format!(
                "- {} | {} → {}",
                e.summary,
                e.dtstart.to_rfc3339(),
                e.dtend
                    .map(|t| t.to_rfc3339())
                    .unwrap_or_else(|| "?".to_string())
            )
        })
        .collect();
    format!(
        "[calendar_query] {} event(s):\n{}",
        events.len().min(30),
        lines.join("\n")
    )
}
