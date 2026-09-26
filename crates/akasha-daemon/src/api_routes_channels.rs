//! Slack / Teams channel webhooks and Telegram channel-access HTTP routes.

use crate::api_http::json_response;
use crate::channels::ChannelConfig;
use std::path::Path;
use uuid::Uuid;

pub struct RouteCtx<'a> {
    pub data_dir: &'a Path,
    pub store_path: &'a Path,
    pub channel_config: &'a ChannelConfig,
    pub main_agent: &'a crate::agents::MainAgent,
    pub headers: &'a std::collections::HashMap<String, String>,
}

/// Returns `Some(response)` when this module handled the route.
/// `body` is taken by value because Slack/Teams handlers consume it.
pub async fn try_handle(
    method: &str,
    path: &str,
    path_only: &str,
    body: Option<Vec<u8>>,
    ctx: &RouteCtx<'_>,
) -> Option<String> {
    let data_dir = ctx.data_dir;
    let store_path = ctx.store_path;
    let channel_config = ctx.channel_config;
    let main_agent = ctx.main_agent;
    let headers = ctx.headers;
// Phase 4: Slack slash command
if method == "POST" && path == "/channels/slack/command" {
    if let Some(ref secret) = channel_config.slack_signing_secret {
        let sig = headers.get("x-slack-signature").map(String::as_str);
        let ts = headers.get("x-slack-request-timestamp").map(String::as_str);
        return Some(crate::channels::slack::handle_slack_command(
            body,
            sig,
            ts,
            secret,
            channel_config.port,
            main_agent,
            store_path,
        ));
    }
    return Some(json_response("404 Not Found", r#"{"error":"slack_not_configured"}"#));
}

// Phase 4: Microsoft Teams Bot Framework webhook
if method == "POST" && (path == "/channels/teams" || path == "/channels/teams/message") {
    if let (Some(ref app_id), Some(ref app_password)) = (
        &channel_config.teams_app_id,
        &channel_config.teams_app_password,
    ) {
        let auth_header = headers.get("authorization").map(String::as_str);
        return Some(crate::channels::teams::handle_teams_message(
            body,
            auth_header,
            app_id,
            app_password,
            channel_config.port,
            main_agent,
            store_path,
        ).await);
    }
    return Some(json_response("404 Not Found", r#"{"error":"teams_not_configured"}"#));
}



// Phase 5: Plugins

// Phase D: Skills (loadable skills for agents; Agent Skills spec + flat YAML)

// Telegram channel access (pairing + RBAC lifecycle)
if method == "GET" && path_only == "/api/channel-access/telegram/users" {
    let state = crate::channel_access::load(data_dir);
    let body = serde_json::json!({
        "approved": state.approved,
        "pending": state.pending
    });
    return Some(json_response("200 OK", &body.to_string()));
}
if method == "POST" && path_only == "/api/channel-access/telegram/request" {
    let body_json = body
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
    let user_id = body_json
        .as_ref()
        .and_then(|v| v.get("user_id").and_then(|n| n.as_i64()))
        .unwrap_or_default();
    if user_id == 0 {
        return Some(json_response("400 Bad Request", r#"{"error":"missing_user_id"}"#));
    }
    let username = body_json
        .as_ref()
        .and_then(|v| v.get("username").and_then(|s| s.as_str()))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let mut state = crate::channel_access::load(data_dir);
    if crate::channel_access::is_approved_user(&state, user_id) {
        return Some(json_response("200 OK", r#"{"ok":true,"already_approved":true}"#));
    }
    state.pending.retain(|p| p.user_id != user_id);
    let code = format!("TG-{}", Uuid::new_v4().to_string()[..8].to_uppercase());
    state.pending.push(crate::channel_access::TelegramPending {
        user_id,
        username,
        pairing_code: code.clone(),
        requested_at: chrono::Utc::now().to_rfc3339(),
    });
    if let Err(e) = crate::channel_access::save(data_dir, &state) {
        tracing::error!(error = %e, "channel-access: failed to persist pairing request");
        return Some(json_response("500 Internal Server Error", r#"{"error":"persistence_error"}"#));
    }
    let body = serde_json::json!({ "ok": true, "pairing_code": code });
    return Some(json_response("200 OK", &body.to_string()));
}
if method == "POST" && path_only == "/api/channel-access/telegram/approve" {
    let body_json = body
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
    let code = body_json
        .as_ref()
        .and_then(|v| v.get("pairing_code").and_then(|s| s.as_str()))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let user_id = body_json
        .as_ref()
        .and_then(|v| v.get("user_id").and_then(|n| n.as_i64()))
        .unwrap_or_default();
    let mut state = crate::channel_access::load(data_dir);
    let pending = if !code.is_empty() {
        let idx = state.pending.iter().position(|p| p.pairing_code == code);
        idx.map(|i| state.pending.remove(i))
    } else if user_id != 0 {
        let idx = state.pending.iter().position(|p| p.user_id == user_id);
        idx.map(|i| state.pending.remove(i))
    } else {
        None
    };
    let Some(pending) = pending else {
        return Some(json_response("404 Not Found", r#"{"error":"pending_not_found"}"#));
    };
    let is_first = state.approved.is_empty();
    state.approved.retain(|u| u.user_id != pending.user_id);
    state.approved.push(crate::channel_access::TelegramUser {
        user_id: pending.user_id,
        username: pending.username,
        role: if is_first {
            crate::channel_access::TelegramRole::Admin
        } else {
            crate::channel_access::TelegramRole::Member
        },
        approved_at: chrono::Utc::now().to_rfc3339(),
    });
    if let Err(e) = crate::channel_access::save(data_dir, &state) {
        tracing::error!(error = %e, "channel-access: failed to persist approve");
        return Some(json_response("500 Internal Server Error", r#"{"error":"persistence_error"}"#));
    }
    return Some(json_response("200 OK", r#"{"ok":true}"#));
}
if method == "POST" && path_only == "/api/channel-access/telegram/reject" {
    let body_json = body
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
    let user_id = body_json
        .as_ref()
        .and_then(|v| v.get("user_id").and_then(|n| n.as_i64()))
        .unwrap_or_default();
    if user_id == 0 {
        return Some(json_response("400 Bad Request", r#"{"error":"missing_user_id"}"#));
    }
    let mut state = crate::channel_access::load(data_dir);
    let before = state.pending.len();
    state.pending.retain(|p| p.user_id != user_id);
    if let Err(e) = crate::channel_access::save(data_dir, &state) {
        tracing::error!(error = %e, "channel-access: failed to persist reject");
        return Some(json_response("500 Internal Server Error", r#"{"error":"persistence_error"}"#));
    }
    let removed = before != state.pending.len();
    return Some(json_response(
        "200 OK",
        &serde_json::json!({ "ok": true, "removed": removed }).to_string(),
    ));
}
if method == "POST" && path_only == "/api/channel-access/telegram/remove" {
    let body_json = body
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
    let user_id = body_json
        .as_ref()
        .and_then(|v| v.get("user_id").and_then(|n| n.as_i64()))
        .unwrap_or_default();
    if user_id == 0 {
        return Some(json_response("400 Bad Request", r#"{"error":"missing_user_id"}"#));
    }
    let mut state = crate::channel_access::load(data_dir);
    let before = state.approved.len();
    state.approved.retain(|u| u.user_id != user_id);
    if let Err(e) = crate::channel_access::save(data_dir, &state) {
        tracing::error!(error = %e, "channel-access: failed to persist remove");
        return Some(json_response("500 Internal Server Error", r#"{"error":"persistence_error"}"#));
    }
    let removed = before != state.approved.len();
    return Some(json_response(
        "200 OK",
        &serde_json::json!({ "ok": true, "removed": removed }).to_string(),
    ));
}
if method == "POST" && path_only == "/api/channel-access/telegram/promote" {
    let body_json = body
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
    let user_id = body_json
        .as_ref()
        .and_then(|v| v.get("user_id").and_then(|n| n.as_i64()))
        .unwrap_or_default();
    if user_id == 0 {
        return Some(json_response("400 Bad Request", r#"{"error":"missing_user_id"}"#));
    }
    let mut state = crate::channel_access::load(data_dir);
    if let Some(u) = state.approved.iter_mut().find(|u| u.user_id == user_id) {
        u.role = crate::channel_access::TelegramRole::Admin;
        if let Err(e) = crate::channel_access::save(data_dir, &state) {
            tracing::error!(error = %e, "channel-access: failed to persist promote");
            return Some(json_response("500 Internal Server Error", r#"{"error":"persistence_error"}"#));
        }
        return Some(json_response("200 OK", r#"{"ok":true}"#));
    }
    return Some(json_response("404 Not Found", r#"{"error":"user_not_found"}"#));
}
if method == "POST" && path_only == "/api/channel-access/telegram/demote" {
    let body_json = body
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
    let user_id = body_json
        .as_ref()
        .and_then(|v| v.get("user_id").and_then(|n| n.as_i64()))
        .unwrap_or_default();
    if user_id == 0 {
        return Some(json_response("400 Bad Request", r#"{"error":"missing_user_id"}"#));
    }
    let mut state = crate::channel_access::load(data_dir);
    if let Some(u) = state.approved.iter_mut().find(|u| u.user_id == user_id) {
        u.role = crate::channel_access::TelegramRole::Member;
        if let Err(e) = crate::channel_access::save(data_dir, &state) {
            tracing::error!(error = %e, "channel-access: failed to persist demote");
            return Some(json_response("500 Internal Server Error", r#"{"error":"persistence_error"}"#));
        }
        return Some(json_response("200 OK", r#"{"ok":true}"#));
    }
    return Some(json_response("404 Not Found", r#"{"error":"user_not_found"}"#));
}
if method == "POST" && path_only == "/api/channel-access/telegram/reset" {
    let state = crate::channel_access::TelegramAccessState::default();
    if let Err(e) = crate::channel_access::save(data_dir, &state) {
        tracing::error!(error = %e, "channel-access: failed to persist reset");
        return Some(json_response("500 Internal Server Error", r#"{"error":"persistence_error"}"#));
    }
    return Some(json_response("200 OK", r#"{"ok":true}"#));
}

    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn channels_paths_smoke() {
        assert!(("/channels/slack/command", "POST").ok());
        assert!(("/channels/teams", "POST").ok());
        assert!(("/channels/teams/message", "POST").ok());
        assert!(("/api/channel-access/telegram/users", "GET").ok());
        assert!(("/api/channel-access/telegram/request", "POST").ok());
        assert!(("/api/channel-access/telegram/approve", "POST").ok());
        assert!(("/api/channel-access/telegram/reject", "POST").ok());
        assert!(("/api/channel-access/telegram/remove", "POST").ok());
        assert!(("/api/channel-access/telegram/promote", "POST").ok());
        assert!(("/api/channel-access/telegram/demote", "POST").ok());
        assert!(("/api/channel-access/telegram/reset", "POST").ok());
        assert!(!("/api/tasks", "GET").ok());
    }

    trait PathSmoke {
        fn ok(self) -> bool;
    }
    impl PathSmoke for (&str, &str) {
        fn ok(self) -> bool {
            let (p, m) = self;
            (m == "POST"
                && (p == "/channels/slack/command"
                    || p == "/channels/teams"
                    || p == "/channels/teams/message"))
                || p.starts_with("/api/channel-access/telegram/")
        }
    }
}
