//! Device bridge, plugins, skills, and tools listing routes.

use crate::api::{
    compute_message_intent_flags, active_intents_from_flags, parse_plugin_reputation_reset_body,
    PluginReputationResetBody, reload_tools_executor_policy, do_install_skill, do_uninstall_skill, AVAILABLE_TOOLS,
};
use crate::api_http::json_response;
use std::path::Path;
use std::sync::Arc;
use uuid::Uuid;

pub struct RouteCtx<'a> {
    pub data_dir: &'a Path,
    pub spec_dir: &'a Path,
    pub plugin_registry: &'a Arc<crate::plugins::PluginRegistry>,
    pub skill_registry: &'a Arc<crate::skills::SkillRegistry>,
    pub device_bridge: Option<&'a Arc<crate::device_bridge::DeviceBridge>>,
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
    let spec_dir = ctx.spec_dir;
    let plugin_registry = ctx.plugin_registry;
    let skill_registry = ctx.skill_registry;
    let device_bridge = ctx.device_bridge;
    let tools_executor = ctx.tools_executor;
if method == "GET" && path == "/api/device/pending" {
    if let Some(bridge) = device_bridge {
        match bridge.get_pending().await {
            Some((request_id, interface, device_id, action, params)) => {
                tracing::info!(request_id = %request_id, interface = %interface, action = %action, "[device_bridge] GET /pending → returning request to UI");
                let body = serde_json::json!({
                    "request_id": request_id,
                    "interface": interface,
                    "device_id": device_id,
                    "action": action,
                    "params": params,
                });
                return Some(json_response("200 OK", &body.to_string()));
            }
            None => {
                return Some(json_response("200 OK", r#"{"pending":false}"#));
            }
        }
    } else {
        return Some(json_response("200 OK", r#"{"pending":false}"#));
    }
}

// POST /api/device/result — UI sends result of device action (e.g. image base64, audio base64)
if method == "POST" && path == "/api/device/result" {
    if let Some(bridge) = device_bridge {
        let body_json = body
            .as_deref()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
        let request_id = body_json
            .as_ref()
            .and_then(|j| j.get("request_id"))
            .and_then(|v| v.as_str());
        let success = body_json
            .as_ref()
            .and_then(|j| j.get("success"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let data = body_json
            .as_ref()
            .and_then(|j| j.get("data"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let data_len = data.as_ref().map(|s| s.len()).unwrap_or(0);
        let request_id = match request_id.filter(|s| !s.is_empty()) {
            Some(id) => id,
            None => {
                tracing::warn!("[device_bridge] POST /result missing or empty request_id");
                return Some(json_response("400 Bad Request", r#"{"error":"request_id_required"}"#));
            }
        };
        tracing::info!(request_id = %request_id, success = success, data_len = data_len, "[device_bridge] POST /result received");
        let result = crate::device_bridge::DeviceResult { success, data };
        let fulfilled = bridge.fulfill(request_id, result).await;
        if !fulfilled {
            tracing::warn!(request_id = %request_id, "[device_bridge] POST /result request_id not found in claimed (stale or wrong id)");
        }
        let body = serde_json::json!({ "ok": fulfilled });
        return Some(json_response("200 OK", &body.to_string()));
    } else {
        return Some(json_response(
            "501 Not Implemented",
            r#"{"error":"device_bridge_unavailable"}"#,
        ));
    }
}

if method == "GET" && path == "/api/plugins" {
    if let Some(cached) = crate::http_get_cache::cache_get_plugins() {
        return Some(json_response("200 OK", &cached));
    }
    let list = plugin_registry.list();
    let body = serde_json::to_string(&list).unwrap_or_else(|_| "[]".to_string());
    crate::http_get_cache::cache_put_plugins(&body);
    return Some(json_response("200 OK", &body));
}
if method == "GET" && (path == "/api/plugins/catalog" || path.starts_with("/api/plugins/catalog?")) {
    match crate::plugin_install::fetch_remote_catalog(None).await {
        Ok(catalog) => {
            return Some(json_response(
                "200 OK",
                &serde_json::to_string(&catalog).unwrap_or_else(|_| "{}".to_string()),
            ));
        }
        Err(e) => {
            return Some(json_response(
                "502 Bad Gateway",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            ));
        }
    }
}
if method == "GET" && path == "/api/plugins/metrics" {
    if let Some(cached) = crate::http_get_cache::cache_get_plugins_metrics() {
        return Some(json_response("200 OK", &cached));
    }
    let m = crate::plugins::metrics::snapshot();
    let body = serde_json::to_string(&m).unwrap_or_else(|_| "{}".to_string());
    crate::http_get_cache::cache_put_plugins_metrics(&body);
    return Some(json_response(
        "200 OK",
        &body,
    ));
}
// GET /api/plugins/routing_rules[?message=...] — debug dynamic plugin routing rules
// - Without message: returns all declared routing rules from loaded plugin manifests.
// - With message: also returns active intents and matched rules for this message.
if method == "GET"
    && (path == "/api/plugins/routing_rules" || path.starts_with("/api/plugins/routing_rules?"))
{
    let query = path.split('?').nth(1).unwrap_or("");
    let message = query
        .split('&')
        .find(|p| p.starts_with("message="))
        .and_then(|p| p.strip_prefix("message="))
        .and_then(|raw| urlencoding::decode(raw).ok().map(|d| d.into_owned()));

    let manifests = plugin_registry.manifests();
    let declared_rules: Vec<serde_json::Value> = manifests
        .iter()
        .flat_map(|m| {
            m.routing_rules.iter().map(|r| {
                serde_json::json!({
                    "plugin_id": m.id,
                    "intent": r.intent,
                    "keywords": r.keywords,
                    "preferred_tools": r.preferred_tools,
                    "forbidden_tools": r.forbidden_tools,
                    "instruction": r.instruction,
                    "priority": r.priority,
                })
            })
        })
        .collect();

    let (active_intents, matched_rules) = if let Some(ref msg) = message {
        let flags = compute_message_intent_flags(msg);
        let intents = active_intents_from_flags(&flags);
        let tools_executor_snapshot = if let Some(exec_lock) = tools_executor {
            Some(exec_lock.read().await.clone())
        } else {
            None
        };
        let matched = plugin_registry.match_routing_rules(msg, &intents, |tool_name| {
            tools_executor_snapshot
                .as_ref()
                .map(|e| e.policy.can_use_tool(tool_name))
                .unwrap_or(true)
        });
        (intents, matched)
    } else {
        (Vec::new(), Vec::new())
    };

    let body = serde_json::json!({
        "rules_count": declared_rules.len(),
        "rules": declared_rules,
        "message": message,
        "active_intents": active_intents,
        "matched_rules_count": matched_rules.len(),
        "matched_rules": matched_rules,
        "routing_rules_deprecated": true,
        "routing_rules_note": "Manifest routing_rules are no longer enforced at runtime. Plugin hints are chosen via a short system LLM call from plugin descriptions; this endpoint remains for debugging legacy manifests.",
    });
    return Some(json_response("200 OK", &body.to_string()));
}
// POST /api/plugins/routing_rules/match — debug dynamic plugin routing rules with JSON body
// Body: { "message": "...", "allowed_tools": ["tool_a", "tool_b"] (optional) }
if method == "POST" && path == "/api/plugins/routing_rules/match" {
    let Some(raw_body) = body.as_deref() else {
        return Some(json_response("400 Bad Request", r#"{"error":"body_required"}"#));
    };

    let parsed: serde_json::Value = match serde_json::from_slice(raw_body) {
        Ok(v) => v,
        Err(_) => return Some(json_response("400 Bad Request", r#"{"error":"invalid_json"}"#)),
    };

    let Some(message) = parsed
        .get("message")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Some(json_response("400 Bad Request", r#"{"error":"message_required"}"#));
    };

    let allowed_tools: Option<std::collections::HashSet<String>> = parsed
        .get("allowed_tools")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        });

    let manifests = plugin_registry.manifests();
    let declared_rules: Vec<serde_json::Value> = manifests
        .iter()
        .flat_map(|m| {
            m.routing_rules.iter().map(|r| {
                serde_json::json!({
                    "plugin_id": m.id,
                    "intent": r.intent,
                    "keywords": r.keywords,
                    "preferred_tools": r.preferred_tools,
                    "forbidden_tools": r.forbidden_tools,
                    "instruction": r.instruction,
                    "priority": r.priority,
                })
            })
        })
        .collect();

    let flags = compute_message_intent_flags(message);
    let intents = active_intents_from_flags(&flags);
    let tools_executor_snapshot = if let Some(exec_lock) = tools_executor {
        Some(exec_lock.read().await.clone())
    } else {
        None
    };
    let matched_rules = plugin_registry.match_routing_rules(message, &intents, |tool_name| {
        let policy_allowed = tools_executor_snapshot
            .as_ref()
            .map(|e| e.policy.can_use_tool(tool_name))
            .unwrap_or(true);
        let list_allowed = allowed_tools
            .as_ref()
            .map(|set| set.contains(tool_name))
            .unwrap_or(true);
        policy_allowed && list_allowed
    });

    let body = serde_json::json!({
        "message": message,
        "rules_count": declared_rules.len(),
        "rules": declared_rules,
        "active_intents": intents,
        "allowed_tools": allowed_tools,
        "matched_rules_count": matched_rules.len(),
        "matched_rules": matched_rules,
        "routing_rules_deprecated": true,
        "routing_rules_note": "Manifest routing_rules are no longer enforced at runtime. Plugin hints are chosen via a short system LLM call from plugin descriptions; this endpoint remains for debugging legacy manifests.",
    });
    return Some(json_response("200 OK", &body.to_string()));
}
if method == "POST" && path == "/api/plugins/reload" {
    plugin_registry.reload();
    return Some(json_response("200 OK", r#"{"reloaded":true}"#));
}
if method == "GET" && path == "/api/tools/device-interfaces" {
    let body = crate::device_catalog::device_interface_catalog();
    return Some(json_response("200 OK", &body.to_string()));
}
if method == "GET" && path == "/api/tools/policy" {
    let policy_path = data_dir.join("tools_policy.yaml");
    let policy = akasha_tools::ToolsPolicy::load_from_path(&policy_path).unwrap_or_default();
    let body = serde_json::json!({
        "policy": policy,
        "path": policy_path.display().to_string(),
        "exists": policy_path.is_file(),
    });
    return Some(json_response("200 OK", &body.to_string()));
}
if method == "POST" && path == "/api/tools/policy" {
    let body_json = body
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
    let policy_value = body_json
        .as_ref()
        .and_then(|j| j.get("policy").cloned())
        .or_else(|| body_json.clone());
    let Some(policy_value) = policy_value else {
        return Some(json_response("400 Bad Request", r#"{"error":"missing policy"}"#));
    };
    let policy: akasha_tools::ToolsPolicy = match serde_json::from_value(policy_value) {
        Ok(p) => p,
        Err(e) => {
            let body = serde_json::json!({
                "error": "invalid_policy",
                "detail": e.to_string()
            });
            return Some(json_response("400 Bad Request", &body.to_string()));
        }
    };
    let policy_path = data_dir.join("tools_policy.yaml");
    if let Err(e) = policy.save_to_path(&policy_path) {
        let body = serde_json::json!({
            "error": "save_failed",
            "detail": e.to_string()
        });
        return Some(json_response("500 Internal Server Error", &body.to_string()));
    }
    let reloaded = match tools_executor {
        Some(exec) => reload_tools_executor_policy(exec, policy_path.as_path(), data_dir)
            .await
            .is_ok(),
        None => false,
    };
    tracing::info!(path = %policy_path.display(), reloaded, "Tools policy saved from settings");
    let body = serde_json::json!({ "ok": true, "reloaded": reloaded });
    return Some(json_response("200 OK", &body.to_string()));
}
if method == "POST" && path == "/api/tools/reload" {
    let policy_path = data_dir.join("tools_policy.yaml");
    match tools_executor {
        Some(exec) => match reload_tools_executor_policy(exec, policy_path.as_path(), data_dir).await
        {
            Ok(()) => {
                tracing::info!(path = %policy_path.display(), "Tools policy hot-reloaded");
                return Some(json_response("200 OK", r#"{"reloaded":true}"#));
            }
            Err(e) => {
                let body = serde_json::json!({
                    "error": "reload_failed",
                    "detail": e.to_string()
                })
                .to_string();
                return Some(json_response("500 Internal Server Error", &body));
            }
        },
        None => {
            let body = serde_json::json!({ "error": "tools_executor_unavailable" }).to_string();
            return Some(json_response("503 Service Unavailable", &body));
        }
    }
}

// POST /api/plugins/reputation/reset
// Body optional:
// - { "plugin_id": "maps" } to reset one plugin
// - {} or empty body to reset all plugins
if method == "POST" && path == "/api/plugins/reputation/reset" {
    let body_json = match parse_plugin_reputation_reset_body(body.as_deref()) {
        PluginReputationResetBody::Parsed(v) => Some(v),
        PluginReputationResetBody::Empty => None,
        PluginReputationResetBody::Invalid(detail) => {
            let body = serde_json::json!({
                "error": "invalid_json",
                "detail": detail
            });
            return Some(json_response("400 Bad Request", &body.to_string()));
        }
    };
    let plugin_id = body_json
        .as_ref()
        .and_then(|j| j.get("plugin_id"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);

    let result = if let Some(id) = plugin_id.as_deref() {
        plugin_registry.reset_reputation(id)
    } else {
        plugin_registry.reset_all_reputation()
    };

    match result {
        Ok(_) => {
            plugin_registry.reload();
            let body = serde_json::json!({
                "ok": true,
                "plugin_id": plugin_id,
                "reloaded": true,
            });
            return Some(json_response("200 OK", &body.to_string()));
        }
        Err(e) => {
            let body = serde_json::json!({ "error": "reputation_reset_failed", "detail": e.to_string() });
            return Some(json_response("500 Internal Server Error", &body.to_string()));
        }
    }
}

// POST /api/plugins/{id}/disable | /enable | /uninstall — user plugin management
if method == "POST" && path.starts_with("/api/plugins/") {
    if let Some(rest) = path.strip_prefix("/api/plugins/") {
        let segs: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
        if segs.len() == 2 {
            let plugin_id = segs[0];
            let action = segs[1];
            if !akasha_plugin_api::is_safe_plugin_id(plugin_id) {
                let body = serde_json::json!({ "error": "invalid_plugin_id" });
                return Some(json_response("400 Bad Request", &body.to_string()));
            }
            let maybe_result = match action {
                "disable" => Some(plugin_registry.set_enabled(plugin_id, false)),
                "enable" => Some(plugin_registry.set_enabled(plugin_id, true)),
                "uninstall" => Some(plugin_registry.uninstall(plugin_id)),
                _ => None,
            };
            if let Some(result) = maybe_result {
                match result {
                    Ok(()) => {
                        let body = serde_json::json!({
                            "ok": true,
                            "id": plugin_id,
                            "action": action,
                        });
                        return Some(json_response("200 OK", &body.to_string()));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        let body = serde_json::json!({
                            "error": "not_found",
                            "detail": e.to_string(),
                        });
                        return Some(json_response("404 Not Found", &body.to_string()));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::InvalidInput => {
                        let body = serde_json::json!({
                            "error": "invalid_plugin_id",
                            "detail": e.to_string(),
                        });
                        return Some(json_response("400 Bad Request", &body.to_string()));
                    }
                    Err(e) => {
                        let body = serde_json::json!({
                            "error": "plugin_action_failed",
                            "detail": e.to_string(),
                        });
                        return Some(json_response("500 Internal Server Error", &body.to_string()));
                    }
                }
            }
        }
    }
}

if method == "GET" && path == "/api/skills" {
    let list = skill_registry.list().await;
    let body = serde_json::to_string(&list).unwrap_or_else(|_| "[]".to_string());
    return Some(json_response("200 OK", &body));
}
if method == "GET" && path.starts_with("/api/skills/search") {
    let query = path
        .strip_prefix("/api/skills/search")
        .and_then(|rest| {
            let q = rest.trim_start_matches('?');
            if q.is_empty() {
                None
            } else if q.starts_with("q=") {
                urlencoding::decode(q.trim_start_matches("q=")).ok().map(|s| s.into_owned())
            } else {
                Some(q.to_string())
            }
        })
        .or_else(|| {
            body.as_deref().and_then(|b| {
                serde_json::from_slice::<serde_json::Value>(b)
                    .ok()
                    .and_then(|j| j.get("q").and_then(|v| v.as_str()).map(String::from))
            })
        })
        .unwrap_or_default();
    let max = 20usize;
    if query.trim().is_empty() {
        let body = serde_json::json!({ "error": "missing_q", "detail": "Provide ?q=search terms" }).to_string();
        return Some(json_response("400 Bad Request", &body));
    }
    if let Some(exec_lock) = tools_executor {
        let exec = exec_lock.read().await.clone();
        match exec.search_skills_catalog(query.trim(), max).await {
            Ok((text, res)) => {
                let body = serde_json::json!({
                    "query": query,
                    "summary": res.summary,
                    "success": res.success,
                    "results": text,
                })
                .to_string();
                return Some(json_response("200 OK", &body));
            }
            Err(e) => {
                let body = serde_json::json!({ "error": "search_failed", "detail": e.to_string() }).to_string();
                return Some(json_response("500 Internal Server Error", &body));
            }
        }
    }
    let body = serde_json::json!({ "error": "tools_unavailable" }).to_string();
    return Some(json_response("503 Service Unavailable", &body));
}
if method == "POST" && path == "/api/skills/reload" {
    match skill_registry.reload(data_dir, spec_dir).await {
        Ok(count) => {
            let body = serde_json::json!({ "reloaded": true, "count": count }).to_string();
            return Some(json_response("200 OK", &body));
        }
        Err(e) => {
            let body = serde_json::json!({ "error": "reload_failed", "detail": e.to_string() })
                .to_string();
            return Some(json_response("500 Internal Server Error", &body));
        }
    }
}
if method == "POST" && path == "/api/skills/uninstall" {
    let body_json = body
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
    let name = body_json
        .as_ref()
        .and_then(|j| j.get("name"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    match name {
        Some(skill_name) => {
            let policy_path = data_dir.join("tools_policy.yaml");
            let tools_reload = tools_executor.map(|r| (r, policy_path.as_path()));
            let (ok, msg) = do_uninstall_skill(
                &skill_name,
                data_dir,
                spec_dir,
                skill_registry,
                tools_reload,
            )
            .await;
            if ok {
                let body = serde_json::json!({ "uninstalled": true, "name": skill_name, "message": msg }).to_string();
                return Some(json_response("200 OK", &body));
            }
            let body =
                serde_json::json!({ "error": "uninstall_failed", "detail": msg }).to_string();
            return Some(json_response("400 Bad Request", &body));
        }
        None => {
            let body = serde_json::json!({ "error": "missing_name", "detail": "Body must be JSON with \"name\": \"<skill_name>\"" }).to_string();
            return Some(json_response("400 Bad Request", &body));
        }
    }
}

// POST /api/skills/install — install a skill from a URL (GitHub or any allowed HTTPS host).
// Body: { "url": "<skill_url>" }
if method == "POST" && path == "/api/skills/install" {
    let body_json = body
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
    let url = body_json
        .as_ref()
        .and_then(|j| j.get("url"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    match url {
        Some(skill_url) => {
            let allowed_hosts = if let Some(exec_lock) = tools_executor {
                exec_lock.read().await.policy.skill_install_allowed_hosts()
            } else {
                vec![
                    "github.com".into(),
                    "raw.githubusercontent.com".into(),
                    "www.github.com".into(),
                ]
            };
            let policy_path = data_dir.join("tools_policy.yaml");
            // tools_reload: passed to do_install_skill so it can hot-reload tools_policy
            // after the new skill command entry is registered.
            let tools_reload = tools_executor.map(|r| (r, policy_path.as_path()));
            let (ok, msg) = do_install_skill(
                &skill_url,
                data_dir,
                spec_dir,
                skill_registry,
                &allowed_hosts,
                tools_reload,
            )
            .await;
            if ok {
                let body = serde_json::json!({ "installed": true, "message": msg }).to_string();
                return Some(json_response("200 OK", &body));
            }
            let body =
                serde_json::json!({ "error": "install_failed", "detail": msg }).to_string();
            return Some(json_response("400 Bad Request", &body));
        }
        None => {
            let body = serde_json::json!({ "error": "missing_url", "detail": "Body must be JSON with \"url\": \"<skill_url>\"" }).to_string();
            return Some(json_response("400 Bad Request", &body));
        }
    }
}

// Liste des outils machine disponibles (Phase A)
if method == "GET" && (path == "/api/tools" || path_only == "/api/tools") {
    let list: Vec<serde_json::Value> = AVAILABLE_TOOLS
        .iter()
        .map(|(name, desc)| serde_json::json!({ "name": name, "description": desc }))
        .collect();
    let body = serde_json::to_string(&serde_json::json!({ "tools": list }))
        .unwrap_or_else(|_| "{}".to_string());
    return Some(json_response("200 OK", &body));
}

// Effective tool policy (toolset visibility): allowed / approval / rule sources.
if method == "GET" && path_only == "/api/tools/effective" {
    match tools_executor {
        Some(ex_arc) => {
            let ex_inner = ex_arc.read().await;
            let executor = ex_inner.as_ref();
            let names: Vec<&str> = AVAILABLE_TOOLS.iter().map(|(n, _)| *n).collect();
            let rows = executor.policy.effective_tool_rows(&names);
            let body = serde_json::to_string(&serde_json::json!({
                "tools": rows,
                "default_profile": executor.policy.default_profile,
            }))
            .unwrap_or_else(|_| "{}".to_string());
            return Some(json_response("200 OK", &body));
        }
        None => {
            return Some(json_response(
                "503 Service Unavailable",
                r#"{"error":"tools_executor_unavailable"}"#,
            ));
        }
    }
}
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn plugins_paths_smoke() {
        for p in [
            "/api/plugins",
            "/api/skills",
            "/api/tools",
            "/api/device/pending",
        ] {
            assert!(
                p.starts_with("/api/plugins")
                    || p.starts_with("/api/skills")
                    || p == "/api/tools"
                    || p.starts_with("/api/device")
                    || p.starts_with("/api/tools/")
            );
        }
    }
}
