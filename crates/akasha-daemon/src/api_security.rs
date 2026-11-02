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

/// Return true only when `origin` resolves to localhost / loopback.
/// Parses the host component to avoid prefix-spoofing (e.g. `http://localhost.evil.example`).
fn origin_is_local(origin: &str) -> bool {
    if origin == "null" {
        return true;
    }
    // tauri:// is the desktop app scheme — always local.
    if origin.starts_with("tauri://") {
        return true;
    }
    let host_port = if let Some(rest) = origin.strip_prefix("https://") {
        rest
    } else if let Some(rest) = origin.strip_prefix("http://") {
        rest
    } else {
        return false;
    };
    // Strip any trailing path.
    let host_port = host_port.split('/').next().unwrap_or(host_port);
    // Handle IPv6 bracket notation: [::1] or [::1]:port.
    let host = if host_port.starts_with('[') {
        host_port
            .split(']')
            .next()
            .unwrap_or(host_port)
            .trim_start_matches('[')
    } else {
        // Extract host before optional port.
        host_port.split(':').next().unwrap_or(host_port)
    };
    matches!(host, "localhost" | "127.0.0.1" | "::1" | "tauri.localhost")
}

/// Reject state-changing browser requests from non-local origins (CSRF).
/// Returns `Some(full HTTP response)` when the request must be blocked.
pub fn csrf_reject_response(
    method: &str,
    path: &str,
    headers: &HashMap<String, String>,
) -> Option<String> {
    let normalized=format!("/{}",split_path_query(path).0.trim_matches('/'));
    let path=normalized.as_str();
    // Delivery reads use the operator's GitHub identity, so browser origin checks
    // apply to them as well as mutations (including opaque sandbox origins).
    let delivery_read = method == "GET" && (path.starts_with("/api/studio/projects/") && path.contains("/delivery/") || path=="/api/tasks" || path.starts_with("/api/tasks/") || path=="/api/pending-human-input" || path=="/api/permissions/queue" || path.starts_with("/api/permissions/queue/") || path=="/api/events" || path.starts_with("/api/events/"));
    let delivery_read=delivery_read || cfg!(feature="consumer-beta") && matches!(method,"GET"|"HEAD") && path.starts_with("/api/");
    if !delivery_read && !matches!(method, "POST" | "PUT" | "DELETE" | "PATCH") {return None;}
    // Local webviews/proxies can legitimately be cross-site across loopback hosts.
    if headers.get("origin").is_some_and(|origin|origin_is_local(origin.trim()) && !((delivery_read || cfg!(feature="consumer-beta")) && origin.trim()=="null")) {return None;}
    if delivery_read && headers.get("sec-fetch-site").is_some_and(|site|site=="cross-site") {
        return Some(json_response("403 Forbidden", r#"{"error":"origin_not_allowed"}"#));
    }
    let origin = headers.get("origin")?.trim();
    if origin_is_local(origin) && !((delivery_read || cfg!(feature="consumer-beta")) && origin=="null") {
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

    #[cfg(feature="consumer-beta")]
    #[test]
    fn personal_reads_are_not_exposed_to_foreign_or_opaque_browser_origins() {
        for path in ["/api/user-rag/documents","/api/privacy","/api/notes","/api/memory/export"] {
            for origin in ["https://foreign.example","null","http://localhost.evil.example"] {
                assert!(csrf_reject_response("GET",path,&header_map_origin(origin)).is_some(),"{path} {origin}");
            }
            assert!(csrf_reject_response("GET",path,&header_map_origin("http://tauri.localhost")).is_none());
            assert!(csrf_reject_response("GET",path,&HashMap::new()).is_none());
        }
    }

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
        assert_eq!(origin_allowed("null"),!cfg!(feature="consumer-beta"));
    }

    #[test]
    fn csrf_blocks_unknown_origin_for_post() {
        let m = header_map_origin("https://evil.example");
        assert!(csrf_reject_response("POST", "/api/message", &m).is_some());
    }

    #[test]
    fn csrf_blocks_localhost_prefix_spoof() {
        // e.g. http://localhost.evil.example should NOT be treated as local
        assert!(!origin_allowed("http://localhost.evil.example"));
        assert!(!origin_allowed("http://127.0.0.1.evil.example"));
        assert!(!origin_allowed("https://localhost-evil.example"));
    }

    #[test]
    fn csrf_get_policy_matches_distribution() {
        let m = header_map_origin("https://evil.example");
        assert_eq!(csrf_reject_response("GET", "/api/foo", &m).is_none(),!cfg!(feature="consumer-beta"));
    }

    #[test]
    fn csrf_no_origin_header_means_no_block() {
        let m = HashMap::new();
        assert!(csrf_reject_response("POST", "/api/foo", &m).is_none());
    }
}

#[cfg(test)]
mod delivery_origin_regressions {
    use super::*;
    #[test] fn authenticated_delivery_reads_reject_remote_and_opaque_origins() {
        for path in ["/api/studio/projects/id/delivery/status","/api/studio/projects/id/delivery/pr","/api/tasks/id/delivery","/api/pending-human-input","/api/permissions/queue","/api/events","/api/tasks/id/events","//api/tasks/id/delivery/","//api/studio/projects/id/delivery/status/","/api/permissions/queue/id"] {
            for origin in ["https://evil.example","null"] {assert!(csrf_reject_response("GET",path,&HashMap::from([("origin".into(),origin.into())])).is_some());}
            assert!(csrf_reject_response("GET",path,&HashMap::from([("origin".into(),"http://127.0.0.1:5178".into())])).is_none());
            assert!(csrf_reject_response("GET",path,&HashMap::from([("origin".into(),"http://localhost:5178".into()),("sec-fetch-site".into(),"cross-site".into())])).is_none());

        }
    }
}
