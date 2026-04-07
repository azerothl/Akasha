//! HTTP handlers for workspace knowledge graph (Graphify-style indexing).
//!
//! External Python tool [graphify](https://github.com/safishamsi/graphify) is **not** bundled;
//! use it separately if you need its full multimodal pipeline; Akasha ships a native Rust indexer.

use crate::api::json_response;
use akasha_store::WorkspaceGraphStore;
use akasha_workspace_graph::WorkspaceGraphConfig;
use std::path::{Path, PathBuf};

/// Handle `/api/workspace-graph` routes. Returns `None` if the path does not match.
pub async fn handle_workspace_graph(
    method: &str,
    path_only: &str,
    body: Option<&[u8]>,
    data_dir: &Path,
    store_path: &Path,
) -> Option<String> {
    if !path_only.starts_with("/api/workspace-graph") {
        return None;
    }

    match (method, path_only) {
        ("GET", "/api/workspace-graph") => Some(get_status(data_dir, store_path)),
        ("GET", "/api/workspace-graph/config") => Some(get_config(data_dir)),
        ("PUT", "/api/workspace-graph/config") => {
            let Some(b) = body else {
                return Some(json_response(
                    "400 Bad Request",
                    r#"{"error":"body_required"}"#,
                ));
            };
            Some(put_config(data_dir, b))
        }
        ("POST", "/api/workspace-graph/rebuild") => {
            let root = body.and_then(|b| {
                serde_json::from_slice::<serde_json::Value>(b)
                    .ok()
                    .and_then(|v| v.get("root").and_then(|x| x.as_str()).map(PathBuf::from))
            });
            Some(post_rebuild(data_dir, store_path, root).await)
        }
        ("GET", "/api/workspace-graph/export") => Some(get_export(store_path)),
        ("GET", "/api/workspace-graph/report") => Some(get_report(data_dir)),
        ("GET", "/api/workspace-graph/html") => Some(get_html(data_dir)),
        _ => Some(json_response(
            "405 Method Not Allowed",
            r#"{"error":"method_not_allowed"}"#,
        )),
    }
}

fn get_status(data_dir: &Path, store_path: &Path) -> String {
    let cfg = match WorkspaceGraphConfig::load(data_dir) {
        Ok(c) => c,
        Err(e) => {
            return json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e.to_string(), "yaml_broken": true }).to_string(),
            );
        }
    };
    let root_config = cfg.root.clone();
    let store = match WorkspaceGraphStore::open(store_path) {
        Ok(s) => s,
        Err(e) => {
            return json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            );
        }
    };
    let (n, e) = store.stats().unwrap_or((0, 0));
    let build = store.get_build_info().ok().flatten();
    json_response(
        "200 OK",
        &serde_json::json!({
            "configured": root_config.is_some(),
            "root": root_config,
            "indexed_root": build.as_ref().map(|b| b.root_path.clone()),
            "built_at": build.as_ref().map(|b| b.built_at.clone()),
            "file_count": build.map(|b| b.file_count).unwrap_or(0),
            "node_count": n,
            "edge_count": e,
            "out_dir": data_dir.join("workspace_graph").join("out").to_string_lossy(),
        })
        .to_string(),
    )
}

fn get_config(data_dir: &Path) -> String {
    let cfg = match WorkspaceGraphConfig::load(data_dir) {
        Ok(c) => c,
        Err(e) => {
            return json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            );
        }
    };
    json_response(
        "200 OK",
        &serde_json::json!({ "root": cfg.root }).to_string(),
    )
}

fn put_config(data_dir: &Path, body: &[u8]) -> String {
    let v: serde_json::Value = match serde_json::from_slice(body) {
        Ok(x) => x,
        Err(e) => {
            return json_response(
                "400 Bad Request",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            );
        }
    };
    let root = v
        .get("root")
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let cfg = WorkspaceGraphConfig { root };
    if let Err(e) = cfg.save(data_dir) {
        return json_response(
            "500 Internal Server Error",
            &serde_json::json!({ "error": e.to_string() }).to_string(),
        );
    }
    json_response(
        "200 OK",
        &serde_json::json!({ "ok": true, "root": cfg.root }).to_string(),
    )
}

async fn post_rebuild(data_dir: &Path, store_path: &Path, root_override: Option<PathBuf>) -> String {
    let data_dir = data_dir.to_path_buf();
    let store_path = store_path.to_path_buf();
    let res = tokio::task::spawn_blocking(move || {
        let store = WorkspaceGraphStore::open(&store_path)?;
        akasha_workspace_graph::rebuild(&data_dir, &store, root_override)
    })
    .await;

    let res = match res {
        Ok(r) => r,
        Err(_) => {
            tracing::error!("workspace-graph rebuild: spawn_blocking join failed");
            return json_response(
                "500 Internal Server Error",
                r#"{"error":"rebuild_join_failed"}"#,
            );
        }
    };

    match res {
        Ok(stats) => {
            tracing::info!(
                root = %stats.root_display,
                files = stats.files_indexed,
                nodes = stats.nodes,
                edges = stats.edges,
                "workspace-graph rebuild ok"
            );
            json_response(
                "200 OK",
                &serde_json::to_string(&serde_json::json!({
                    "ok": true,
                    "root": stats.root_display,
                    "files_indexed": stats.files_indexed,
                    "nodes": stats.nodes,
                    "edges": stats.edges,
                }))
                .unwrap_or_else(|_| r#"{"ok":true}"#.to_string()),
            )
        }
        Err(e) => {
            tracing::warn!(error = %e, "workspace-graph rebuild failed");
            json_response(
                "400 Bad Request",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            )
        }
    }
}

fn get_export(store_path: &Path) -> String {
    let store = match WorkspaceGraphStore::open(store_path) {
        Ok(s) => s,
        Err(e) => {
            return json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            );
        }
    };
    let build = match store.get_build_info() {
        Ok(b) => b,
        Err(e) => {
            return json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            );
        }
    };
    let Some(b) = build else {
        return json_response(
            "404 Not Found",
            r#"{"error":"no_graph_build"}"#,
        );
    };
    let (nodes, edges) = match store.export_graph() {
        Ok(x) => x,
        Err(e) => {
            return json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            );
        }
    };
    json_response(
        "200 OK",
        &serde_json::to_string(&serde_json::json!({
            "schema": "akasha-workspace-graph/v1",
            "root_path": b.root_path,
            "built_at": b.built_at,
            "file_count": b.file_count,
            "nodes": nodes,
            "edges": edges,
        }))
        .unwrap_or_default(),
    )
}

fn get_report(data_dir: &Path) -> String {
    let p = data_dir.join("workspace_graph").join("out").join("GRAPH_REPORT.md");
    match std::fs::read_to_string(&p) {
        Ok(md) => json_response(
            "200 OK",
            &serde_json::json!({ "markdown": md }).to_string(),
        ),
        Err(_) => json_response(
            "404 Not Found",
            r#"{"error":"report_not_found"}"#,
        ),
    }
}

fn get_html(data_dir: &Path) -> String {
    let p = data_dir.join("workspace_graph").join("out").join("graph.html");
    match std::fs::read_to_string(&p) {
        Ok(html) => http_html_response(&html),
        Err(_) => json_response(
            "404 Not Found",
            r#"{"error":"graph_html_not_found"}"#,
        ),
    }
}

fn http_html_response(body: &str) -> String {
    let bytes = body.as_bytes();
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n{}",
        bytes.len(),
        body
    )
}
