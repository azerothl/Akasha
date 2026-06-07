//! REST routes for user notes (`/api/notes/*`).

use crate::api_http::json_response;
use crate::notes::SharedNotesStore;
use base64::Engine;

fn parse_json(body: Option<&[u8]>) -> Option<serde_json::Value> {
    body.and_then(|b| serde_json::from_slice(b).ok())
}

fn parse_query(qs: &str, key: &str) -> Option<String> {
    qs.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        if k == key {
            Some(urlencoding::decode(v).map(|s| s.into_owned()).unwrap_or_else(|_| v.to_string()))
        } else {
            None
        }
    })
}

pub async fn try_handle(
    method: &str,
    path: &str,
    query_str: Option<&str>,
    body: Option<&[u8]>,
    notes_store: &SharedNotesStore,
) -> Option<String> {
    // GET /api/notes/search?q=
    if method == "GET" && path == "/api/notes/search" {
        let q = query_str
            .and_then(|qs| parse_query(qs, "q"))
            .unwrap_or_default();
        let limit = query_str
            .and_then(|qs| parse_query(qs, "limit"))
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(10);
        if q.trim().is_empty() {
            return Some(json_response("400 Bad Request", r#"{"error":"q_required"}"#));
        }
        let store = notes_store.lock().await;
        match store.search(&q, limit) {
            Ok(hits) => {
                let body = serde_json::json!({ "query": q, "hits": hits, "count": hits.len() });
                Some(json_response("200 OK", &body.to_string()))
            }
            Err(e) => Some(json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            )),
        }
    } else if method == "GET" && path == "/api/notes" {
        let store = notes_store.lock().await;
        match store.list() {
            Ok(notes) => {
                let body = serde_json::json!({ "notes": notes });
                Some(json_response("200 OK", &body.to_string()))
            }
            Err(e) => Some(json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            )),
        }
    } else if method == "POST" && path == "/api/notes" {
        let j = parse_json(body)?;
        let title = j.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let content = j
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let store = notes_store.lock().await;
        match store.create(&title, &content) {
            Ok(doc) => Some(json_response("200 OK", &serde_json::to_string(&doc).unwrap_or_default())),
            Err(e) => Some(json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            )),
        }
    } else if method == "GET" && path.starts_with("/api/notes/") && path.contains("/assets/") {
        let after_notes = path.trim_start_matches("/api/notes/");
        let (note_id, asset_part) = after_notes.split_once("/assets/")?;
        if note_id.is_empty() || asset_part.is_empty() || note_id.contains('/') {
            return None;
        }
        let filename = asset_part.split('?').next().unwrap_or("").trim();
        if filename.is_empty() {
            return Some(json_response("400 Bad Request", r#"{"error":"filename_required"}"#));
        }
        let store = notes_store.lock().await;
        match store.read_asset(note_id, filename) {
            Ok(Some((bytes, mime))) => {
                let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                let body = serde_json::json!({
                    "filename": filename,
                    "mime_type": mime,
                    "content_base64": b64,
                });
                Some(json_response("200 OK", &body.to_string()))
            }
            Ok(None) => Some(json_response("404 Not Found", r#"{"error":"asset_not_found"}"#)),
            Err(e) => Some(json_response(
                "400 Bad Request",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            )),
        }
    } else if method == "GET" && path.starts_with("/api/notes/") {
        let rest = path.trim_start_matches("/api/notes/").trim();
        if rest.is_empty() || rest.contains('/') {
            return None;
        }
        let store = notes_store.lock().await;
        match store.read(rest) {
            Ok(Some(doc)) => Some(json_response("200 OK", &serde_json::to_string(&doc).unwrap_or_default())),
            Ok(None) => Some(json_response("404 Not Found", r#"{"error":"note_not_found"}"#)),
            Err(e) => Some(json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            )),
        }
    } else if method == "PUT" && path.starts_with("/api/notes/") {
        let rest = path.trim_start_matches("/api/notes/").trim();
        if rest.is_empty() || rest.contains('/') {
            return None;
        }
        let j = parse_json(body)?;
        let title = j.get("title").and_then(|v| v.as_str());
        let content = j.get("content").and_then(|v| v.as_str());
        let store = notes_store.lock().await;
        match store.update(rest, title, content) {
            Ok(Some(doc)) => Some(json_response("200 OK", &serde_json::to_string(&doc).unwrap_or_default())),
            Ok(None) => Some(json_response("404 Not Found", r#"{"error":"note_not_found"}"#)),
            Err(e) => Some(json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            )),
        }
    } else if method == "DELETE" && path.starts_with("/api/notes/") {
        let rest = path.trim_start_matches("/api/notes/").trim();
        if rest.is_empty() || rest.contains('/') {
            return None;
        }
        let store = notes_store.lock().await;
        match store.delete(rest) {
            Ok(true) => Some(json_response("200 OK", r#"{"ok":true}"#)),
            Ok(false) => Some(json_response("404 Not Found", r#"{"error":"note_not_found"}"#)),
            Err(e) => Some(json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            )),
        }
    } else if method == "POST" && path.starts_with("/api/notes/") && path.ends_with("/assets") {
        let inner = path
            .trim_start_matches("/api/notes/")
            .trim_end_matches("/assets")
            .trim_end_matches('/');
        if inner.is_empty() || inner.contains('/') {
            return None;
        }
        let j = parse_json(body)?;
        let filename = j
            .get("filename")
            .or_else(|| j.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let content_base64 = j
            .get("content_base64")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if filename.is_empty() || content_base64.is_empty() {
            return Some(json_response(
                "400 Bad Request",
                r#"{"error":"filename_and_content_base64_required"}"#,
            ));
        }
        let bytes = match base64::engine::general_purpose::STANDARD.decode(content_base64) {
            Ok(b) => b,
            Err(e) => {
                return Some(json_response(
                    "400 Bad Request",
                    &serde_json::json!({ "error": format!("invalid_base64: {e}") }).to_string(),
                ))
            }
        };
        let store = notes_store.lock().await;
        match store.add_asset(inner, filename, &bytes) {
            Ok(rel_path) => {
                let body = serde_json::json!({ "path": rel_path, "url": format!("/api/notes/{inner}/assets/{}", rel_path.trim_start_matches("assets/")) });
                Some(json_response("200 OK", &body.to_string()))
            }
            Err(e) => Some(json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            )),
        }
    } else {
        None
    }
}
