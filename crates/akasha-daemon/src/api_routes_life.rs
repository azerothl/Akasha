//! P7 Life layer HTTP routes: notify, packs, NL schedule, skill draft.

use crate::api_http::json_response;
use crate::life_layer::{
    self, find_pack_schedule, morning_brief_template, overnight_pack_template, pack_status_from_schedule,
    parse_nl_schedule, skill_draft_filename, NAME_MORNING_BRIEF, NAME_OVERNIGHT_PACK, TAG_MORNING_BRIEF,
    TAG_OVERNIGHT_PACK,
};
use akasha_store::{ScheduleStore, TaskStore};
use chrono::{Duration, Utc};
use std::path::Path;
use uuid::Uuid;

pub struct LifeRouteCtx<'a> {
    pub store_path: &'a Path,
    pub data_dir: &'a Path,
}

pub async fn try_handle(
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    ctx: &LifeRouteCtx<'_>,
) -> Option<String> {
    if method == "POST" && path == "/api/channels/notify" {
        return Some(post_channels_notify(ctx.data_dir, body).await);
    }
    if method == "GET" && path == "/api/life/packs" {
        return Some(get_life_packs(ctx.store_path).await);
    }
    if method == "POST" && path == "/api/life/morning-brief" {
        return Some(upsert_morning_brief(ctx.store_path, body).await);
    }
    if method == "POST" && path == "/api/life/overnight-pack" {
        return Some(upsert_overnight_pack(ctx.store_path, body).await);
    }
    if method == "POST" && path == "/api/schedules/from-nl" {
        return Some(post_schedules_from_nl(ctx.store_path, body).await);
    }
    if method == "POST" && path == "/api/skills/draft-from-task" {
        return Some(post_skill_draft_from_task(ctx.store_path, ctx.data_dir, body).await);
    }
    if method == "GET" && path == "/api/process/watch/subscriptions" {
        let list = crate::process_watch::list_subscriptions().await;
        return Some(json_response(
            "200 OK",
            &serde_json::json!({ "subscriptions": list }).to_string(),
        ));
    }
    if method == "POST" && path == "/api/process/watch/subscriptions" {
        return Some(post_process_watch_sub(body).await);
    }
    if method == "DELETE" && path.starts_with("/api/process/watch/subscriptions/") {
        let id = path
            .trim_start_matches("/api/process/watch/subscriptions/")
            .trim();
        let ok = crate::process_watch::remove_subscription(id).await;
        return Some(json_response(
            if ok { "200 OK" } else { "404 Not Found" },
            &serde_json::json!({ "deleted": ok }).to_string(),
        ));
    }
    // P6-B3: cron / schedule watch exit → wakeup
    if method == "GET" && path == "/api/schedules/watch/recent" {
        let limit = 50usize;
        let ev = crate::cron_watch::recent(limit).await;
        return Some(json_response(
            "200 OK",
            &serde_json::json!({ "events": ev }).to_string(),
        ));
    }
    if method == "GET" && path == "/api/schedules/watch/subscriptions" {
        let list = crate::cron_watch::list_subscriptions().await;
        return Some(json_response(
            "200 OK",
            &serde_json::json!({ "subscriptions": list }).to_string(),
        ));
    }
    if method == "POST" && path == "/api/schedules/watch/subscriptions" {
        return Some(post_cron_watch_sub(body).await);
    }
    if method == "DELETE" && path.starts_with("/api/schedules/watch/subscriptions/") {
        let id = path
            .trim_start_matches("/api/schedules/watch/subscriptions/")
            .trim();
        let ok = crate::cron_watch::remove_subscription(id).await;
        return Some(json_response(
            if ok { "200 OK" } else { "404 Not Found" },
            &serde_json::json!({ "deleted": ok }).to_string(),
        ));
    }
    None
}

async fn post_cron_watch_sub(body: Option<&[u8]>) -> String {
    let json: serde_json::Value = match body.and_then(|b| serde_json::from_slice(b).ok()) {
        Some(j) => j,
        None => return json_response("400 Bad Request", r#"{"error":"invalid_json"}"#),
    };
    let id = json
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let on_status = json
        .get("on_status")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let sub = crate::cron_watch::CronWatchSubscription {
        id: id.clone(),
        session_id: json
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        schedule_id: json
            .get("schedule_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        name_contains: json
            .get("name_contains")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        on_status,
        on_failure: json
            .get("on_failure")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        message: json
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("Cron watch condition matched")
            .to_string(),
    };
    crate::cron_watch::add_subscription(sub.clone()).await;
    json_response(
        "201 Created",
        &serde_json::json!({ "ok": true, "subscription": sub }).to_string(),
    )
}

async fn post_process_watch_sub(body: Option<&[u8]>) -> String {
    let json: serde_json::Value = match body.and_then(|b| serde_json::from_slice(b).ok()) {
        Some(j) => j,
        None => return json_response("400 Bad Request", r#"{"error":"invalid_json"}"#),
    };
    let id = json
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let sub = crate::process_watch::ProcessWatchSubscription {
        id: id.clone(),
        session_id: json
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        cmd_contains: json
            .get("cmd_contains")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        exit_code: json.get("exit_code").and_then(|v| v.as_i64()).map(|n| n as i32),
        on_failure: json
            .get("on_failure")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        message: json
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("Process watch condition matched")
            .to_string(),
    };
    crate::process_watch::add_subscription(sub.clone()).await;
    json_response(
        "201 Created",
        &serde_json::json!({ "ok": true, "subscription": sub }).to_string(),
    )
}

async fn post_channels_notify(data_dir: &Path, body: Option<&[u8]>) -> String {
    let json: serde_json::Value = match body.and_then(|b| serde_json::from_slice(b).ok()) {
        Some(j) => j,
        None => return json_response("400 Bad Request", r#"{"error":"invalid_json"}"#),
    };
    let channel = json
        .get("channel")
        .and_then(|v| v.as_str())
        .unwrap_or("telegram")
        .trim();
    let text = json
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if text.is_empty() {
        return json_response("400 Bad Request", r#"{"error":"text_required"}"#);
    }
    match life_layer::notify_channel(data_dir, channel, text).await {
        Ok(()) => json_response(
            "200 OK",
            &serde_json::json!({ "ok": true, "channel": channel }).to_string(),
        ),
        Err(e) => json_response(
            "502 Bad Gateway",
            &serde_json::json!({ "error": "notify_failed", "detail": e }).to_string(),
        ),
    }
}

async fn get_life_packs(store_path: &Path) -> String {
    let store = match ScheduleStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let schedules = store.list_schedules().unwrap_or_default();
    let morning = find_pack_schedule(&schedules, NAME_MORNING_BRIEF);
    let overnight = find_pack_schedule(&schedules, NAME_OVERNIGHT_PACK);
    let body = serde_json::json!({
        "packs": [
            pack_status_from_schedule(TAG_MORNING_BRIEF, morning.as_ref(), 8),
            pack_status_from_schedule(TAG_OVERNIGHT_PACK, overnight.as_ref(), 2),
        ]
    });
    json_response("200 OK", &body.to_string())
}

fn parse_pack_body(body: Option<&[u8]>) -> (bool, u32, u32, String, Option<String>) {
    let json: serde_json::Value = body
        .and_then(|b| serde_json::from_slice(b).ok())
        .unwrap_or(serde_json::json!({}));
    let enabled = json.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
    let hour = json
        .get("hour_local")
        .and_then(|v| v.as_u64())
        .unwrap_or(8) as u32;
    let minute = json
        .get("minute_local")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let timezone = json
        .get("timezone")
        .and_then(|v| v.as_str())
        .unwrap_or("UTC")
        .to_string();
    let notify = json
        .get("notify_channel")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty());
    (enabled, hour.min(23), minute.min(59), timezone, notify)
}

async fn upsert_morning_brief(store_path: &Path, body: Option<&[u8]>) -> String {
    let (enabled, hour, minute, timezone, notify) = parse_pack_body(body);
    let store = match ScheduleStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let schedules = store.list_schedules().unwrap_or_default();
    if let Some(mut existing) = find_pack_schedule(&schedules, NAME_MORNING_BRIEF) {
        existing.enabled = enabled;
        existing.timezone = timezone.clone();
        existing.rrule = life_layer::daily_rrule(hour, minute);
        existing.channel_context = Some(life_layer::build_channel_context(
            TAG_MORNING_BRIEF,
            "Tu es le brief matinal Akasha. Produis un digest court (FR) avec sections : (1) Agenda / wakeups du jour si disponibles via outils calendrier, (2) Tâches actives ou en cours, (3) Derniers rapports de schedules/mission si présents, (4) 3 priorités suggérées. Sois concis (max ~400 mots).",
            notify.as_deref().or(Some("telegram")),
        ));
        existing.updated_at = Utc::now();
        if store.update_schedule(&existing).is_err() {
            return json_response("500 Internal Server Error", r#"{"error":"store"}"#);
        }
        let status = pack_status_from_schedule(TAG_MORNING_BRIEF, Some(&existing), hour);
        return json_response(
            "200 OK",
            &serde_json::json!({ "ok": true, "pack": status }).to_string(),
        );
    }
    let sched = morning_brief_template(hour, minute, &timezone, notify.as_deref(), enabled);
    if store.insert_schedule(&sched).is_err() {
        return json_response("500 Internal Server Error", r#"{"error":"store"}"#);
    }
    let status = pack_status_from_schedule(TAG_MORNING_BRIEF, Some(&sched), hour);
    json_response(
        "201 Created",
        &serde_json::json!({ "ok": true, "pack": status }).to_string(),
    )
}

async fn upsert_overnight_pack(store_path: &Path, body: Option<&[u8]>) -> String {
    let (enabled, hour, minute, timezone, _) = parse_pack_body(body);
    let hour = if body
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
        .and_then(|j| j.get("hour_local").and_then(|v| v.as_u64()))
        .is_none()
    {
        2
    } else {
        hour
    };
    let store = match ScheduleStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let schedules = store.list_schedules().unwrap_or_default();
    if let Some(mut existing) = find_pack_schedule(&schedules, NAME_OVERNIGHT_PACK) {
        existing.enabled = enabled;
        existing.timezone = timezone.clone();
        existing.rrule = life_layer::daily_rrule(hour, minute);
        existing.channel_context = Some(life_layer::build_channel_context(
            TAG_OVERNIGHT_PACK,
            "Tu es le pack nuit Akasha. Exécute un passage « overnight » : (1) Résume l'état des tâches et schedules, (2) Vérifie le calendrier du lendemain si connecté, (3) Propose des follow-ups concrets, (4) Écris un rapport Markdown structuré. N'envoie pas d'emails. Utilise les skills installés si pertinents.",
            None,
        ));
        existing.updated_at = Utc::now();
        if store.update_schedule(&existing).is_err() {
            return json_response("500 Internal Server Error", r#"{"error":"store"}"#);
        }
        let status = pack_status_from_schedule(TAG_OVERNIGHT_PACK, Some(&existing), hour);
        return json_response(
            "200 OK",
            &serde_json::json!({ "ok": true, "pack": status }).to_string(),
        );
    }
    let sched = overnight_pack_template(hour, minute, &timezone, enabled);
    if store.insert_schedule(&sched).is_err() {
        return json_response("500 Internal Server Error", r#"{"error":"store"}"#);
    }
    let status = pack_status_from_schedule(TAG_OVERNIGHT_PACK, Some(&sched), hour);
    json_response(
        "201 Created",
        &serde_json::json!({ "ok": true, "pack": status }).to_string(),
    )
}

async fn post_schedules_from_nl(store_path: &Path, body: Option<&[u8]>) -> String {
    let json: serde_json::Value = match body.and_then(|b| serde_json::from_slice(b).ok()) {
        Some(j) => j,
        None => return json_response("400 Bad Request", r#"{"error":"invalid_json"}"#),
    };
    let text = json
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if text.is_empty() {
        return json_response("400 Bad Request", r#"{"error":"text_required"}"#);
    }
    let commit = json.get("commit").and_then(|v| v.as_bool()).unwrap_or(false);
    let preview = parse_nl_schedule(text);
    if !commit {
        return json_response(
            "200 OK",
            &serde_json::json!({ "preview": preview, "committed": false }).to_string(),
        );
    }
    let store = match ScheduleStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    // Upsert by canonical pack name when applicable.
    if preview.name == NAME_MORNING_BRIEF || preview.name == NAME_OVERNIGHT_PACK {
        let schedules = store.list_schedules().unwrap_or_default();
        if let Some(mut existing) = find_pack_schedule(&schedules, &preview.name) {
            existing.enabled = true;
            existing.timezone = preview.timezone.clone();
            existing.rrule = preview.rrule.clone();
            existing.description = preview.description.clone();
            existing.channel_context = Some(life_layer::build_channel_context(
                preview.tag.as_deref().unwrap_or(""),
                &preview.message,
                preview.notify_channel.as_deref(),
            ));
            existing.updated_at = Utc::now();
            if store.update_schedule(&existing).is_err() {
                return json_response("500 Internal Server Error", r#"{"error":"store"}"#);
            }
            return json_response(
                "200 OK",
                &serde_json::json!({
                    "preview": preview,
                    "committed": true,
                    "schedule_id": existing.id.to_string(),
                })
                .to_string(),
            );
        }
    }
    let now = Utc::now();
    let id = Uuid::new_v4();
    let schedule = akasha_store::Schedule {
        id,
        name: preview.name.clone(),
        description: preview.description.clone(),
        enabled: true,
        timezone: preview.timezone.clone(),
        rrule: preview.rrule.clone(),
        interval_seconds: None,
        start_at: now - Duration::minutes(1),
        end_at: None,
        channel_context: Some(life_layer::build_channel_context(
            preview.tag.as_deref().unwrap_or("nl_schedule"),
            &preview.message,
            preview.notify_channel.as_deref(),
        )),
        created_at: now,
        updated_at: now,
    };
    if store.insert_schedule(&schedule).is_err() {
        return json_response("500 Internal Server Error", r#"{"error":"store"}"#);
    }
    json_response(
        "201 Created",
        &serde_json::json!({
            "preview": preview,
            "committed": true,
            "schedule_id": id.to_string(),
        })
        .to_string(),
    )
}

async fn post_skill_draft_from_task(
    store_path: &Path,
    data_dir: &Path,
    body: Option<&[u8]>,
) -> String {
    let json: serde_json::Value = match body.and_then(|b| serde_json::from_slice(b).ok()) {
        Some(j) => j,
        None => return json_response("400 Bad Request", r#"{"error":"invalid_json"}"#),
    };
    let task_id = match json
        .get("task_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
    {
        Some(id) => id,
        None => return json_response("400 Bad Request", r#"{"error":"task_id_required"}"#),
    };
    let store = match TaskStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let task = match store.get(task_id) {
        Ok(Some(t)) => t,
        Ok(None) => return json_response("404 Not Found", r#"{"error":"task_not_found"}"#),
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let progress = store.get_progress(task_id).unwrap_or_default();
    let last = progress
        .iter()
        .rev()
        .find(|(_, m)| !m.trim().is_empty())
        .map(|(_, m)| m.as_str())
        .unwrap_or("(no progress text)");
    let initial = task.initial_message.as_deref().unwrap_or("");
    let draft = format!(
        "---\nname: draft-from-task-{}\ndescription: Auto-draft from completed task (Hermes-inspired H5)\n---\n\n# Skill draft\n\n## Goal\n\n{}\n\n## Observed outcome\n\n{}\n\n## Suggested steps\n\n1. Re-run the successful approach from the task.\n2. Confirm tools/policy allow the same actions.\n3. Install via skills catalog when ready.\n",
        &task_id.to_string()[..8],
        if initial.is_empty() {
            "(no initial message)"
        } else {
            initial
        },
        last
    );
    let drafts_dir = data_dir.join("skill_drafts");
    let _ = std::fs::create_dir_all(&drafts_dir);
    let filename = skill_draft_filename(task_id);
    let path = drafts_dir.join(&filename);
    if std::fs::write(&path, &draft).is_err() {
        return json_response("500 Internal Server Error", r#"{"error":"write_failed"}"#);
    }
    json_response(
        "201 Created",
        &serde_json::json!({
            "ok": true,
            "path": path.display().to_string(),
            "filename": filename,
            "content": draft,
            "installed": false,
        })
        .to_string(),
    )
}
