//! Autonomous mission HTTP routes (`/api/autonomous-mission*`).

use crate::api_http::json_response;
use crate::api_security::parse_query_param;
use crate::autonomous_mission_config::{
    merge_from_json_partial, persist_config_and_snapshot, AutonomousMissionConfig, Horizon,
    MissionStatusYaml,
};
use akasha_store::AutonomousMissionStore;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

async fn get_autonomous_mission_state(
    data_dir: &Path,
    store_path: &Path,
    am: &Arc<RwLock<AutonomousMissionConfig>>,
) -> String {
    let cfg = am.read().await;
    let am_store = match AutonomousMissionStore::open(store_path) {
        Ok(s) => s,
        Err(e) => {
            return json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            );
        }
    };
    let (last_hb, last_tid) = am_store.get_meta().unwrap_or((None, None));
    let report_abs = data_dir.join(&cfg.report_dir);
    let horizon_s = match cfg.horizon {
        Horizon::Short => "short",
        Horizon::Medium => "medium",
        Horizon::Long => "long",
    };
    let status_s = match cfg.status {
        MissionStatusYaml::Active => "active",
        MissionStatusYaml::Paused => "paused",
        MissionStatusYaml::Completed => "completed",
    };
    let next_hb = last_hb.map(|t| t + chrono::Duration::minutes(cfg.heartbeat_interval_minutes as i64));
    let role_definitions: Vec<serde_json::Value> = cfg
        .role_definitions
        .iter()
        .map(|r| {
            serde_json::json!({
                "name": r.name,
                "responsibility": r.responsibility,
                "preferred_agent_type": r.preferred_agent_type,
            })
        })
        .collect();
    let body = serde_json::json!({
        "enabled": cfg.enabled,
        "global_context": cfg.global_context.as_str(),
        "horizon": horizon_s,
        "objective": cfg.objective.as_str(),
        "heartbeat_interval_minutes": cfg.heartbeat_interval_minutes,
        "report_dir": cfg.report_dir.as_str(),
        "report_path_absolute": report_abs.display().to_string(),
        "session_id": cfg.session_id.as_str(),
        "status": status_s,
        "operating_rules": cfg.operating_rules.as_str(),
        "role_definitions": role_definitions,
        "heartbeat_preferred_task_type": cfg.heartbeat_preferred_task_type.as_str(),
        "last_heartbeat_at": last_hb.map(|t| t.to_rfc3339()),
        "last_task_id": last_tid.map(|u| u.to_string()),
        "next_heartbeat_approx_at": next_hb.map(|t| t.to_rfc3339()),
    });
    json_response("200 OK", &body.to_string())
}

async fn put_autonomous_mission_state(
    data_dir: &Path,
    store_path: &Path,
    am: &Arc<RwLock<AutonomousMissionConfig>>,
    body: &[u8],
) -> String {
    let v: serde_json::Value = match serde_json::from_slice(body) {
        Ok(x) => x,
        Err(_) => return json_response("400 Bad Request", r#"{"error":"invalid_json"}"#),
    };
    {
        let mut w = am.write().await;
        let _ = merge_from_json_partial(&mut *w, &v);
        if let Err(e) = persist_config_and_snapshot(data_dir, store_path, &*w) {
            return json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            );
        }
    }
    get_autonomous_mission_state(data_dir, store_path, am).await
}

async fn get_autonomous_mission_events_list(store_path: &Path, query: &str) -> String {
    let limit = parse_query_param(query, "limit")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(100)
        .min(1000);
    let since = parse_query_param(query, "since").and_then(|s| {
        chrono::DateTime::parse_from_rfc3339(s.trim())
            .ok()
            .map(|d| d.with_timezone(&chrono::Utc))
    });
    let am_store = match AutonomousMissionStore::open(store_path) {
        Ok(s) => s,
        Err(e) => {
            return json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            );
        }
    };
    let events = match am_store.list_events_since(since, limit) {
        Ok(e) => e,
        Err(e) => {
            return json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            );
        }
    };
    let arr: Vec<_> = events
        .iter()
        .map(|e| {
            serde_json::json!({
                "id": e.id,
                "at": e.at.to_rfc3339(),
                "event_type": e.event_type,
                "payload": e.payload,
            })
        })
        .collect();
    json_response("200 OK", &serde_json::json!({ "events": arr }).to_string())
}

async fn post_autonomous_mission_status(
    data_dir: &Path,
    store_path: &Path,
    am: &Arc<RwLock<AutonomousMissionConfig>>,
    status: MissionStatusYaml,
) -> String {
    {
        let mut w = am.write().await;
        w.status = status;
        if let Err(e) = persist_config_and_snapshot(data_dir, store_path, &*w) {
            return json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            );
        }
    }
    get_autonomous_mission_state(data_dir, store_path, am).await
}

/// Returns `Some(response)` when this module handled the route.
pub async fn handle_mission_routes(
    method: &str,
    path_only: &str,
    query_str: &str,
    body: Option<&[u8]>,
    data_dir: &Path,
    store_path: &Path,
    autonomous_mission: Option<Arc<RwLock<AutonomousMissionConfig>>>,
) -> Option<String> {
    if path_only == "/api/autonomous-mission" {
        let Some(ref am) = autonomous_mission else {
            return Some(json_response(
                "503 Service Unavailable",
                r#"{"error":"autonomous_mission_unavailable"}"#,
            ));
        };
        if method == "GET" {
            return Some(get_autonomous_mission_state(data_dir, store_path, am).await);
        }
        if method == "PUT" {
            let Some(b) = body else {
                return Some(json_response("400 Bad Request", r#"{"error":"body_required"}"#));
            };
            return Some(put_autonomous_mission_state(data_dir, store_path, am, b).await);
        }
        return Some(json_response("405 Method Not Allowed", r#"{"error":"method_not_allowed"}"#));
    }
    if path_only == "/api/autonomous-mission/events" && method == "GET" {
        if autonomous_mission.is_none() {
            return Some(json_response(
                "503 Service Unavailable",
                r#"{"error":"autonomous_mission_unavailable"}"#,
            ));
        }
        return Some(get_autonomous_mission_events_list(store_path, query_str).await);
    }
    if path_only == "/api/autonomous-mission/pause" && method == "POST" {
        let Some(ref am) = autonomous_mission else {
            return Some(json_response(
                "503 Service Unavailable",
                r#"{"error":"autonomous_mission_unavailable"}"#,
            ));
        };
        return Some(
            post_autonomous_mission_status(data_dir, store_path, am, MissionStatusYaml::Paused).await,
        );
    }
    if path_only == "/api/autonomous-mission/resume" && method == "POST" {
        let Some(ref am) = autonomous_mission else {
            return Some(json_response(
                "503 Service Unavailable",
                r#"{"error":"autonomous_mission_unavailable"}"#,
            ));
        };
        return Some(
            post_autonomous_mission_status(data_dir, store_path, am, MissionStatusYaml::Active).await,
        );
    }
    None
}
