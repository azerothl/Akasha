//! Session, connectors, discovery, and akasha.env config routes.

use crate::agents::TaskPriority;
use crate::api_http::json_response;
use crate::memory::ShortTermStore;
use std::path::Path;
use std::sync::Arc;
use uuid::Uuid;

pub struct RouteCtx<'a> {
    pub data_dir: &'a Path,
    pub store_path: &'a Path,
    pub main_agent: &'a crate::agents::MainAgent,
    pub short_term: Option<Arc<ShortTermStore>>,
    pub tools_executor: Option<
        &'a Arc<tokio::sync::RwLock<Arc<akasha_tools::ToolExecutor>>>,
    >,
}

fn full_path(path_only: &str, query_str: Option<&str>) -> String {
    match query_str.filter(|q| !q.is_empty()) {
        Some(q) => format!("{path_only}?{q}"),
        None => path_only.to_string(),
    }
}


pub async fn try_handle(
    method: &str,
    path_only: &str,
    query_str: Option<&str>,
    body: Option<&[u8]>,
    ctx: &RouteCtx<'_>,
) -> Option<String> {
    let path = full_path(path_only, query_str);
    let data_dir = ctx.data_dir;
    let store_path = ctx.store_path;
    let main_agent = ctx.main_agent;
    let short_term = ctx.short_term.clone();
    let tools_executor = ctx.tools_executor;
if method == "GET" && path.starts_with("/api/session-state") {
    let session_id = path
        .split('?')
        .nth(1)
        .and_then(|q| {
            q.split('&').find_map(|p| {
                if let Some(v) = p.strip_prefix("session_id=") {
                    Some(
                        urlencoding::decode(v)
                            .map(|c| c.into_owned())
                            .unwrap_or_else(|_| v.to_string()),
                    )
                } else {
                    None
                }
            })
        })
        .unwrap_or_default();
    if session_id.is_empty() {
        return Some(json_response(
            "400 Bad Request",
            r#"{"error":"session_id query parameter required"}"#,
        ));
    }
    let st = crate::session_state::load(data_dir, &session_id);
    return Some(json_response(
        "200 OK",
        &serde_json::to_string(&serde_json::json!({
            "session_id": session_id,
            "state": st,
        }))
        .unwrap_or_else(|_| "{}".into()),
    ));
}

// GET /api/session/resume-brief?session_id=… — structured session state + short-term size for resume UX.
if method == "GET" && path.starts_with("/api/session/resume-brief") {
    let session_id = path
        .split('?')
        .nth(1)
        .and_then(|q| {
            q.split('&').find_map(|p| {
                if let Some(v) = p.strip_prefix("session_id=") {
                    Some(
                        urlencoding::decode(v)
                            .map(|c| c.into_owned())
                            .unwrap_or_else(|_| v.to_string()),
                    )
                } else {
                    None
                }
            })
        })
        .unwrap_or_default();
    if session_id.is_empty() {
        return Some(json_response(
            "400 Bad Request",
            r#"{"error":"session_id query_parameter_required"}"#,
        ));
    }
    let st = crate::session_state::load(data_dir, &session_id);
    let (short_term_turns, compaction_count) = match &short_term {
        Some(st_mem) => {
            let n = st_mem.get_turns(&session_id).await.len();
            let c = st_mem.get_compaction_count(&session_id).await;
            (n, c)
        }
        None => (0usize, 0u32),
    };
    let tools_policy_brief = if let Some(exec_lock) = tools_executor {
        let g = exec_lock.read().await;
        let p = &g.policy;
        serde_json::json!({
            "default_profile": p.default_profile,
            "web_search_enabled": p.web_search_enabled,
            "browser_enabled": p.browser_enabled,
            "web_crawl_enabled": p.web_crawl_enabled,
            "cloudflare_account_configured": p.resolved_cloudflare_account_id().is_some(),
            "cloudflare_token_configured": p.resolved_cloudflare_api_token().is_some(),
        })
    } else {
        serde_json::Value::Null
    };
    return Some(json_response(
        "200 OK",
        &serde_json::to_string(&serde_json::json!({
            "session_id": session_id,
            "session_state": st,
            "short_term_turn_count": short_term_turns,
            "compaction_count": compaction_count,
            "memory_recall_metrics": crate::memory_orchestrator::memory_recall_metrics_snapshot(),
            "tools_policy_brief": tools_policy_brief,
            "hint": "Use this payload to restore UI tabs (goals, constraints) and to explain context to the user after reconnect."
        }))
        .unwrap_or_else(|_| "{}".into()),
    ));
}

if method == "GET" && path == "/api/config" {
    let env_path = data_dir.join("akasha.env");
    let mut vars = std::collections::HashMap::new();
    if let Ok(s) = std::fs::read_to_string(&env_path) {
        for line in s.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
                vars.insert(k.trim().to_string(), v);
            }
        }
    }
    let body_json = serde_json::to_string(&serde_json::json!({ "vars": vars }))
        .unwrap_or_else(|_| "{}".into());
    return Some(format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body_json.len(),
        body_json
    ));
}

// POST /api/config — set one var in akasha.env (body: {"key": "K", "value": "V"})
if method == "POST" && path == "/api/config" {
    let body_json = body
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
    let key = body_json
        .as_ref()
        .and_then(|j| j.get("key"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let value = body_json
        .as_ref()
        .and_then(|j| j.get("value"))
        .and_then(|v| v.as_str())
        .map(String::from);
    match (key, value) {
        (Some(k), Some(v)) if !k.is_empty() => {
            let env_path = data_dir.join("akasha.env");
            let mut lines: Vec<String> = if env_path.exists() {
                std::fs::read_to_string(&env_path)
                    .unwrap_or_default()
                    .lines()
                    .map(String::from)
                    .collect()
            } else {
                vec!["# akasha.env".to_string(), "".to_string()]
            };
            let new_line = format!("{}={}", k, v);
            let mut found = false;
            for line in lines.iter_mut() {
                if line.trim_start().starts_with(&format!("{}=", k)) {
                    *line = new_line.clone();
                    found = true;
                    break;
                }
            }
            if !found {
                lines.push(new_line);
            }
            if std::fs::write(&env_path, lines.join("\n")).is_ok() {
                return Some(json_response(
                    "200 OK",
                    &serde_json::json!({ "ok": true, "key": k }).to_string(),
                ));
            }
        }
        _ => {}
    }
    return Some(json_response("400 Bad Request", r#"{"error":"missing key or value"}"#));
}

if method == "POST" && path == "/api/session/handoff" {
    let body_json = body
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
    let Some(body_json) = body_json else {
        return Some(json_response("400 Bad Request", r#"{"error":"invalid_json"}"#));
    };
    let session_id = body_json
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let Some(session_id) = session_id else {
        return Some(json_response("400 Bad Request", r#"{"error":"missing_session_id"}"#));
    };
    let target_model = body_json
        .get("target_model")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let target_provider = body_json
        .get("target_provider")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let task_id_filter = body_json
        .get("task_id")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let task_uuid = match task_id_filter {
        Some(ref s) => match Uuid::parse_str(s) {
            Ok(id) => Some(id),
            Err(_) => {
                return Some(json_response("400 Bad Request", r#"{"error":"invalid_task_id"}"#));
            }
        },
        None => None,
    };
    let transcript_ctx = if let Some(tid) = task_uuid {
        let transcript_path = data_dir.join("transcripts").join(format!("{tid}.json"));
        if let Ok(raw) = std::fs::read_to_string(&transcript_path) {
            serde_json::from_str::<serde_json::Value>(&raw)
                .ok()
                .map(|v| {
                    let status = v
                        .get("status")
                        .and_then(|x| x.as_str())
                        .unwrap_or("unknown");
                    let summary = v
                        .get("summary")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .chars()
                        .take(1800)
                        .collect::<String>();
                    let actions = v
                        .get("actions")
                        .and_then(|x| x.as_array())
                        .map(|arr| {
                            arr.iter()
                                .take(8)
                                .filter_map(|a| a.get("summary").and_then(|s| s.as_str()))
                                .map(|s| format!("- {}", s.trim()))
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                        .unwrap_or_default();
                    format!(
                        "[Handoff transcript]\n- source_task_id: {tid}\n- status: {status}\n- summary: {summary}\n{}",
                        if actions.is_empty() {
                            String::new()
                        } else {
                            format!("\n[Recent actions]\n{actions}\n")
                        }
                    )
                })
                .unwrap_or_default()
        } else {
            String::new()
        }
    } else {
        String::new()
    };
    let model_route = match (&target_provider, &target_model) {
        (Some(p), Some(m)) => format!("{p}/{m}"),
        (Some(p), None) => p.clone(),
        (None, Some(m)) => m.clone(),
        (None, None) => "auto".to_string(),
    };
    let route_hint = if target_provider.is_some() || target_model.is_some() {
        format!(
            "[Handoff route request]\n- target_provider: {}\n- target_model: {}\nUse this route preference if configured; otherwise fall back to the nearest available route and mention the fallback.\n\n",
            target_provider.as_deref().unwrap_or(""),
            target_model.as_deref().unwrap_or("")
        )
    } else {
        String::new()
    };
    let handoff_prompt = format!(
        "{}{}\n[Instruction]\nContinue the conversation from this handoff context and produce the next actionable response.",
        route_hint, transcript_ctx
    );
    let envelope = crate::gateway::MessageEnvelope::api(
        session_id.clone(),
        handoff_prompt,
        None,
        TaskPriority::UserNormal,
        false,
    );
    match crate::gateway::handle_envelope(main_agent, store_path, envelope).await {
        Ok(task_id) => {
            let body = serde_json::json!({
                "schema_version": 2,
                "task_id": task_id.to_string(),
                "session_id": session_id,
                "model": model_route
            });
            return Some(json_response("200 OK", &body.to_string()));
        }
        Err(_) => {
            return Some(json_response("500 Internal Server Error", r#"{"error":"handle_failed"}"#));
        }
    }
}

if method == "GET" && path == "/api/discovery" {
    let profiles: Vec<serde_json::Value> = akasha_core::service_discovery::list_profiles()
        .iter()
        .map(|p| {
            serde_json::json!({
                "id": p.id,
                "display_name": p.display_name,
                "port": p.port,
                "install_url": p.install_url,
            })
        })
        .collect();
    let body = serde_json::json!({ "profiles": profiles });
    return Some(json_response("200 OK", &body.to_string()));
}
if method == "GET" && path.starts_with("/api/discovery/") {
    let service_id = path.trim_start_matches("/api/discovery/").trim();
    if service_id.is_empty() || service_id.contains('/') {
        return Some(json_response("400 Bad Request", r#"{"error":"invalid_service_id"}"#));
    }
    let Some(profile) = akasha_core::service_discovery::profile(service_id) else {
        let body = serde_json::json!({ "error": "unknown_service", "service_id": service_id });
        return Some(json_response("404 Not Found", &body.to_string()));
    };
    let opts = akasha_core::service_discovery::DiscoveryOptions::from_env();
    let instances = akasha_core::service_discovery::discover(profile, &opts).await;
    let body = serde_json::json!({
        "service_id": service_id,
        "instances": instances,
        "install_url": profile.install_url,
    });
    return Some(json_response("200 OK", &body.to_string()));
}
if method == "GET" && path == "/api/connectors" {
    let connectors = crate::connectors_config::connectors_status(data_dir);
    let config = crate::connectors_config::connectors_config_view(data_dir);
    let restart_required = connectors
        .iter()
        .any(|c| c.enabled_in_file != c.active_in_process);
    let body = serde_json::json!({
        "connectors": connectors,
        "config": config,
        "restart_required": restart_required,
        "matrix_note": "Matrix uses plugin matrix-channel sidecar and MATRIX_* env vars (manual setup).",
        "homeassistant_note": "Enable AKASHA_HOMEASSISTANT_ENABLED=1, set HA_BASE_URL (or akasha discover homeassistant), vault ha_access_token; install plugin homeassistant."
    });
    return Some(json_response("200 OK", &body.to_string()));
}
if method == "POST" && path == "/api/connectors" {
    let body_json = body
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
    let Some(ref j) = body_json else {
        return Some(json_response("400 Bad Request", r#"{"error":"invalid_json"}"#));
    };
    let update: crate::connectors_config::ConnectorsUpdate =
        match serde_json::from_value(j.clone()) {
            Ok(u) => u,
            Err(e) => {
                let body = serde_json::json!({
                    "error": "invalid_body",
                    "detail": e.to_string()
                });
                return Some(json_response("400 Bad Request", &body.to_string()));
            }
        };
    match crate::connectors_config::apply_connectors_update(data_dir, &update) {
        Ok(()) => {
            let body = serde_json::json!({ "ok": true, "restart_required": true });
            return Some(json_response("200 OK", &body.to_string()));
        }
        Err(e) => {
            let body = serde_json::json!({
                "error": "save_failed",
                "detail": e.to_string()
            });
            return Some(json_response("500 Internal Server Error", &body.to_string()));
        }
    }
}
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn config_paths_smoke() {
        for (p, m) in [
            ("/api/session-state", "GET"),
            ("/api/session/resume-brief", "GET"),
            ("/api/session/handoff", "POST"),
            ("/api/config", "GET"),
            ("/api/connectors", "GET"),
            ("/api/discovery", "GET"),
        ] {
            assert!(matches_config(p, m), "{m} {p}");
        }
    }

    fn matches_config(path: &str, method: &str) -> bool {
        path.starts_with("/api/session")
            || path == "/api/session-state"
            || path.starts_with("/api/config")
            || path.starts_with("/api/connectors")
            || path.starts_with("/api/discovery")
            || (method == "POST" && path == "/api/config")
    }
}
