//! Permissions mode, queue, and decision center HTTP routes.

use crate::api::{
    decode_url_component, load_permission_mode, save_permission_mode, HumanInputStore,
    PermissionModeState,
};
use crate::api_http::json_response;
use std::path::Path;
use uuid::Uuid;

fn permission_queue_decision_body(body: Option<&[u8]>) -> (Option<String>, Option<String>) {
    let Some(b) = body else {
        return (None, None);
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(b) else {
        return (None, None);
    };
    let note = v
        .get("note")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string());
    let decision_source = v
        .get("decision_source")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string());
    (note, decision_source)
}

pub struct RouteCtx<'a> {
    pub data_dir: &'a Path,
    /// Full request path including query string (used by queue filters).
    pub path: &'a str,
    pub human_input_store: Option<&'a HumanInputStore>,
}

/// Returns `Some(response)` when this module handled the route.
pub async fn try_handle(
    method: &str,
    path_only: &str,
    body: Option<&[u8]>,
    ctx: &RouteCtx<'_>,
) -> Option<String> {
    let data_dir = ctx.data_dir;
    let path = ctx.path;
    let human_input_store = ctx.human_input_store;
if method == "GET" && path_only == "/api/permissions/mode" {
    let state = load_permission_mode(data_dir);
    return Some(json_response(
        "200 OK",
        &serde_json::json!({ "mode": state.mode, "updated_at": state.updated_at }).to_string(),
    ));
}
if method == "POST" && path_only == "/api/permissions/mode" {
    let body_json = body
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
    let mode = body_json
        .as_ref()
        .and_then(|v| v.get("mode").and_then(|s| s.as_str()))
        .unwrap_or("")
        .to_lowercase();
    if mode != "ask_me" && mode != "allow_all" {
        return Some(json_response("400 Bad Request", r#"{"error":"invalid_mode"}"#));
    }
    let state = PermissionModeState {
        mode,
        updated_at: chrono::Utc::now().to_rfc3339(),
    };
    match save_permission_mode(data_dir, &state) {
        Ok(()) => {
            return Some(json_response(
                "200 OK",
                &serde_json::json!({ "ok": true, "mode": state.mode }).to_string(),
            ));
        }
        Err(e) => {
            return Some(json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error":"save_failed", "detail": e.to_string() }).to_string(),
            ));
        }
    }
}

// Permission center: persisted approval decisions.
if method == "GET" && path_only == "/api/permissions/decisions" {
    let state = crate::permissions_center::load(data_dir);
    let body = serde_json::json!({ "decisions": state.decisions });
    return Some(json_response("200 OK", &body.to_string()));
}
if method == "GET" && path_only == "/api/permissions/queue" {
    let status_filter = path
        .split('?')
        .nth(1)
        .and_then(|q| q.split('&').find(|p| p.starts_with("status=")))
        .and_then(|p| p.split_once('=').map(|(_, v)| decode_url_component(v)))
        .and_then(|v| crate::permissions_queue::QueueStatus::parse(&v));
    let limit = path
        .split('?')
        .nth(1)
        .and_then(|q| q.split('&').find(|p| p.starts_with("limit=")))
        .and_then(|p| p.split_once('=').map(|(_, v)| v.to_string()))
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(100)
        .clamp(1, 500);
    let mut items = crate::permissions_queue::load(data_dir).requests;
    if let Some(status) = status_filter {
        items.retain(|i| i.status == status);
    }
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    items.truncate(limit);
    let body = serde_json::json!({ "items": items });
    return Some(json_response("200 OK", &body.to_string()));
}
if method == "GET" && path_only.starts_with("/api/permissions/queue/") {
    let id = path_only.trim_start_matches("/api/permissions/queue/").trim();
    if id.is_empty() {
        return Some(json_response("400 Bad Request", r#"{"error":"missing_id"}"#));
    }
    if let Some(item) = crate::permissions_queue::get_request(data_dir, id) {
        return Some(json_response(
            "200 OK",
            &serde_json::json!({ "item": item }).to_string(),
        ));
    }
    return Some(json_response("404 Not Found", r#"{"error":"request_not_found"}"#));
}
if method == "POST"
    && (path_only.starts_with("/api/permissions/queue/") && path_only.ends_with("/approve"))
{
    let id = path_only
        .trim_start_matches("/api/permissions/queue/")
        .trim_end_matches("/approve")
        .trim_matches('/');
    if id.is_empty() {
        return Some(json_response("400 Bad Request", r#"{"error":"missing_id"}"#));
    }
    let (note, decision_source) = permission_queue_decision_body(body.as_deref());
    match crate::permissions_queue::update_status(
        data_dir,
        id,
        crate::permissions_queue::QueueStatus::Approved,
        note,
        decision_source.or(Some("api".to_string())),
    ) {
        Ok(Some(item)) => {
            if let (Some(store), Ok(task_id)) =
                (human_input_store, Uuid::parse_str(&item.task_id))
            {
                let pending = {
                    let mut g = store.write().await;
                    g.remove(&task_id)
                };
                if let Some(pending) = pending {
                    let _ = pending.response_tx.send("Approuver".to_string());
                }
            }
            return Some(json_response(
                "200 OK",
                &serde_json::json!({ "ok": true, "item": item }).to_string(),
            ));
        }
        Ok(None) => return Some(json_response("404 Not Found", r#"{"error":"request_not_found"}"#)),
        Err(e) => {
            return Some(json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error":"save_failed", "detail": e.to_string() }).to_string(),
            ));
        }
    }
}
if method == "POST"
    && (path_only.starts_with("/api/permissions/queue/") && path_only.ends_with("/deny"))
{
    let id = path_only
        .trim_start_matches("/api/permissions/queue/")
        .trim_end_matches("/deny")
        .trim_matches('/');
    if id.is_empty() {
        return Some(json_response("400 Bad Request", r#"{"error":"missing_id"}"#));
    }
    let (note, decision_source) = permission_queue_decision_body(body.as_deref());
    match crate::permissions_queue::update_status(
        data_dir,
        id,
        crate::permissions_queue::QueueStatus::Denied,
        note,
        decision_source.or(Some("api".to_string())),
    ) {
        Ok(Some(item)) => {
            if let (Some(store), Ok(task_id)) =
                (human_input_store, Uuid::parse_str(&item.task_id))
            {
                let pending = {
                    let mut g = store.write().await;
                    g.remove(&task_id)
                };
                if let Some(pending) = pending {
                    let _ = pending.response_tx.send("Refuser".to_string());
                }
            }
            return Some(json_response(
                "200 OK",
                &serde_json::json!({ "ok": true, "item": item }).to_string(),
            ));
        }
        Ok(None) => return Some(json_response("404 Not Found", r#"{"error":"request_not_found"}"#)),
        Err(e) => {
            return Some(json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error":"save_failed", "detail": e.to_string() }).to_string(),
            ));
        }
    }
}
if method == "POST"
    && (path_only.starts_with("/api/permissions/queue/") && path_only.ends_with("/expire"))
{
    let id = path_only
        .trim_start_matches("/api/permissions/queue/")
        .trim_end_matches("/expire")
        .trim_matches('/');
    if id.is_empty() {
        return Some(json_response("400 Bad Request", r#"{"error":"missing_id"}"#));
    }
    match crate::permissions_queue::update_status(
        data_dir,
        id,
        crate::permissions_queue::QueueStatus::Expired,
        Some("expired by operator".to_string()),
        Some("api".to_string()),
    ) {
        Ok(Some(item)) => {
            return Some(json_response(
                "200 OK",
                &serde_json::json!({ "ok": true, "item": item }).to_string(),
            ));
        }
        Ok(None) => return Some(json_response("404 Not Found", r#"{"error":"request_not_found"}"#)),
        Err(e) => {
            return Some(json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error":"save_failed", "detail": e.to_string() }).to_string(),
            ));
        }
    }
}
if method == "POST" && path_only == "/api/permissions/decisions" {
    let body_json = body
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
    let tool = body_json
        .as_ref()
        .and_then(|v| v.get("tool").and_then(|s| s.as_str()))
        .unwrap_or("")
        .trim()
        .to_string();
    let scope = body_json
        .as_ref()
        .and_then(|v| v.get("scope").and_then(|s| s.as_str()))
        .unwrap_or("global")
        .trim()
        .to_string();
    let mode_str = body_json
        .as_ref()
        .and_then(|v| v.get("mode").and_then(|s| s.as_str()))
        .unwrap_or("")
        .trim()
        .to_lowercase();
    let mode = match mode_str.as_str() {
        "allow_persistent" => crate::permissions_center::DecisionMode::AllowPersistent,
        "deny_persistent" => crate::permissions_center::DecisionMode::DenyPersistent,
        _ => {
            return Some(json_response(
                "400 Bad Request",
                r#"{"error":"invalid_mode","expected":"allow_persistent|deny_persistent"}"#,
            ));
        }
    };
    if tool.is_empty() {
        return Some(json_response("400 Bad Request", r#"{"error":"missing_tool"}"#));
    }
    let mut state = crate::permissions_center::load(data_dir);
    state
        .decisions
        .retain(|d| !(d.tool == tool && d.scope == scope));
    let decision = crate::permissions_center::PermissionDecision {
        id: Uuid::new_v4().to_string(),
        tool,
        scope,
        mode,
        created_at: chrono::Utc::now().to_rfc3339(),
        expires_at: None,
    };
    state.decisions.push(decision.clone());
    match crate::permissions_center::save(data_dir, &state) {
        Ok(()) => {
            return Some(json_response(
                "200 OK",
                &serde_json::json!({ "ok": true, "decision": decision }).to_string(),
            ));
        }
        Err(e) => {
            return Some(json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error":"save_failed", "detail": e.to_string() }).to_string(),
            ));
        }
    }
}
if method == "DELETE" && path_only.starts_with("/api/permissions/decisions/") {
    let id = path_only
        .trim_start_matches("/api/permissions/decisions/")
        .trim();
    if id.is_empty() {
        return Some(json_response("400 Bad Request", r#"{"error":"missing_id"}"#));
    }
    let mut state = crate::permissions_center::load(data_dir);
    let before = state.decisions.len();
    state.decisions.retain(|d| d.id != id);
    let removed = before != state.decisions.len();
    if removed {
        if let Err(e) = crate::permissions_center::save(data_dir, &state) {
            tracing::error!(error = %e, id = %id, "permissions/decisions: failed to persist delete");
            return Some(json_response("500 Internal Server Error", r#"{"error":"persistence_error"}"#));
        }
    }
    return Some(json_response(
        "200 OK",
        &serde_json::json!({ "ok": true, "removed": removed, "id": id }).to_string(),
    ));
}

    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn permissions_paths_smoke() {
        assert!(("/api/permissions/mode", "GET").ok());
        assert!(("/api/permissions/mode", "POST").ok());
        assert!(("/api/permissions/decisions", "GET").ok());
        assert!(("/api/permissions/decisions", "POST").ok());
        assert!(("/api/permissions/queue", "GET").ok());
        assert!(("/api/permissions/queue/abc", "GET").ok());
        assert!(("/api/permissions/queue/abc/approve", "POST").ok());
        assert!(("/api/permissions/queue/abc/deny", "POST").ok());
        assert!(("/api/permissions/queue/abc/expire", "POST").ok());
        assert!(("/api/permissions/decisions/abc", "DELETE").ok());
        assert!(!("/api/tasks", "GET").ok());
    }

    trait PathSmoke {
        fn ok(self) -> bool;
    }
    impl PathSmoke for (&str, &str) {
        fn ok(self) -> bool {
            let (p, _m) = self;
            p == "/api/permissions/mode"
                || p == "/api/permissions/decisions"
                || p == "/api/permissions/queue"
                || p.starts_with("/api/permissions/queue/")
                || p.starts_with("/api/permissions/decisions/")
        }
    }
}
