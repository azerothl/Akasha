//! CSRF / Origin checks and small HTTP path helpers for `handle_api`.

use crate::api_http::json_response;
use std::collections::HashMap;

/// Split request path into path and query (without leading `?`).
pub fn split_path_query(path: &str) -> (&str, &str) {
    path.split_once('?').map(|(p, q)| (p, q)).unwrap_or((path, ""))
}

/// Parse a single query parameter (first match), with URL decoding.
pub fn parse_query_param(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        if let Some((k, v)) = pair.split_once('=') {
            if k == key {
                return Some(
                    urlencoding::decode(v)
                        .map(|c| c.into_owned())
                        .unwrap_or_else(|_| v.to_string()),
                );
            }
        }
    }
    None
}

/// Reject state-changing browser requests from non-local origins (CSRF).
/// Returns `Some(full HTTP response)` when the request must be blocked.
pub fn csrf_reject_response(
    method: &str,
    path: &str,
    headers: &HashMap<String, String>,
) -> Option<String> {
    if !matches!(method, "POST" | "PUT" | "DELETE" | "PATCH") {
        return None;
    }
    let origin = headers.get("origin")?.trim();
    let is_local = origin == "null"
        || origin.starts_with("http://localhost")
        || origin.starts_with("http://127.0.0.1")
        || origin.starts_with("https://localhost")
        || origin.starts_with("https://127.0.0.1")
        || origin.starts_with("http://tauri.localhost")
        || origin.starts_with("https://tauri.localhost")
        || origin.starts_with("tauri://");
    if is_local {
        return None;
    }
    tracing::warn!(
        origin = %origin,
        method = %method,
        path = %path,
        "CSRF: rejected request from non-local origin"
    );
    Some(json_response(
        "403 Forbidden",
        r#"{"error":"origin_not_allowed"}"#,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_map_origin(origin: &str) -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert("origin".to_string(), origin.to_string());
        m
    }

    fn origin_allowed(origin: &str) -> bool {
        csrf_reject_response("POST", "/api/x", &header_map_origin(origin)).is_none()
    }

    #[test]
    fn split_path_query_splits() {
        assert_eq!(
            split_path_query("/api/foo?a=1&b=2"),
            ("/api/foo", "a=1&b=2")
        );
        assert_eq!(split_path_query("/api/foo"), ("/api/foo", ""));
    }

    #[test]
    fn parse_query_param_decodes() {
        assert_eq!(
            parse_query_param("limit=10&x=y", "limit").as_deref(),
            Some("10")
        );
        assert_eq!(parse_query_param("", "limit"), None);
    }

    #[test]
    fn csrf_allows_local_origins() {
        assert!(origin_allowed("http://localhost:5173"));
        assert!(origin_allowed("http://127.0.0.1:3876"));
        assert!(origin_allowed("https://tauri.localhost/"));
        assert!(origin_allowed("tauri://localhost"));
        assert!(origin_allowed("null"));
    }

    #[test]
    fn csrf_blocks_unknown_origin_for_post() {
        let m = header_map_origin("https://evil.example");
        assert!(csrf_reject_response("POST", "/api/message", &m).is_some());
    }

    #[test]
    fn csrf_get_never_blocked_by_origin() {
        let m = header_map_origin("https://evil.example");
        assert!(csrf_reject_response("GET", "/api/foo", &m).is_none());
    }

    #[test]
    fn csrf_no_origin_header_means_no_block() {
        let m = HashMap::new();
        assert!(csrf_reject_response("POST", "/api/foo", &m).is_none());
    }
}
