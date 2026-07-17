//! P7 Life layer: overnight pack, morning brief, channel notify, NL schedule parse.

use akasha_store::Schedule;
use akasha_vault::Vault;
use chrono::{Datelike, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;
use uuid::Uuid;

pub const TAG_MORNING_BRIEF: &str = "morning_brief";
pub const TAG_OVERNIGHT_PACK: &str = "overnight_pack";
pub const NAME_MORNING_BRIEF: &str = "morning_brief";
pub const NAME_OVERNIGHT_PACK: &str = "overnight_pack";

const MORNING_BRIEF_PROMPT: &str = "\
Tu es le brief matinal Akasha. Produis un digest court (FR) avec sections : \
(1) Agenda / wakeups du jour si disponibles via outils calendrier, \
(2) Tâches actives ou en cours, \
(3) Derniers rapports de schedules/mission si présents, \
(4) 3 priorités suggérées. Sois concis (max ~400 mots).";

const OVERNIGHT_PACK_PROMPT: &str = "\
Tu es le pack nuit Akasha. Exécute un passage « overnight » : \
(1) Résume l'état des tâches et schedules, \
(2) Vérifie le calendrier du lendemain si connecté, \
(3) Propose des follow-ups concrets, \
(4) Écris un rapport Markdown structuré. N'envoie pas d'emails. Utilise les skills installés si pertinents.";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelNotifySpec {
    pub channel: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifeChannelContext {
    pub message: String,
    pub tag: String,
    #[serde(default)]
    pub notify: Option<ChannelNotifySpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifePackStatus {
    pub id: String,
    pub enabled: bool,
    pub schedule_id: Option<String>,
    pub hour_local: u32,
    pub minute_local: u32,
    pub timezone: String,
    pub notify_channel: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NlSchedulePreview {
    pub name: String,
    pub description: String,
    pub rrule: String,
    pub timezone: String,
    pub hour_local: u32,
    pub minute_local: u32,
    pub tag: Option<String>,
    pub notify_channel: Option<String>,
    pub message: String,
    pub confidence: f32,
}

pub fn build_channel_context(tag: &str, message: &str, notify_channel: Option<&str>) -> String {
    let ctx = LifeChannelContext {
        message: message.to_string(),
        tag: tag.to_string(),
        notify: notify_channel.map(|c| ChannelNotifySpec {
            channel: c.to_string(),
        }),
    };
    serde_json::to_string(&ctx).unwrap_or_else(|_| message.to_string())
}

pub fn parse_life_context(raw: Option<&str>) -> Option<LifeChannelContext> {
    let s = raw?.trim();
    if s.is_empty() {
        return None;
    }
    serde_json::from_str::<LifeChannelContext>(s).ok()
}

pub fn schedule_wants_notify(schedule: &Schedule) -> Option<String> {
    parse_life_context(schedule.channel_context.as_deref())
        .and_then(|c| c.notify.map(|n| n.channel))
        .or_else(|| {
            let name = schedule.name.trim().to_lowercase();
            if name == NAME_MORNING_BRIEF || name.contains("morning_brief") {
                Some("telegram".to_string())
            } else {
                None
            }
        })
}

fn local_tz_name() -> String {
    std::env::var("TZ").unwrap_or_else(|_| "UTC".to_string())
}

/// Build RRULE daily at hour:minute (local wall clock interpreted in schedule timezone).
pub fn daily_rrule(hour: u32, minute: u32) -> String {
    format!("FREQ=DAILY;BYHOUR={};BYMINUTE={};BYSECOND=0", hour.min(23), minute.min(59))
}

pub fn template_schedule(
    name: &str,
    tag: &str,
    message: &str,
    hour: u32,
    minute: u32,
    timezone: &str,
    notify_channel: Option<&str>,
    enabled: bool,
) -> Schedule {
    let now = Utc::now();
    Schedule {
        id: Uuid::new_v4(),
        name: name.to_string(),
        description: format!("Akasha Life layer pack ({tag})"),
        enabled,
        timezone: timezone.to_string(),
        rrule: daily_rrule(hour, minute),
        interval_seconds: None,
        start_at: now - Duration::minutes(1),
        end_at: None,
        channel_context: Some(build_channel_context(tag, message, notify_channel)),
        created_at: now,
        updated_at: now,
    }
}

pub fn morning_brief_template(
    hour: u32,
    minute: u32,
    timezone: &str,
    notify_channel: Option<&str>,
    enabled: bool,
) -> Schedule {
    template_schedule(
        NAME_MORNING_BRIEF,
        TAG_MORNING_BRIEF,
        MORNING_BRIEF_PROMPT,
        hour,
        minute,
        timezone,
        notify_channel.or(Some("telegram")),
        enabled,
    )
}

pub fn overnight_pack_template(
    hour: u32,
    minute: u32,
    timezone: &str,
    enabled: bool,
) -> Schedule {
    template_schedule(
        NAME_OVERNIGHT_PACK,
        TAG_OVERNIGHT_PACK,
        OVERNIGHT_PACK_PROMPT,
        hour,
        minute,
        timezone,
        None,
        enabled,
    )
}

/// Deterministic NL → schedule preview (Hermes-style). No LLM required for MVP.
pub fn parse_nl_schedule(text: &str) -> NlSchedulePreview {
    let lower = text.to_lowercase();
    let timezone = local_tz_name();
    let (mut hour, mut minute) = (8u32, 0u32);
    if let Some((h, m)) = extract_time(&lower) {
        hour = h;
        minute = m;
    }
    let notify_channel = if lower.contains("telegram") {
        Some("telegram".to_string())
    } else {
        None
    };
    let (name, tag, message, description) = if lower.contains("nuit")
        || lower.contains("overnight")
        || lower.contains("soir")
        || (lower.contains("pack") && lower.contains("nuit"))
    {
        if hour == 8 && minute == 0 && !lower.contains(':') && !has_hour_word(&lower) {
            hour = 2;
        }
        (
            NAME_OVERNIGHT_PACK.to_string(),
            Some(TAG_OVERNIGHT_PACK.to_string()),
            OVERNIGHT_PACK_PROMPT.to_string(),
            "Pack nuit (Life layer)".to_string(),
        )
    } else if lower.contains("brief")
        || lower.contains("matin")
        || lower.contains("morning")
        || lower.contains("digest")
    {
        (
            NAME_MORNING_BRIEF.to_string(),
            Some(TAG_MORNING_BRIEF.to_string()),
            MORNING_BRIEF_PROMPT.to_string(),
            "Brief matinal (Life layer)".to_string(),
        )
    } else {
        (
            "nl_schedule".to_string(),
            None,
            text.trim().to_string(),
            "Schedule créé depuis le langage naturel".to_string(),
        )
    };
    let confidence = if tag.is_some() { 0.85 } else { 0.55 };
    NlSchedulePreview {
        name,
        description,
        rrule: daily_rrule(hour, minute),
        timezone,
        hour_local: hour,
        minute_local: minute,
        tag,
        notify_channel,
        message,
        confidence,
    }
}

fn has_hour_word(s: &str) -> bool {
    s.contains("h") || s.contains("heure") || s.contains("am") || s.contains("pm")
}

fn extract_time(s: &str) -> Option<(u32, u32)> {
    // 8h30, 8:30, 08:30
    let re_colon = regex::Regex::new(r"\b(\d{1,2})[:hH](\d{2})\b").ok()?;
    if let Some(c) = re_colon.captures(s) {
        let h: u32 = c.get(1)?.as_str().parse().ok()?;
        let m: u32 = c.get(2)?.as_str().parse().ok()?;
        if h < 24 && m < 60 {
            return Some((h, m));
        }
    }
    let re_h = regex::Regex::new(r"\b(\d{1,2})\s*h\b").ok()?;
    if let Some(c) = re_h.captures(s) {
        let h: u32 = c.get(1)?.as_str().parse().ok()?;
        if h < 24 {
            return Some((h, 0));
        }
    }
    None
}

/// Send a Telegram message using vault token + connectors.env notify chat id.
pub async fn notify_telegram(data_dir: &Path, text: &str) -> Result<(), String> {
    let vault = akasha_vault::open_vault(data_dir).map_err(|e| e.to_string())?;
    let token = vault.get("telegram_bot_token").map_err(|e| e.to_string())?;
    if token.trim().is_empty() {
        return Err("telegram_bot_token not configured".to_string());
    }
    let chat_id = crate::connectors_config::connectors_config_view(data_dir)
        .telegram_notify_chat_id
        .and_then(|s| s.trim().parse::<i64>().ok())
        .ok_or_else(|| "AKASHA_TELEGRAM_NOTIFY_CHAT_ID not configured".to_string())?;
    crate::channels::telegram::send_telegram_message(&token, chat_id, text).await
}

pub async fn notify_channel(data_dir: &Path, channel: &str, text: &str) -> Result<(), String> {
    match channel.trim().to_lowercase().as_str() {
        "telegram" => notify_telegram(data_dir, text).await,
        other => Err(format!("unsupported notify channel: {other}")),
    }
}

/// After a scheduled task completes, push summary to preferred channel if configured.
pub async fn maybe_notify_completed_schedule(
    data_dir: &Path,
    store_path: &Path,
    schedule_id: Uuid,
    task_id: Uuid,
) {
    let Ok(schedule_store) = akasha_store::ScheduleStore::open(store_path) else {
        return;
    };
    let Ok(Some(schedule)) = schedule_store.get_schedule(schedule_id) else {
        return;
    };
    let Some(channel) = schedule_wants_notify(&schedule) else {
        return;
    };
    let summary = task_progress_summary(store_path, task_id).unwrap_or_else(|| {
        format!(
            "Akasha schedule « {} » terminé (task {}).",
            schedule.name,
            task_id
        )
    });
    let text: String = summary.chars().take(3500).collect();
    if let Err(e) = notify_channel(data_dir, &channel, &text).await {
        tracing::warn!(error = %e, schedule_id = %schedule_id, "life_layer notify failed");
    }
}

fn task_progress_summary(store_path: &Path, task_id: Uuid) -> Option<String> {
    let store = akasha_store::TaskStore::open(store_path).ok()?;
    let progress = store.get_progress(task_id).ok()?;
    let last = progress
        .iter()
        .rev()
        .find(|(_, m)| !m.trim().is_empty())
        .map(|(_, m)| m.clone())?;
    Some(last)
}

/// Find existing life pack schedule by canonical name.
pub fn find_pack_schedule(
    schedules: &[Schedule],
    name: &str,
) -> Option<Schedule> {
    schedules
        .iter()
        .find(|s| s.name.trim().eq_ignore_ascii_case(name))
        .cloned()
}

pub fn pack_status_from_schedule(id: &str, s: Option<&Schedule>, default_hour: u32) -> LifePackStatus {
    if let Some(sched) = s {
        let (hour, minute) = parse_hour_minute_from_rrule(&sched.rrule).unwrap_or((default_hour, 0));
        let notify = schedule_wants_notify(sched);
        LifePackStatus {
            id: id.to_string(),
            enabled: sched.enabled,
            schedule_id: Some(sched.id.to_string()),
            hour_local: hour,
            minute_local: minute,
            timezone: sched.timezone.clone(),
            notify_channel: notify,
        }
    } else {
        LifePackStatus {
            id: id.to_string(),
            enabled: false,
            schedule_id: None,
            hour_local: default_hour,
            minute_local: 0,
            timezone: local_tz_name(),
            notify_channel: if id == TAG_MORNING_BRIEF {
                Some("telegram".to_string())
            } else {
                None
            },
        }
    }
}

fn parse_hour_minute_from_rrule(rrule: &str) -> Option<(u32, u32)> {
    let mut hour = None;
    let mut minute = None;
    for part in rrule.split(';') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix("BYHOUR=") {
            hour = rest.parse().ok();
        }
        if let Some(rest) = part.strip_prefix("BYMINUTE=") {
            minute = rest.parse().ok();
        }
    }
    Some((hour?, minute.unwrap_or(0)))
}

/// Soft hint for skill draft filename uniqueness.
pub fn skill_draft_filename(task_id: Uuid) -> String {
    let day = Utc::now().date_naive();
    format!(
        "draft-from-task-{}-{}{}{}.md",
        &task_id.to_string()[..8],
        day.year(),
        day.month(),
        day.day()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nl_morning_brief_parses() {
        let p = parse_nl_schedule("chaque matin à 7h30, brief Telegram");
        assert_eq!(p.name, NAME_MORNING_BRIEF);
        assert_eq!(p.hour_local, 7);
        assert_eq!(p.minute_local, 30);
        assert_eq!(p.notify_channel.as_deref(), Some("telegram"));
        assert!(p.rrule.contains("BYHOUR=7"));
    }

    #[test]
    fn nl_overnight_defaults_hour() {
        let p = parse_nl_schedule("lance le pack nuit");
        assert_eq!(p.name, NAME_OVERNIGHT_PACK);
        assert_eq!(p.hour_local, 2);
    }

    #[test]
    fn channel_context_roundtrip() {
        let raw = build_channel_context(TAG_MORNING_BRIEF, "hello", Some("telegram"));
        let parsed = parse_life_context(Some(&raw)).unwrap();
        assert_eq!(parsed.tag, TAG_MORNING_BRIEF);
        assert_eq!(parsed.notify.unwrap().channel, "telegram");
    }
}
