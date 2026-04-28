//! HTTP handlers for multi-workspace project knowledge graphs.

use crate::api_http::json_response;
use std::path::{Path, PathBuf};

mod handlers;

use handlers::{
    delete_workspace, get_export_workspace, get_graph_json_workspace, get_html_workspace,
    get_legacy_config, get_legacy_first_export, get_legacy_first_html, get_legacy_first_report,
    get_legacy_summary, get_report_workspace, get_workspaces_list, post_create_workspace,
    post_legacy_rebuild, post_rebuild_workspace, put_legacy_config,
};


#[derive(Debug)]
enum WsTail {
    Rebuild,
    Export,
    Report,
    Html,
    GraphJson,
}

#[derive(Debug)]
enum WsPath {
    Collection,
    Resource {
        id: String,
        tail: Option<WsTail>,
    },
}

fn parse_workspace_path(path_only: &str) -> Option<WsPath> {
    const PREFIX: &str = "/api/workspace-graph/workspaces";
    if path_only == PREFIX {
        return Some(WsPath::Collection);
    }
    let rest = path_only.strip_prefix(&(PREFIX.to_string() + "/"))?;
    if rest.is_empty() {
        return None;
    }
    let parts: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    let id = parts.first()?.to_string();
    if parts.len() == 1 {
        return Some(WsPath::Resource { id, tail: None });
    }
    let tail = match parts[1] {
        "rebuild" => WsTail::Rebuild,
        "export" => WsTail::Export,
        "report" => WsTail::Report,
        "html" => WsTail::Html,
        "graph.json" => WsTail::GraphJson,
        _ => return None,
    };
    Some(WsPath::Resource {
        id,
        tail: Some(tail),
    })
}

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

    // New multi-workspace API
    if let Some(ws) = parse_workspace_path(path_only) {
        return Some(match ws {
            WsPath::Collection => match method {
                "GET" => get_workspaces_list(data_dir, store_path),
                "POST" => match body {
                    Some(b) => post_create_workspace(data_dir, store_path, b).await,
                    None => json_response("400 Bad Request", r#"{"error":"body_required"}"#),
                },
                _ => json_response("405 Method Not Allowed", r#"{"error":"method_not_allowed"}"#),
            },
            WsPath::Resource { id, tail } => match (method, tail) {
                ("DELETE", None) => delete_workspace(data_dir, store_path, &id),
                ("POST", Some(WsTail::Rebuild)) => {
                    post_rebuild_workspace(data_dir, store_path, &id, body).await
                }
                ("GET", Some(WsTail::Export)) => get_export_workspace(store_path, &id),
                ("GET", Some(WsTail::Report)) => get_report_workspace(data_dir, &id),
                ("GET", Some(WsTail::Html)) => get_html_workspace(data_dir, &id),
                ("GET", Some(WsTail::GraphJson)) => get_graph_json_workspace(data_dir, &id),
                _ => json_response("405 Method Not Allowed", r#"{"error":"method_not_allowed"}"#),
            },
        });
    }

    // Deprecated single-workspace shims (minimal)
    match (method, path_only) {
        ("GET", "/api/workspace-graph") => Some(get_legacy_summary(data_dir, store_path)),
        ("GET", "/api/workspace-graph/config") => Some(get_legacy_config(data_dir)),
        ("PUT", "/api/workspace-graph/config") => match body {
            Some(b) => Some(put_legacy_config(data_dir, store_path, b).await),
            None => Some(json_response(
                "400 Bad Request",
                r#"{"error":"body_required"}"#,
            )),
        },
        ("POST", "/api/workspace-graph/rebuild") => {
            let root = body.and_then(|b| {
                serde_json::from_slice::<serde_json::Value>(b)
                    .ok()
                    .and_then(|v| v.get("root").and_then(|x| x.as_str()).map(PathBuf::from))
            });
            Some(post_legacy_rebuild(data_dir, store_path, root).await)
        }
        ("GET", "/api/workspace-graph/export") => Some(get_legacy_first_export(store_path)),
        ("GET", "/api/workspace-graph/report") => Some(get_legacy_first_report(data_dir, store_path)),
        ("GET", "/api/workspace-graph/html") => Some(get_legacy_first_html(data_dir, store_path)),
        _ if path_only.starts_with("/api/workspace-graph") => Some(json_response(
            "404 Not Found",
            r#"{"error":"not_found"}"#,
        )),
        _ => None,
    }
}

