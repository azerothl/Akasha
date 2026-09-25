//! Companion device routes: snapshot, pairing, presence (Phase 4–5 / CMP-011…015).

use crate::api_http::json_response;
use akasha_store::{TaskStatus, TaskStore};
use chrono::Timelike;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CompanionDevicesFile {
    devices: Vec<CompanionDevice>,
    #[serde(default)]
    last_presence_event: Option<PresenceEventRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CompanionDevice {
    device_id: String,
    token: String,
    name: String,
    paired_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PresenceEventRecord {
    event: String,
    device_id: String,
    ts: String,
    #[serde(default)]
    detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PresencePolicy {
    enabled: bool,
    threshold_rms: f32,
    min_speech_ms: u32,
    max_speech_ms: u32,
    silence_hang_ms: u32,
    cooldown_ms: u32,
    /// Optional quiet hours "HH:MM-HH:MM" local (daemon local time); empty = always.
    #[serde(default)]
    quiet_hours: String,
}

impl Default for PresencePolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold_rms: 0.035,
            min_speech_ms: 400,
            max_speech_ms: 10000,
            silence_hang_ms: 700,
            cooldown_ms: 2500,
            quiet_hours: String::new(),
        }
    }
}

fn devices_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("companion_devices.json")
}

fn presence_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("companion_presence.json")
}

fn load_devices(data_dir: &Path) -> CompanionDevicesFile {
    let path = devices_path(data_dir);
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => CompanionDevicesFile::default(),
    }
}

fn save_devices(data_dir: &Path, file: &CompanionDevicesFile) -> Result<(), String> {
    let path = devices_path(data_dir);
    let json = serde_json::to_string_pretty(file).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())
}

fn load_presence_policy(data_dir: &Path) -> PresencePolicy {
    let path = presence_path(data_dir);
    match std::fs::read_to_string(&path) {
        Ok(s) => {
            let mut p: PresencePolicy = serde_json::from_str(&s).unwrap_or_default();
            // Env overrides
            if let Ok(v) = std::env::var("AKASHA_COMPANION_VAD_ENABLED") {
                if v == "1" || v.eq_ignore_ascii_case("true") {
                    p.enabled = true;
                } else if v == "0" || v.eq_ignore_ascii_case("false") {
                    p.enabled = false;
                }
            }
            p
        }
        Err(_) => {
            let mut p = PresencePolicy::default();
            if let Ok(v) = std::env::var("AKASHA_COMPANION_VAD_ENABLED") {
                if v == "1" || v.eq_ignore_ascii_case("true") {
                    p.enabled = true;
                }
            }
            p
        }
    }
}

fn save_presence_policy(data_dir: &Path, policy: &PresencePolicy) -> Result<(), String> {
    let path = presence_path(data_dir);
    let json = serde_json::to_string_pretty(policy).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())
}

/// Quiet hours "HH:MM-HH:MM" — if current local time is inside, VAD is considered off.
fn in_quiet_hours(quiet: &str) -> bool {
    let quiet = quiet.trim();
    if quiet.is_empty() {
        return false;
    }
    let Some((a, b)) = quiet.split_once('-') else {
        return false;
    };
    let parse = |s: &str| -> Option<(u32, u32)> {
        let (h, m) = s.trim().split_once(':')?;
        Some((h.parse().ok()?, m.parse().ok()?))
    };
    let Some((h1, m1)) = parse(a) else {
        return false;
    };
    let Some((h2, m2)) = parse(b) else {
        return false;
    };
    let now = chrono::Local::now();
    let cur = now.hour() * 60 + now.minute();
    let start = h1 * 60 + m1;
    let end = h2 * 60 + m2;
    if start <= end {
        cur >= start && cur < end
    } else {
        // wraps midnight
        cur >= start || cur < end
    }
}

fn bearer_token(headers: &std::collections::HashMap<String, String>) -> Option<String> {
    let auth = headers.get("authorization")?;
    let auth = auth.trim();
    let rest = auth
        .strip_prefix("Bearer ")
        .or_else(|| auth.strip_prefix("bearer "))?;
    let t = rest.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// Soft auth: empty bearer always OK (LAN). If Authorization present, accept known device token
/// or (when set) `AKASHA_API_TOKEN` / `AKASHA_TOKEN`.
fn companion_auth_ok(data_dir: &Path, headers: &std::collections::HashMap<String, String>) -> bool {
    let Some(token) = bearer_token(headers) else {
        return true;
    };
    if let Ok(env_tok) = std::env::var("AKASHA_API_TOKEN") {
        if !env_tok.is_empty() && env_tok == token {
            return true;
        }
    }
    if let Ok(env_tok) = std::env::var("AKASHA_TOKEN") {
        if !env_tok.is_empty() && env_tok == token {
            return true;
        }
    }
    let file = load_devices(data_dir);
    file.devices.iter().any(|d| d.token == token)
}

fn count_active_tasks(store_path: &Path) -> u32 {
    match TaskStore::open(store_path) {
        Ok(store) => store
            .get_all()
            .unwrap_or_default()
            .iter()
            .filter(|t| {
                matches!(
                    t.status,
                    TaskStatus::Pending
                        | TaskStatus::Queued
                        | TaskStatus::Running
                        | TaskStatus::WaitingUserInput
                )
            })
            .count() as u32,
        Err(_) => 0,
    }
}

fn build_snapshot(
    data_dir: &Path,
    store_path: &Path,
    llm_router: &Arc<akasha_llm::LLMRouter>,
) -> serde_json::Value {
    let voice = crate::voice::load_voice_config(data_dir);
    let stt = voice.as_ref().map_or(false, |c| c.stt_configured());
    let tts = voice.as_ref().map_or(false, |c| c.tts_configured());
    let (provider, model) = llm_router
        .primary_route_for_task_type("conversation")
        .unwrap_or_else(|| (String::new(), String::new()));
    let active = count_active_tasks(store_path);
    let unread: u32 = 0;
    let headline: Option<String> = None;
    let avatar_hint = if unread > 0 { "notify" } else { "idle" };

    let policy = load_presence_policy(data_dir);
    let quiet = in_quiet_hours(&policy.quiet_hours);
    let vad_enabled = policy.enabled && !quiet && stt;
    let devices = load_devices(data_dir);
    let last_event_ts = devices
        .last_presence_event
        .as_ref()
        .map(|e| e.ts.clone());
    let presence_mode = if vad_enabled {
        "hands_free"
    } else {
        "ptt"
    };

    serde_json::json!({
        "schema_version": 1,
        "daemon": {
            "ok": true,
            "version": env!("CARGO_PKG_VERSION"),
        },
        "llm": {
            "provider": if provider.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(provider) },
            "model": if model.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(model) },
        },
        "voice": { "stt": stt, "tts": tts },
        "tasks": { "active": active },
        "notify": { "unread": unread, "headline": headline },
        "avatar_hint": avatar_hint,
        "presence_mode": presence_mode,
        "vad_enabled": vad_enabled,
        "last_event_ts": last_event_ts,
        "presence": {
            "enabled": policy.enabled,
            "threshold_rms": policy.threshold_rms,
            "min_speech_ms": policy.min_speech_ms,
            "max_speech_ms": policy.max_speech_ms,
            "silence_hang_ms": policy.silence_hang_ms,
            "cooldown_ms": policy.cooldown_ms,
            "quiet_hours": policy.quiet_hours,
            "quiet_now": quiet,
        }
    })
}

pub struct RouteCtx<'a> {
    pub data_dir: &'a Path,
    pub store_path: &'a Path,
    pub llm_router: &'a Arc<akasha_llm::LLMRouter>,
    pub headers: &'a std::collections::HashMap<String, String>,
}

pub async fn try_handle(
    method: &str,
    path_only: &str,
    _query_str: Option<&str>,
    body: Option<&[u8]>,
    ctx: &RouteCtx<'_>,
) -> Option<String> {
    if !path_only.starts_with("/api/companion") {
        return None;
    }

    if !companion_auth_ok(ctx.data_dir, ctx.headers) {
        return Some(json_response(
            "401 Unauthorized",
            r#"{"error":"invalid_token"}"#,
        ));
    }

    if method == "GET" && path_only == "/api/companion/snapshot" {
        let snap = build_snapshot(ctx.data_dir, ctx.store_path, ctx.llm_router);
        return Some(json_response("200 OK", &snap.to_string()));
    }

    if method == "GET" && path_only == "/api/companion/presence/config" {
        let policy = load_presence_policy(ctx.data_dir);
        let quiet = in_quiet_hours(&policy.quiet_hours);
        let body = serde_json::json!({
            "enabled": policy.enabled,
            "threshold_rms": policy.threshold_rms,
            "min_speech_ms": policy.min_speech_ms,
            "max_speech_ms": policy.max_speech_ms,
            "silence_hang_ms": policy.silence_hang_ms,
            "cooldown_ms": policy.cooldown_ms,
            "quiet_hours": policy.quiet_hours,
            "quiet_now": quiet,
            "effective_enabled": policy.enabled && !quiet,
        });
        return Some(json_response("200 OK", &body.to_string()));
    }

    if method == "POST" && path_only == "/api/companion/presence/config" {
        let body_json = body.and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
        let mut policy = load_presence_policy(ctx.data_dir);
        if let Some(j) = body_json.as_ref() {
            if let Some(v) = j.get("enabled").and_then(|x| x.as_bool()) {
                policy.enabled = v;
            }
            if let Some(v) = j.get("threshold_rms").and_then(|x| x.as_f64()) {
                policy.threshold_rms = v.clamp(0.001, 0.5) as f32;
            }
            if let Some(v) = j.get("min_speech_ms").and_then(|x| x.as_u64()) {
                policy.min_speech_ms = (v as u32).clamp(100, 5000);
            }
            if let Some(v) = j.get("max_speech_ms").and_then(|x| x.as_u64()) {
                policy.max_speech_ms = (v as u32).clamp(1000, 15000);
            }
            if let Some(v) = j.get("silence_hang_ms").and_then(|x| x.as_u64()) {
                policy.silence_hang_ms = (v as u32).clamp(100, 3000);
            }
            if let Some(v) = j.get("cooldown_ms").and_then(|x| x.as_u64()) {
                policy.cooldown_ms = (v as u32).clamp(500, 30000);
            }
            if let Some(v) = j.get("quiet_hours").and_then(|x| x.as_str()) {
                policy.quiet_hours = v.trim().to_string();
            }
        }
        if let Err(e) = save_presence_policy(ctx.data_dir, &policy) {
            return Some(json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e }).to_string(),
            ));
        }
        return Some(json_response(
            "200 OK",
            &serde_json::to_string(&policy).unwrap_or_else(|_| "{}".into()),
        ));
    }

    if method == "POST" && path_only == "/api/companion/presence/event" {
        let body_json = body.and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
        let event = body_json
            .as_ref()
            .and_then(|j| j.get("event"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let device_id = body_json
            .as_ref()
            .and_then(|j| j.get("device_id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let detail = body_json
            .as_ref()
            .and_then(|j| j.get("detail"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        if event.is_empty() {
            return Some(json_response(
                "400 Bad Request",
                r#"{"error":"event_required"}"#,
            ));
        }
        let allowed = [
            "vad_start",
            "vad_end",
            "speech_dropped",
            "speech_sent",
            "hands_free_on",
            "hands_free_off",
            "error",
        ];
        if !allowed.contains(&event.as_str()) {
            return Some(json_response(
                "400 Bad Request",
                r#"{"error":"event_unknown"}"#,
            ));
        }

        let rec = PresenceEventRecord {
            event: event.clone(),
            device_id: device_id.clone(),
            ts: chrono::Utc::now().to_rfc3339(),
            detail,
        };
        let mut file = load_devices(ctx.data_dir);
        file.last_presence_event = Some(rec.clone());
        let _ = save_devices(ctx.data_dir, &file);
        tracing::info!(
            event = %rec.event,
            device_id = %rec.device_id,
            "companion presence event"
        );
        return Some(json_response(
            "200 OK",
            &serde_json::json!({ "ok": true, "ts": rec.ts }).to_string(),
        ));
    }

    if method == "POST" && path_only == "/api/companion/pair" {
        let body_json = body.and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
        let device_id = body_json
            .as_ref()
            .and_then(|j| j.get("device_id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let name = body_json
            .as_ref()
            .and_then(|j| j.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or("companion")
            .trim()
            .to_string();
        let secret = body_json
            .as_ref()
            .and_then(|j| j.get("secret"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();

        if device_id.is_empty() {
            return Some(json_response(
                "400 Bad Request",
                r#"{"error":"device_id_required"}"#,
            ));
        }

        let expected = std::env::var("AKASHA_COMPANION_PAIR_SECRET").unwrap_or_default();
        if !expected.is_empty() && secret != expected {
            return Some(json_response(
                "403 Forbidden",
                r#"{"error":"pair_secret_invalid"}"#,
            ));
        }
        if expected.is_empty() {
            tracing::warn!(
                "AKASHA_COMPANION_PAIR_SECRET unset — pairing open on LAN for device_id={}",
                device_id
            );
        }

        let token = format!("cmp_{}", uuid::Uuid::new_v4().simple());
        let paired_at = chrono::Utc::now().to_rfc3339();
        let mut file = load_devices(ctx.data_dir);
        if let Some(existing) = file.devices.iter_mut().find(|d| d.device_id == device_id) {
            existing.token = token.clone();
            existing.name = name.clone();
            existing.paired_at = paired_at.clone();
        } else {
            file.devices.push(CompanionDevice {
                device_id: device_id.clone(),
                token: token.clone(),
                name: name.clone(),
                paired_at,
            });
        }
        if let Err(e) = save_devices(ctx.data_dir, &file) {
            return Some(json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e }).to_string(),
            ));
        }

        let resp = serde_json::json!({
            "token": token,
            "device_id": device_id,
        });
        return Some(json_response("200 OK", &resp.to_string()));
    }

    Some(json_response(
        "404 Not Found",
        r#"{"error":"companion_route_not_found"}"#,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn snapshot_shape_has_required_keys() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let store_path = dir.path().join("tasks.db");
        let _ = TaskStore::open(&store_path);
        let router = Arc::new(akasha_llm::LLMRouter::new(
            akasha_llm::RoutingConfig::default_config(),
        ));
        let snap = build_snapshot(dir.path(), &store_path, &router);
        assert_eq!(snap["schema_version"], 1);
        assert_eq!(snap["daemon"]["ok"], true);
        assert!(snap["daemon"]["version"].as_str().is_some());
        assert!(snap.get("voice").is_some());
        assert!(snap.get("tasks").is_some());
        assert!(snap.get("notify").is_some());
        assert!(snap.get("avatar_hint").is_some());
        assert!(snap.get("llm").is_some());
        assert!(snap.get("presence_mode").is_some());
        assert!(snap.get("vad_enabled").is_some());
        assert!(snap.get("presence").is_some());
    }

    #[test]
    fn presence_config_roundtrip() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let mut p = PresencePolicy::default();
        p.enabled = true;
        p.threshold_rms = 0.05;
        save_presence_policy(dir.path(), &p).unwrap();
        let loaded = load_presence_policy(dir.path());
        assert!(loaded.enabled);
        assert!((loaded.threshold_rms - 0.05).abs() < 0.0001);
    }

    #[test]
    fn presence_event_ingested() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let store_path = dir.path().join("tasks.db");
        let router = Arc::new(akasha_llm::LLMRouter::new(
            akasha_llm::RoutingConfig::default_config(),
        ));
        let headers = HashMap::new();
        let ctx = RouteCtx {
            data_dir: dir.path(),
            store_path: &store_path,
            llm_router: &router,
            headers: &headers,
        };
        let body = br#"{"event":"vad_start","device_id":"abc","detail":"rms=0.1"}"#;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let resp = rt.block_on(try_handle(
            "POST",
            "/api/companion/presence/event",
            None,
            Some(body.as_slice()),
            &ctx,
        ));
        assert!(resp.unwrap().contains("200 OK"));
        let file = load_devices(dir.path());
        assert_eq!(
            file.last_presence_event.as_ref().map(|e| e.event.as_str()),
            Some("vad_start")
        );
    }

    #[test]
    fn pair_secret_gate() {
        std::env::set_var("AKASHA_COMPANION_PAIR_SECRET", "test-secret");
        let dir = tempfile::tempdir().expect("tmpdir");
        let store_path = dir.path().join("tasks.db");
        let router = Arc::new(akasha_llm::LLMRouter::new(
            akasha_llm::RoutingConfig::default_config(),
        ));
        let headers = HashMap::new();
        let ctx = RouteCtx {
            data_dir: dir.path(),
            store_path: &store_path,
            llm_router: &router,
            headers: &headers,
        };
        let body = br#"{"device_id":"aa:bb","name":"test","secret":"wrong"}"#;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let resp = rt.block_on(try_handle(
            "POST",
            "/api/companion/pair",
            None,
            Some(body.as_slice()),
            &ctx,
        ));
        assert!(resp.unwrap().contains("403"));
        let body_ok = br#"{"device_id":"aa:bb","name":"test","secret":"test-secret"}"#;
        let resp_ok = rt.block_on(try_handle(
            "POST",
            "/api/companion/pair",
            None,
            Some(body_ok.as_slice()),
            &ctx,
        ));
        let r = resp_ok.unwrap();
        assert!(r.contains("200 OK"));
        assert!(r.contains("cmp_"));
        std::env::remove_var("AKASHA_COMPANION_PAIR_SECRET");
    }

    #[test]
    fn bearer_empty_allowed() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let headers = HashMap::new();
        assert!(companion_auth_ok(dir.path(), &headers));
    }

    #[test]
    fn quiet_hours_parse() {
        assert!(!in_quiet_hours(""));
        // Can't reliably assert current time; just ensure no panic
        let _ = in_quiet_hours("00:00-23:59");
        let _ = in_quiet_hours("22:00-06:00");
    }
}
