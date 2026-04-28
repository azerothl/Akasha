//! Workspace graph store and legacy handlers (split from api_workspace_graph).

use crate::api_http::json_response;
use akasha_store::WorkspaceGraphStore;
use akasha_workspace_graph::WorkspaceGraphConfig;
use std::path::{Path, PathBuf};

pub(super) fn remove_workspace_artifacts(data_dir: &Path, workspace_id: &str) {
    let dir = data_dir
        .join("workspace_graph")
        .join("out")
        .join(workspace_id);
    if dir.is_dir() {
        let _ = std::fs::remove_dir_all(&dir);
    }
}

pub(super) fn maybe_import_legacy_yaml(data_dir: &Path, store: &WorkspaceGraphStore) -> anyhow::Result<()> {
    if !store.list_workspaces()?.is_empty() {
        return Ok(());
    }
    let cfg = WorkspaceGraphConfig::load(data_dir)?;
    let Some(root) = cfg.root.filter(|s| !s.trim().is_empty()) else {
        return Ok(());
    };
    let p = Path::new(root.trim());
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        data_dir.join(p)
    };
    let canon = match abs.canonicalize() {
        Ok(c) => c,
        Err(_) => return Ok(()),
    };
    if !canon.is_dir() {
        return Ok(());
    }
    store.create_workspace("Default", &canon.to_string_lossy())?;
    let _ = std::fs::remove_file(WorkspaceGraphConfig::path(data_dir));
    Ok(())
}
pub(super) fn open_store(store_path: &Path) -> Result<WorkspaceGraphStore, String> {
    WorkspaceGraphStore::open(store_path).map_err(|e| e.to_string())
}

pub(super) fn get_workspaces_list(data_dir: &Path, store_path: &Path) -> String {
    let store = match open_store(store_path) {
        Ok(s) => s,
        Err(e) => {
            return json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e }).to_string(),
            );
        }
    };
    if let Err(e) = maybe_import_legacy_yaml(data_dir, &store) {
        tracing::warn!(error = %e, "workspace-graph legacy yaml import skipped");
    }
    let list = match store.list_workspaces() {
        Ok(l) => l,
        Err(e) => {
            return json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            );
        }
    };
    let mut rows = Vec::new();
    for w in list {
        let (n, e) = store.stats(&w.id).unwrap_or((0, 0));
        let build = store.get_build_info(&w.id).ok().flatten();
        let file_count = build.as_ref().map(|b| b.file_count).unwrap_or(0);
        let indexed_root = build.as_ref().map(|b| b.root_path.clone());
        let built_at = build.as_ref().map(|b| b.built_at.clone());
        rows.push(serde_json::json!({
            "id": w.id,
            "name": w.name,
            "root_path": w.root_path,
            "created_at": w.created_at,
            "node_count": n,
            "edge_count": e,
            "built_at": built_at,
            "file_count": file_count,
            "indexed_root": indexed_root,
        }));
    }
    let (tn, te) = store.stats_all().unwrap_or((0, 0));
    json_response(
        "200 OK",
        &serde_json::json!({
            "workspaces": rows,
            "total_node_count": tn,
            "total_edge_count": te,
            "out_base": data_dir.join("workspace_graph").join("out").to_string_lossy(),
        })
        .to_string(),
    )
}

pub(super) async fn post_create_workspace(data_dir: &Path, store_path: &Path, body: &[u8]) -> String {
    let v: serde_json::Value = match serde_json::from_slice(body) {
        Ok(x) => x,
        Err(e) => {
            return json_response(
                "400 Bad Request",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            );
        }
    };
    let name = v
        .get("name")
        .and_then(|x| x.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    let Some(name) = name else {
        return json_response("400 Bad Request", r#"{"error":"name_required"}"#);
    };
    let root_path = v
        .get("root_path")
        .and_then(|x| x.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    let Some(root_raw) = root_path else {
        return json_response("400 Bad Request", r#"{"error":"root_path_required"}"#);
    };
    let p = Path::new(root_raw);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        data_dir.join(p)
    };
    let canon = match abs.canonicalize() {
        Ok(c) if c.is_dir() => c,
        Ok(c) => {
            return json_response(
                "400 Bad Request",
                &serde_json::json!({ "error": format!("not a directory: {}", c.display()) })
                    .to_string(),
            );
        }
        Err(e) => {
            return json_response(
                "400 Bad Request",
                &serde_json::json!({ "error": format!("path: {}", e) }).to_string(),
            );
        }
    };
    let root_s = canon.to_string_lossy().to_string();

    let rebuild = v.get("rebuild").and_then(|x| x.as_bool()).unwrap_or(true);

    let data_dir = data_dir.to_path_buf();
    let store_path = store_path.to_path_buf();
    let name = name.to_string();

    let res = tokio::task::spawn_blocking(move || {
        let store = WorkspaceGraphStore::open(&store_path)?;
        if store.workspace_name_exists(&name, None)? {
            return Err(anyhow::anyhow!("workspace name already exists"));
        }
        let id = store.create_workspace(&name, &root_s)?;
        let stats = if rebuild {
            Some(akasha_workspace_graph::rebuild(&data_dir, &store, &id, None)?)
        } else {
            None
        };
        Ok((id, stats))
    })
    .await;

    match res {
        Ok(Ok((id, stats))) => {
            let body = if let Some(s) = stats {
                serde_json::json!({
                    "ok": true,
                    "id": id,
                    "workspace_id": s.workspace_id,
                    "root": s.root_display,
                    "files_indexed": s.files_indexed,
                    "nodes": s.nodes,
                    "edges": s.edges,
                })
            } else {
                serde_json::json!({ "ok": true, "id": id })
            };
            json_response("201 Created", &body.to_string())
        }
        Ok(Err(e)) => json_response(
            "400 Bad Request",
            &serde_json::json!({ "error": e.to_string() }).to_string(),
        ),
        Err(_) => json_response(
            "500 Internal Server Error",
            r#"{"error":"create_join_failed"}"#,
        ),
    }
}

pub(super) fn delete_workspace(data_dir: &Path, store_path: &Path, id: &str) -> String {
    remove_workspace_artifacts(data_dir, id);
    match open_store(store_path) {
        Ok(store) => match store.delete_workspace(id) {
            Ok(true) => json_response("200 OK", r#"{"ok":true}"#),
            Ok(false) => json_response("404 Not Found", r#"{"error":"not_found"}"#),
            Err(e) => json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            ),
        },
        Err(e) => json_response(
            "500 Internal Server Error",
            &serde_json::json!({ "error": e }).to_string(),
        ),
    }
}

pub(super) async fn post_rebuild_workspace(
    data_dir: &Path,
    store_path: &Path,
    id: &str,
    body: Option<&[u8]>,
) -> String {
    let root_override = body.and_then(|b| {
        serde_json::from_slice::<serde_json::Value>(b)
            .ok()
            .and_then(|v| {
                v.get("root_path")
                    .and_then(|x| x.as_str())
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .map(PathBuf::from)
            })
    });

    let id = id.to_string();
    let data_dir = data_dir.to_path_buf();
    let store_path = store_path.to_path_buf();

    let res = tokio::task::spawn_blocking(move || {
        let store = WorkspaceGraphStore::open(&store_path)?;
        if let Some(ref p) = root_override {
            let abs = if p.is_absolute() {
                p.clone()
            } else {
                data_dir.join(p)
            };
            let canon = abs
                .canonicalize()
                .map_err(|e| anyhow::anyhow!("root_path: {}", e))?;
            if !canon.is_dir() {
                return Err(anyhow::anyhow!("root_path is not a directory"));
            }
            store.update_workspace_root(&id, &canon.to_string_lossy())?;
        }
        akasha_workspace_graph::rebuild(&data_dir, &store, &id, None)
    })
    .await;

    let res = match res {
        Ok(r) => r,
        Err(_) => {
            return json_response(
                "500 Internal Server Error",
                r#"{"error":"rebuild_join_failed"}"#,
            );
        }
    };

    match res {
        Ok(stats) => {
            tracing::info!(
                workspace_id = %stats.workspace_id,
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
                    "workspace_id": stats.workspace_id,
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

pub(super) fn get_export_workspace(store_path: &Path, id: &str) -> String {
    let store = match open_store(store_path) {
        Ok(s) => s,
        Err(e) => {
            return json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e }).to_string(),
            );
        }
    };
    let ws = match store.get_workspace(id) {
        Ok(Some(w)) => w,
        Ok(None) => {
            return json_response("404 Not Found", r#"{"error":"not_found"}"#);
        }
        Err(e) => {
            return json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            );
        }
    };
    let build = match store.get_build_info(id) {
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
    let (nodes, edges) = match store.export_graph(id) {
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
            "schema": "akasha-workspace-graph/v2",
            "workspace_id": id,
            "workspace_name": ws.name,
            "root_path": b.root_path,
            "built_at": b.built_at,
            "file_count": b.file_count,
            "nodes": nodes,
            "edges": edges,
        }))
        .unwrap_or_default(),
    )
}

pub(super) fn get_report_workspace(data_dir: &Path, id: &str) -> String {
    let p = data_dir
        .join("workspace_graph")
        .join("out")
        .join(id)
        .join("GRAPH_REPORT.md");
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

pub(super) fn get_html_workspace(data_dir: &Path, id: &str) -> String {
    let p = data_dir
        .join("workspace_graph")
        .join("out")
        .join(id)
        .join("graph.html");
    match std::fs::read_to_string(&p) {
        Ok(html) => http_html_response(&html),
        Err(_) => json_response(
            "404 Not Found",
            r#"{"error":"graph_html_not_found"}"#,
        ),
    }
}

pub(super) fn get_graph_json_workspace(data_dir: &Path, id: &str) -> String {
    let p = data_dir
        .join("workspace_graph")
        .join("out")
        .join(id)
        .join("graph.json");
    match std::fs::read_to_string(&p) {
        Ok(json) => format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n{}",
            json.as_bytes().len(),
            json
        ),
        Err(_) => json_response(
            "404 Not Found",
            r#"{"error":"graph_json_not_found"}"#,
        ),
    }
}

// --- Legacy (first workspace or yaml) ---

pub(super) fn get_legacy_summary(data_dir: &Path, store_path: &Path) -> String {
    let store = match open_store(store_path) {
        Ok(s) => s,
        Err(e) => {
            return json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e }).to_string(),
            );
        }
    };
    let _ = maybe_import_legacy_yaml(data_dir, &store);
    let list = store.list_workspaces().unwrap_or_default();
    let first = list.first();
    let (n, e) = first
        .map(|w| store.stats(&w.id).unwrap_or((0, 0)))
        .unwrap_or((0, 0));
    let build = first.and_then(|w| store.get_build_info(&w.id).ok().flatten());
    json_response(
        "200 OK",
        &serde_json::json!({
            "deprecated": true,
            "use": "/api/workspace-graph/workspaces",
            "configured": first.is_some(),
            "root": first.map(|w| w.root_path.clone()),
            "indexed_root": build.as_ref().map(|b| b.root_path.clone()),
            "built_at": build.as_ref().map(|b| b.built_at.clone()),
            "file_count": build.map(|b| b.file_count).unwrap_or(0),
            "node_count": n,
            "edge_count": e,
            "workspace_id": first.map(|w| w.id.clone()),
            "out_dir": data_dir.join("workspace_graph").join("out").to_string_lossy(),
        })
        .to_string(),
    )
}

pub(super) fn get_legacy_config(data_dir: &Path) -> String {
    match WorkspaceGraphConfig::load(data_dir) {
        Ok(c) => json_response(
            "200 OK",
            &serde_json::json!({ "root": c.root, "deprecated": true }).to_string(),
        ),
        Err(e) => json_response(
            "500 Internal Server Error",
            &serde_json::json!({ "error": e.to_string() }).to_string(),
        ),
    }
}

pub(super) async fn put_legacy_config(data_dir: &Path, _store_path: &Path, body: &[u8]) -> String {
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
        &serde_json::json!({ "ok": true, "root": cfg.root, "deprecated": true }).to_string(),
    )
}

pub(super) async fn post_legacy_rebuild(data_dir: &Path, store_path: &Path, root: Option<PathBuf>) -> String {
    let data_dir = data_dir.to_path_buf();
    let store_path = store_path.to_path_buf();
    let res = tokio::task::spawn_blocking(move || {
        let store = WorkspaceGraphStore::open(&store_path)?;
        let _ = maybe_import_legacy_yaml(&data_dir, &store);
        let list = store.list_workspaces()?;
        let id = if let Some(w) = list.first() {
            w.id.clone()
        } else {
            let cfg = WorkspaceGraphConfig::load(&data_dir)?;
            let r = root
                .as_ref()
                .map(|p| p.canonicalize())
                .transpose()
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            let rp = if let Some(c) = r {
                c.to_string_lossy().to_string()
            } else if let Some(ref s) = cfg.root {
                let p = Path::new(s.trim());
                let abs = if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    data_dir.join(p)
                };
                abs.canonicalize()
                    .map_err(|e| anyhow::anyhow!("{}", e))?
                    .to_string_lossy()
                    .to_string()
            } else {
                return Err(anyhow::anyhow!("no workspace; create via POST /workspaces or set root"));
            };
            store.create_workspace("Default", &rp)?
        };
        akasha_workspace_graph::rebuild(&data_dir, &store, &id, root.clone())
    })
    .await;

    let res = match res {
        Ok(r) => r,
        Err(_) => {
            return json_response(
                "500 Internal Server Error",
                r#"{"error":"rebuild_join_failed"}"#,
            );
        }
    };

    match res {
        Ok(stats) => json_response(
            "200 OK",
            &serde_json::to_string(&serde_json::json!({
                "ok": true,
                "workspace_id": stats.workspace_id,
                "root": stats.root_display,
                "files_indexed": stats.files_indexed,
                "nodes": stats.nodes,
                "edges": stats.edges,
                "deprecated": true,
            }))
            .unwrap_or_else(|_| r#"{"ok":true}"#.to_string()),
        ),
        Err(e) => json_response(
            "400 Bad Request",
            &serde_json::json!({ "error": e.to_string() }).to_string(),
        ),
    }
}

pub(super) fn get_legacy_first_export(store_path: &Path) -> String {
    let store = match open_store(store_path) {
        Ok(s) => s,
        Err(e) => {
            return json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e }).to_string(),
            );
        }
    };
    let Some(w) = store.list_workspaces().ok().and_then(|l| l.into_iter().next()) else {
        return json_response("404 Not Found", r#"{"error":"no_workspace"}"#);
    };
    get_export_workspace(store_path, &w.id)
}

pub(super) fn get_legacy_first_report(data_dir: &Path, store_path: &Path) -> String {
    let store = match open_store(store_path) {
        Ok(s) => s,
        Err(e) => {
            return json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e }).to_string(),
            );
        }
    };
    let Some(w) = store.list_workspaces().ok().and_then(|l| l.into_iter().next()) else {
        return json_response("404 Not Found", r#"{"error":"no_workspace"}"#);
    };
    get_report_workspace(data_dir, &w.id)
}

pub(super) fn get_legacy_first_html(data_dir: &Path, store_path: &Path) -> String {
    let store = match open_store(store_path) {
        Ok(s) => s,
        Err(e) => {
            return json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e }).to_string(),
            );
        }
    };
    let Some(w) = store.list_workspaces().ok().and_then(|l| l.into_iter().next()) else {
        return json_response("404 Not Found", r#"{"error":"no_workspace"}"#);
    };
    get_html_workspace(data_dir, &w.id)
}

pub(super) fn http_html_response(body: &str) -> String {
    let bytes = body.as_bytes();
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n{}",
        bytes.len(),
        body
    )
}
