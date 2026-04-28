//! PTY terminal HTTP routes (`/api/terminal/*`).

use crate::api_http::json_response;
use crate::api_security::parse_query_param;

/// Returns `Some(response)` when this module handled the route.
pub async fn handle_terminal_routes(
    method: &str,
    path_only: &str,
    query_str: &str,
    body: Option<&[u8]>,
) -> Option<String> {
    if method == "GET" && path_only == "/api/terminal/capabilities" {
        let j = serde_json::json!({
            "interactive_pty": "available",
            "current": ["run_command", "run_terminal", "run_command_background", "process list|poll|kill"],
            "pty_api": {
                "create": "POST /api/terminal/pty/sessions",
                "list": "GET /api/terminal/pty/sessions",
                "input": "POST /api/terminal/pty/sessions/{id}/input",
                "output": "GET /api/terminal/pty/sessions/{id}/output?max=8192",
                "resize": "POST /api/terminal/pty/sessions/{id}/resize",
                "close": "DELETE /api/terminal/pty/sessions/{id}"
            },
            "spec": "spec/43_session_terminal.md"
        });
        return Some(json_response("200 OK", &j.to_string()));
    }

    if method == "GET" && path_only == "/api/terminal/pty/sessions" {
        let res =
            tokio::task::spawn_blocking(move || crate::terminal_pty::PtyManager::global().list_sessions())
                .await;
        return Some(match res {
            Ok(Ok(v)) => json_response(
                "200 OK",
                &serde_json::to_string(&v).unwrap_or_else(|_| "[]".to_string()),
            ),
            Ok(Err(e)) => json_response(
                "500 Internal Server Error",
                &serde_json::json!({"error":"pty_list_failed","detail": e.to_string()}).to_string(),
            ),
            Err(e) => json_response(
                "500 Internal Server Error",
                &serde_json::json!({"error":"pty_list_join","detail": e.to_string()}).to_string(),
            ),
        });
    }

    if method == "POST" && path_only == "/api/terminal/pty/sessions" {
        let Some(b) = body.as_deref() else {
            return Some(json_response("400 Bad Request", r#"{"error":"body_required"}"#));
        };
        let parsed: Result<crate::terminal_pty::PtyCreateBody, _> = serde_json::from_slice(b);
        let Ok(create_body) = parsed else {
            return Some(json_response(
                "400 Bad Request",
                &serde_json::json!({"error":"invalid_json"}).to_string(),
            ));
        };
        let res = tokio::task::spawn_blocking(move || {
            crate::terminal_pty::PtyManager::global().create(create_body)
        })
        .await;
        return Some(match res {
            Ok(Ok(r)) => json_response(
                "200 OK",
                &serde_json::to_string(&r).unwrap_or_else(|_| "{}".to_string()),
            ),
            Ok(Err(e)) => json_response(
                "500 Internal Server Error",
                &serde_json::json!({"error":"pty_create_failed","detail": e.to_string()}).to_string(),
            ),
            Err(e) => json_response(
                "500 Internal Server Error",
                &serde_json::json!({"error":"pty_create_join","detail": e.to_string()}).to_string(),
            ),
        });
    }

    if let Some(rest) = path_only.strip_prefix("/api/terminal/pty/sessions/") {
        let parts: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
        if parts.len() == 1 {
            let sid = parts[0];
            if method == "DELETE" {
                let sid = sid.to_string();
                let res = tokio::task::spawn_blocking(move || {
                    crate::terminal_pty::PtyManager::global().close(&sid)
                })
                .await;
                return Some(match res {
                    Ok(Ok(())) => json_response("200 OK", r#"{"ok":true}"#),
                    Ok(Err(e)) => {
                        let msg = e.to_string();
                        let (status, code) = if msg.contains("unknown session_id") {
                            ("404 Not Found", "pty_session_not_found")
                        } else {
                            ("500 Internal Server Error", "pty_close_failed")
                        };
                        json_response(
                            status,
                            &serde_json::json!({"error": code, "detail": msg}).to_string(),
                        )
                    }
                    Err(e) => json_response(
                        "500 Internal Server Error",
                        &serde_json::json!({"error":"pty_close_join","detail": e.to_string()}).to_string(),
                    ),
                });
            }
        }
        if parts.len() == 2 {
            let sid = parts[0].to_string();
            let action = parts[1];
            if action == "output" && method == "GET" {
                let max = parse_query_param(query_str, "max")
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(8192);
                let sid2 = sid.clone();
                let res = tokio::task::spawn_blocking(move || {
                    crate::terminal_pty::PtyManager::global().read_output(&sid2, max)
                })
                .await;
                return Some(match res {
                    Ok(Ok(o)) => json_response(
                        "200 OK",
                        &serde_json::to_string(&o).unwrap_or_else(|_| "{}".to_string()),
                    ),
                    Ok(Err(e)) => {
                        let msg = e.to_string();
                        let (status, code) = if msg.contains("unknown session_id") {
                            ("404 Not Found", "pty_session_not_found")
                        } else {
                            ("500 Internal Server Error", "pty_read_failed")
                        };
                        json_response(
                            status,
                            &serde_json::json!({"error": code, "detail": msg}).to_string(),
                        )
                    }
                    Err(e) => json_response(
                        "500 Internal Server Error",
                        &serde_json::json!({"error":"pty_read_join","detail": e.to_string()}).to_string(),
                    ),
                });
            }
            if action == "input" && method == "POST" {
                let Some(b) = body.as_deref() else {
                    return Some(json_response("400 Bad Request", r#"{"error":"body_required"}"#));
                };
                let parsed: Result<crate::terminal_pty::PtyInputBody, _> = serde_json::from_slice(b);
                if parsed.is_err() {
                    return Some(json_response(
                        "400 Bad Request",
                        r#"{"error":"invalid_json"}"#,
                    ));
                }
                let input_body = parsed.unwrap();
                let sid2 = sid.clone();
                let res = tokio::task::spawn_blocking(move || {
                    crate::terminal_pty::PtyManager::global().write_input(&sid2, input_body)
                })
                .await;
                return Some(match res {
                    Ok(Ok(())) => json_response("200 OK", r#"{"ok":true}"#),
                    Ok(Err(e)) => json_response(
                        "400 Bad Request",
                        &serde_json::json!({"error":"pty_write_failed","detail": e.to_string()}).to_string(),
                    ),
                    Err(e) => json_response(
                        "500 Internal Server Error",
                        &serde_json::json!({"error":"pty_write_join","detail": e.to_string()}).to_string(),
                    ),
                });
            }
            if action == "resize" && method == "POST" {
                let Some(b) = body.as_deref() else {
                    return Some(json_response("400 Bad Request", r#"{"error":"body_required"}"#));
                };
                let parsed: Result<crate::terminal_pty::PtyResizeBody, _> = serde_json::from_slice(b);
                if parsed.is_err() {
                    return Some(json_response(
                        "400 Bad Request",
                        r#"{"error":"invalid_json"}"#,
                    ));
                }
                let resize_body = parsed.unwrap();
                let sid2 = sid.clone();
                let res = tokio::task::spawn_blocking(move || {
                    crate::terminal_pty::PtyManager::global().resize(&sid2, resize_body)
                })
                .await;
                return Some(match res {
                    Ok(Ok(())) => json_response("200 OK", r#"{"ok":true}"#),
                    Ok(Err(e)) => json_response(
                        "400 Bad Request",
                        &serde_json::json!({"error":"pty_resize_failed","detail": e.to_string()}).to_string(),
                    ),
                    Err(e) => json_response(
                        "500 Internal Server Error",
                        &serde_json::json!({"error":"pty_resize_join","detail": e.to_string()}).to_string(),
                    ),
                });
            }
        }
    }

    None
}
