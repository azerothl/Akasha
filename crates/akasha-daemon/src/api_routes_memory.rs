//! Memory, agent identity, and second-brain routes.

use crate::api::{load_second_brain_settings, save_second_brain_settings, SecondBrainSettings};
use crate::api_http::json_response;
use crate::memory_actor::LongTermMemoryClient;
use std::path::Path;
use std::sync::Arc;

pub struct RouteCtx<'a> {
    pub data_dir: &'a Path,
    pub short_term: Option<Arc<crate::memory::ShortTermStore>>,
    pub long_term_client: Option<LongTermMemoryClient>,
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
    let short_term = ctx.short_term.clone();
    let long_term_client = ctx.long_term_client.clone();
if method == "GET" && path == "/api/agent-identity" {
    let identity = crate::memory_agent_identity::AgentIdentityManifest::load(data_dir);
    return Some(json_response(
        "200 OK",
        &serde_json::json!({ "identity": identity }).to_string(),
    ));
}
if method == "POST" && path == "/api/agent-identity" {
    let body_json = body
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
    let mut identity = body_json
        .as_ref()
        .and_then(|v| v.get("identity"))
        .and_then(|v| serde_json::from_value::<crate::memory_agent_identity::AgentIdentityManifest>(v.clone()).ok())
        .unwrap_or_else(|| crate::memory_agent_identity::AgentIdentityManifest::load(data_dir));
    if let Some(v) = body_json.as_ref().and_then(|j| j.get("name")).and_then(|x| x.as_str()) {
        identity.name = Some(v.to_string());
    }
    if let Some(v) = body_json.as_ref().and_then(|j| j.get("role")).and_then(|x| x.as_str()) {
        identity.role = Some(v.to_string());
    }
    if let Some(v) = body_json.as_ref().and_then(|j| j.get("tone")).and_then(|x| x.as_str()) {
        identity.tone = Some(v.to_string());
    }
    if let Some(v) = body_json.as_ref().and_then(|j| j.get("values")).and_then(|x| x.as_array()) {
        identity.values = Some(
            v.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect(),
        );
    }
    if let Some(v) = body_json
        .as_ref()
        .and_then(|j| j.get("constraints"))
        .and_then(|x| x.as_array())
    {
        identity.constraints = Some(
            v.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect(),
        );
    }
    match crate::memory_agent_identity::save(data_dir, &identity) {
        Ok(()) => {
            crate::memory_agent_identity::bootstrap_agent_identity(data_dir, long_term_client.as_ref());
            return Some(json_response(
                "200 OK",
                &serde_json::json!({ "ok": true, "identity": identity }).to_string(),
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
if method == "GET" && path == "/api/memory/advanced-settings" {
    let settings = crate::memory_retrieval_enhance::advanced_settings_snapshot();
    return Some(json_response("200 OK", &serde_json::json!({ "settings": settings }).to_string()));
}
if method == "POST" && path == "/api/memory/advanced-settings" {
    let body_json = body
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
    if let Some(ref j) = body_json {
        crate::memory_retrieval_enhance::apply_advanced_settings(j);
    }
    let settings = crate::memory_retrieval_enhance::advanced_settings_snapshot();
    return Some(json_response(
        "200 OK",
        &serde_json::json!({ "ok": true, "settings": settings }).to_string(),
    ));
}
if method == "GET" && path == "/api/memory/second-brain/settings" {
    let settings = load_second_brain_settings(data_dir);
    let body = serde_json::json!({ "settings": settings });
    return Some(json_response("200 OK", &body.to_string()));
}
if method == "POST" && path == "/api/memory/second-brain/settings" {
    let body_json = body
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
    let mut settings = load_second_brain_settings(data_dir);
    if let Some(enabled) = body_json
        .as_ref()
        .and_then(|v| v.get("enabled").and_then(|b| b.as_bool()))
    {
        settings.enabled = enabled;
    }
    if let Some(paused) = body_json
        .as_ref()
        .and_then(|v| v.get("paused").and_then(|b| b.as_bool()))
    {
        settings.paused = paused;
    }
    match save_second_brain_settings(data_dir, &settings) {
        Ok(()) => {
            return Some(json_response(
                "200 OK",
                &serde_json::json!({ "ok": true, "settings": settings }).to_string(),
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
if method == "GET" && path.starts_with("/api/memory/second-brain/overview") {
    let settings = load_second_brain_settings(data_dir);
    let (entries, total) = if let Some(ref client) = long_term_client {
        let client = client.clone();
        tokio::task::spawn_blocking(move || client.list(500, 0))
            .await
            .unwrap_or((vec![], 0))
    } else {
        (vec![], 0)
    };
    let mut by_type: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    for (_, _, source, _) in entries {
        let t = if source.contains("preference") {
            "preference"
        } else if source.contains("goal") {
            "goal"
        } else if source.contains("project") {
            "project"
        } else if source.contains("decision") {
            "decision"
        } else if source.contains("constraint") {
            "constraint"
        } else if source.contains("identity") || source.contains("user_fact") {
            "identity"
        } else {
            "other"
        };
        *by_type.entry(t.to_string()).or_insert(0) += 1;
    }
    let body = serde_json::json!({
        "settings": settings,
        "total_entries": total,
        "typed_counts": by_type
    });
    return Some(json_response("200 OK", &body.to_string()));
}
if method == "POST" && path == "/api/memory/second-brain/clear" {
    let mut deleted = 0u64;
    if let Some(ref client) = long_term_client {
        let client = client.clone();
        deleted = tokio::task::spawn_blocking(move || {
            let mut removed = 0u64;
            loop {
                let (rows, _total) = client.list(200, 0);
                if rows.is_empty() {
                    break;
                }
                for (id, _content, _source, _created) in rows {
                    if client.delete(id).is_ok() {
                        removed += 1;
                    }
                }
            }
            removed
        })
        .await
        .unwrap_or(0);
    }
    let body = serde_json::json!({ "ok": true, "deleted_entries": deleted });
    return Some(json_response("200 OK", &body.to_string()));
}

if method == "GET" && path == "/api/memory/hygiene-status" {
    return Some(json_response(
        "200 OK",
        &crate::memory_hygiene::metrics_snapshot().to_string(),
    ));
}

// GET /api/memory/branch/:session_id — branch overview (E3)
if method == "GET" && path.starts_with("/api/memory/branch/") {
    let session_id = path
        .trim_start_matches("/api/memory/branch/")
        .split('?')
        .next()
        .unwrap_or("")
        .trim();
    if session_id.is_empty() {
        return Some(json_response("400 Bad Request", r#"{"error":"session_id_required"}"#));
    }
    let db = data_dir.join("memory.db");
    match crate::memory_branch::branch_overview(&db, session_id) {
        Ok(v) => return Some(json_response("200 OK", &v.to_string())),
        Err(e) => {
            return Some(json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            ));
        }
    }
}

// POST /api/memory/branch/:session_id — clone session memory to target branch (E3)
if method == "POST" && path.starts_with("/api/memory/branch/") {
    let source_session = path
        .trim_start_matches("/api/memory/branch/")
        .split('?')
        .next()
        .unwrap_or("")
        .trim();
    if source_session.is_empty() {
        return Some(json_response("400 Bad Request", r#"{"error":"session_id_required"}"#));
    }
    let body_json = body
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
    let target_session = body_json
        .as_ref()
        .and_then(|j| j.get("target_session_id").and_then(|v| v.as_str()))
        .unwrap_or("")
        .trim()
        .to_string();
    if target_session.is_empty() {
        return Some(json_response(
            "400 Bad Request",
            r#"{"error":"target_session_id_required"}"#,
        ));
    }
    let Some(ref client) = long_term_client else {
        return Some(json_response(
            "503 Service Unavailable",
            r#"{"error":"long_term_memory_unavailable"}"#,
        ));
    };
    let db = data_dir.join("memory.db");
    let client = client.clone();
    let source = source_session.to_string();
    let target_for_clone = target_session.clone();
    let result = tokio::task::spawn_blocking(move || {
        crate::memory_branch::clone_branch(&client, &db, &source, &target_for_clone)
    })
    .await;
    match result {
        Ok(Ok((lt, ep))) => {
            return Some(json_response(
                "200 OK",
                &serde_json::json!({
                    "ok": true,
                    "source_session_id": source_session,
                    "target_session_id": target_session,
                    "long_term_cloned": lt,
                    "episodic_cloned": ep,
                })
                .to_string(),
            ));
        }
        Ok(Err(e)) => {
            return Some(json_response(
                "400 Bad Request",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            ));
        }
        Err(e) => {
            return Some(json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            ));
        }
    }
}

if method == "GET" && path == "/api/memory/export" {
    let db = data_dir.join("memory.db");
    match crate::memory_export::export_memory(&db) {
        Ok(bundle) => {
            return Some(json_response(
                "200 OK",
                &serde_json::to_string(&bundle).unwrap_or_else(|_| "{}".into()),
            ));
        }
        Err(e) => {
            return Some(json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            ));
        }
    }
}

if method == "POST" && path == "/api/memory/import" {
    let parsed = body
        .as_deref()
        .and_then(|b| serde_json::from_slice::<crate::memory_export::MemoryExportBundle>(b).ok());
    let Some(bundle) = parsed else {
        return Some(json_response("400 Bad Request", r#"{"error":"invalid_json"}"#));
    };
    let db = data_dir.join("memory.db");
    match crate::memory_export::import_memory(&db, &bundle) {
        Ok((entries, facts)) => {
            return Some(json_response(
                "200 OK",
                &serde_json::json!({ "entries_imported": entries, "facts_imported": facts })
                    .to_string(),
            ));
        }
        Err(e) => {
            return Some(json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            ));
        }
    }
}

if method == "GET" && path == "/api/memory/recall-metrics" {
    if let Some(cached) = crate::http_get_cache::cache_get_recall_metrics() {
        return Some(json_response("200 OK", &cached));
    }
    let mut body = crate::memory_orchestrator::memory_recall_metrics_snapshot();
    if let Some(obj) = body.as_object_mut() {
        if let Some(m) = crate::memory_maintenance::metrics_snapshot().as_object() {
            for (k, v) in m {
                obj.insert(k.clone(), v.clone());
            }
        }
        if let Some(m) = crate::memory_hygiene::metrics_snapshot().as_object() {
            for (k, v) in m {
                obj.insert(k.clone(), v.clone());
            }
        }
    }
    let body_str = serde_json::to_string(&body).unwrap_or_else(|_| "{}".into());
    crate::http_get_cache::cache_put_recall_metrics(&body_str);
    return Some(json_response("200 OK", &body_str));
}

// GET /api/memory/short-term?session_id=... — turns for session (default: day-YYYY-MM-DD)
if method == "GET" && path.starts_with("/api/memory/short-term") {
    let session_id = path
        .split('?')
        .nth(1)
        .and_then(|q| {
            q.split('&')
                .find(|p| p.starts_with("session_id="))
                .map(|p| {
                    urlencoding::decode(p.trim_start_matches("session_id="))
                        .unwrap_or_default()
                        .into_owned()
                })
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("day-{}", chrono::Utc::now().format("%Y-%m-%d")));
    let turns = if let Some(ref st) = short_term {
        st.get_turns(&session_id).await
    } else {
        vec![]
    };
    let list: Vec<serde_json::Value> = turns
        .iter()
        .map(|t| serde_json::json!({ "role": t.role, "content": t.content }))
        .collect();
    let body_json = serde_json::json!({ "session_id": session_id, "turns": list });
    return Some(json_response("200 OK", &body_json.to_string()));
}

// DELETE /api/memory/session?session_id=... — remove short-term turns and session_state for this session
if method == "DELETE" && path.starts_with("/api/memory/session") {
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
    if session_id.is_empty() || !crate::memory::is_safe_session_id(&session_id) {
        return Some(json_response(
            "400 Bad Request",
            r#"{"error":"invalid or missing session_id"}"#,
        ));
    }
    if let Some(ref st) = short_term {
        st.delete_session(&session_id).await;
    } else {
        let path = data_dir
            .join("short_term")
            .join(format!("{}.json", session_id));
        let _ = tokio::fs::remove_file(path).await;
    }
    let _ = crate::session_state::delete(data_dir, &session_id);
    let body_json = serde_json::json!({ "ok": true, "session_id": session_id });
    return Some(json_response("200 OK", &body_json.to_string()));
}

// GET /api/memory/search?q=...&top_k=... — semantic search in long-term memory
if method == "GET" && path.starts_with("/api/memory/search") {
    let (q, top_k) = path
        .split('?')
        .nth(1)
        .map(|query_str| {
            let mut q = None;
            let mut top_k = 10u32;
            for part in query_str.split('&') {
                if let Some(v) = part.strip_prefix("q=") {
                    let decoded = urlencoding::decode(v)
                        .unwrap_or_else(|_| std::borrow::Cow::Borrowed(v));
                    q = Some(decoded.trim().to_string());
                } else if let Some(v) = part.strip_prefix("top_k=") {
                    if let Ok(n) = v.parse::<u32>() {
                        top_k = n.min(20);
                    }
                }
            }
            (q, top_k)
        })
        .unwrap_or((None, 10));
    let query = q.as_deref().map(|s| s.trim()).unwrap_or("");
    if query.is_empty() {
        return Some(json_response("400 Bad Request", r#"{"error":"missing or empty q"}"#));
    }
    let result = match long_term_client {
        Some(ref client) => {
            let client = client.clone();
            let query = query.to_string();
            let top_k = top_k as usize;
            tokio::task::spawn_blocking(move || client.search(query, top_k, None))
                .await
                .unwrap_or_default()
        }
        None => {
            return Some(json_response(
                "503 Service Unavailable",
                r#"{"error":"long-term memory not available","long_term_available":false}"#,
            ));
        }
    };
    let results: Vec<serde_json::Value> = result
        .iter()
        .map(|(id, content)| serde_json::json!({ "id": id, "content": content }))
        .collect();
    let body_json = serde_json::json!({
        "results": results,
        "long_term_available": true
    });
    return Some(json_response("200 OK", &body_json.to_string()));
}

// GET /api/memory/long-term?limit=200&offset=0 — recent long-term entries (content, created_at, source, related), paginated
if method == "GET" && path.starts_with("/api/memory/long-term") {
    let (limit, offset) = path
        .split('?')
        .nth(1)
        .map(|q| {
            let mut limit = 200usize;
            let mut offset = 0usize;
            for part in q.split('&') {
                if let Some(v) = part.strip_prefix("limit=") {
                    if let Ok(n) = v.parse::<usize>() {
                        limit = n.min(200);
                    }
                } else if let Some(v) = part.strip_prefix("offset=") {
                    if let Ok(n) = v.parse::<usize>() {
                        offset = n;
                    }
                }
            }
            (limit, offset)
        })
        .unwrap_or((200, 0));
    let (entries, total) = if let Some(ref client) = long_term_client {
        let client = client.clone();
        tokio::task::spawn_blocking(move || client.list(limit, offset))
            .await
            .unwrap_or((vec![], 0))
    } else {
        (vec![], 0)
    };
    let relations = if !entries.is_empty() {
        if let Some(ref client) = long_term_client {
            let ids: Vec<String> = entries.iter().map(|(id, _, _, _)| id.clone()).collect();
            let client = client.clone();
            tokio::task::spawn_blocking(move || client.get_relations_for_entries(ids))
                .await
                .unwrap_or_default()
        } else {
            std::collections::HashMap::new()
        }
    } else {
        std::collections::HashMap::new()
    };
    let list: Vec<serde_json::Value> = entries
        .iter()
        .map(|(id, content, created_at, source)| {
            let related = relations
                .get(id)
                .map(|v| {
                    v.iter()
                        .map(|(to_id, kind)| serde_json::json!({ "id": to_id, "kind": kind }))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            serde_json::json!({ "id": id, "content": content, "created_at": created_at, "source": source, "related": related })
        })
        .collect();
    let body_json = serde_json::json!({
        "entries": list,
        "total": total,
        "long_term_available": long_term_client.is_some()
    });
    return Some(json_response("200 OK", &body_json.to_string()));
}

// POST /api/memory/rebuild-relations — recompute embedding-based relations (similar / relates_to tiers)
if method == "POST" && path == "/api/memory/rebuild-relations" {
    const DEFAULT_MAX_PER_ENTRY: usize = 5;
    let result = match long_term_client {
        Some(ref client) => {
            let client = client.clone();
            tokio::task::spawn_blocking(move || {
                client.rebuild_similar_relations(DEFAULT_MAX_PER_ENTRY)
            })
            .await
            .unwrap_or_else(|e| Err(format!("task join error: {}", e)))
        }
        None => Err("long-term memory not available".to_string()),
    };
    match result {
        Ok(inserted) => {
            let body_json = serde_json::json!({ "inserted": inserted, "ok": true });
            return Some(json_response("200 OK", &body_json.to_string()));
        }
        Err(e) => {
            let body_json = serde_json::json!({ "error": e, "ok": false });
            return Some(json_response("500 Internal Server Error", &body_json.to_string()));
        }
    }
}

// DELETE /api/memory/long-term/:id — delete one long-term memory entry by id
if method == "DELETE" && path.starts_with("/api/memory/long-term/") {
    let id = path
        .trim_start_matches("/api/memory/long-term/")
        .split('?')
        .next()
        .unwrap_or("")
        .trim();
    if id.is_empty() {
        return Some(json_response("400 Bad Request", r#"{"error":"missing id"}"#));
    }
    let result = match long_term_client {
        Some(ref client) => {
            let client = client.clone();
            let id = id.to_string();
            tokio::task::spawn_blocking(move || client.delete(id))
                .await
                .unwrap_or_else(|e| Err(format!("task join error: {}", e)))
        }
        None => Err("long-term memory not available".to_string()),
    };
    match result {
        Ok(()) => return Some(json_response("200 OK", r#"{"deleted":true}"#)),
        Err(e) if e == "not found" || e == "invalid uuid" => {
            return Some(json_response("404 Not Found", &format!(r#"{{"error":"{}"}}"#, e)));
        }
        Err(e) => {
            return Some(json_response(
                "500 Internal Server Error",
                &format!(r#"{{"error":"{}"}}"#, e),
            ));
        }
    }
}
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn memory_paths_smoke() {
        for p in ["/api/memory/search", "/api/agent-identity", "/api/memory/export"] {
            assert!(p.starts_with("/api/memory") || p == "/api/agent-identity");
        }
    }
}
