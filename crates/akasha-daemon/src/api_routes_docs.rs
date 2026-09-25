//! Health, status, update banner, process watch, and user docs routes.

use crate::api::UpdateCheckCache;
use crate::api_http::json_response;
use std::path::Path;

pub struct RouteCtx<'a> {
    pub data_dir: &'a Path,
    pub spec_dir: &'a Path,
    pub update_cache: &'a UpdateCheckCache,
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
    _body: Option<&[u8]>,
    ctx: &RouteCtx<'_>,
) -> Option<String> {
    let path = full_path(path_only, query_str);
    let data_dir = ctx.data_dir;
    let spec_dir = ctx.spec_dir;
    let update_cache = ctx.update_cache;
if method == "GET" && path_only == "/api/process/watch/recent" {
    let limit_opt = crate::api_security::parse_query_param(query_str.unwrap_or(""), "limit")
        .and_then(|s| s.parse::<usize>().ok());
    let can_cache = limit_opt.unwrap_or(50) == 50;
    if can_cache {
        if let Some(cached) = crate::http_get_cache::cache_get_process_watch_recent() {
            return Some(json_response("200 OK", &cached));
        }
    }
    let limit = limit_opt.unwrap_or(50);
    let ev = crate::process_watch::recent(limit).await;
    let body = serde_json::to_string(&ev).unwrap_or_else(|_| "[]".to_string());
    if can_cache {
        crate::http_get_cache::cache_put_process_watch_recent(&body);
    }
    return Some(json_response(
        "200 OK",
        &body,
    ));
}

if method == "GET" && (path == "/" || path.is_empty()) {
    return Some(json_response("200 OK", r#"{"status":"ok"}"#));
}

if method == "GET" && path == "/api/update/status" {
    let status = update_cache.read().await;
    let body = serde_json::json!({
        "remote_version": status.remote_version,
        "download_url": status.download_url,
        "release_notes_url": status.release_notes_url,
        "last_checked_at": status.last_checked_at.map(|t| t.to_rfc3339()),
        "error": status.error,
    });
    return Some(json_response("200 OK", &body.to_string()));
}

if method == "GET" && path == "/api/status" {
    return Some(json_response("200 OK", r#"{"status":"ok"}"#));
}

// GET /api/docs — multi-page user documentation (JSON index or page markdown)
if method == "GET" && path_only.starts_with("/api/docs") {
    if let Some(body_json) =
        crate::user_docs::handle_docs_get(path_only, query_str.unwrap_or(""), spec_dir, data_dir)
    {
        return Some(json_response("200 OK", &body_json));
    }
}
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn docs_paths_smoke() {
        assert!(("/", "GET").is_ok_for_docs());
        assert!(("/api/status", "GET").is_ok_for_docs());
        assert!(("/api/update/status", "GET").is_ok_for_docs());
        assert!(("/api/docs", "GET").is_ok_for_docs());
        assert!(("/api/process/watch/recent", "GET").is_ok_for_docs());
        assert!(!("/api/tasks", "GET").is_ok_for_docs());
    }

    trait PathSmoke {
        fn is_ok_for_docs(self) -> bool;
    }
    impl PathSmoke for (&str, &str) {
        fn is_ok_for_docs(self) -> bool {
            let (p, m) = self;
            m == "GET"
                && (p == "/"
                    || p == "/api/status"
                    || p == "/api/update/status"
                    || p.starts_with("/api/docs")
                    || p == "/api/process/watch/recent")
        }
    }
}
