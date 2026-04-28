//! MCP operator + runtime HTTP routes and lifecycle hooks summary.

use crate::api_http::json_response;
use std::path::Path;

/// Returns `Some(response)` when this module handled the route.
pub async fn handle_mcp_lifecycle_routes(
    method: &str,
    path_only: &str,
    body: Option<&[u8]>,
    data_dir: &Path,
) -> Option<String> {
    if method == "GET" && path_only == "/api/mcp/status" {
        if let Some(cached) = crate::http_get_cache::cache_get_mcp_status() {
            return Some(json_response("200 OK", &cached));
        }
        let j = crate::mcp::mcp_operator_status(data_dir);
        let body = j.to_string();
        crate::http_get_cache::cache_put_mcp_status(&body);
        return Some(json_response("200 OK", &body));
    }

    if method == "GET" && path_only == "/api/mcp/runtime" {
        let j = crate::mcp_runtime::summary().await;
        return Some(json_response("200 OK", &j.to_string()));
    }
    if method == "GET" && path_only == "/api/mcp/runtime/sse" {
        let summary = crate::mcp_runtime::summary().await.to_string();
        return Some(format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\nAccess-Control-Allow-Origin: *\r\n\r\nevent: runtime\n\
data: {}\n\n",
            summary.replace('\n', "").replace('\r', "")
        ));
    }
    if method == "POST" && path_only == "/api/mcp/runtime/stdio/start" {
        let Some(b) = body.as_deref() else {
            return Some(json_response("400 Bad Request", r#"{"error":"body_required"}"#));
        };
        let v: serde_json::Value = match serde_json::from_slice(b) {
            Ok(v) => v,
            Err(_) => return Some(json_response("400 Bad Request", r#"{"error":"invalid_json"}"#)),
        };
        let server = v
            .get("server")
            .and_then(|x| x.as_str())
            .map(str::trim)
            .unwrap_or("");
        if server.is_empty() {
            return Some(json_response(
                "400 Bad Request",
                r#"{"error":"missing_server","hint":"{\"server\":\"myMcpKey\"}"}"#,
            ));
        }
        match crate::mcp_runtime::start_stdio_server(data_dir, server).await {
            Ok(j) => return Some(json_response("200 OK", &j.to_string())),
            Err(e) => {
                return Some(json_response(
                    "500 Internal Server Error",
                    &serde_json::json!({"error":"mcp_stdio_start_failed","detail": e}).to_string(),
                ));
            }
        }
    }
    if method == "POST" && path_only == "/api/mcp/runtime/stdio/stop" {
        let j = crate::mcp_runtime::stop_stdio_server().await;
        return Some(json_response("200 OK", &j.to_string()));
    }
    if method == "GET" && path_only == "/api/mcp/runtime/oauth" {
        let j = crate::mcp_runtime::oauth_get(data_dir).await;
        return Some(json_response("200 OK", &j.to_string()));
    }
    if method == "POST" && path_only == "/api/mcp/runtime/oauth" {
        let Some(b) = body.as_deref() else {
            return Some(json_response("400 Bad Request", r#"{"error":"body_required"}"#));
        };
        let v: serde_json::Value = match serde_json::from_slice(b) {
            Ok(v) => v,
            Err(_) => return Some(json_response("400 Bad Request", r#"{"error":"invalid_json"}"#)),
        };
        let provider = v
            .get("provider")
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("unknown")
            .to_string();
        let status = v
            .get("status")
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("configured")
            .to_string();
        match crate::mcp_runtime::oauth_put(data_dir, provider, status).await {
            Ok(j) => return Some(json_response("200 OK", &j.to_string())),
            Err(e) => {
                return Some(json_response(
                    "500 Internal Server Error",
                    &serde_json::json!({"error":"mcp_oauth_state_write_failed","detail":e}).to_string(),
                ));
            }
        }
    }

    if method == "GET" && path_only == "/api/lifecycle/hooks" {
        let j = crate::lifecycle_hooks::lifecycle_hooks_summary(data_dir);
        return Some(json_response("200 OK", &j.to_string()));
    }

    None
}
