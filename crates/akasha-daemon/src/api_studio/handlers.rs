//! Dispatch for /api/studio/* routes.

use super::*;

/// Returns HTTP response if handled.
pub async fn handle_studio_route(
    method: &str,
    path_only: &str,
    query_str: &str,
    body: Option<&[u8]>,
    data_dir: &Path,
) -> Option<String> {
    let base = studio_projects_base(data_dir);
    let _ = fs::create_dir_all(&base);

    if method == "GET" && path_only == "/api/studio/projects" {
        let dirs = list_project_dirs(&base);
        let projects: Vec<ProjectMetaOut> = dirs
            .iter()
            .filter_map(|dir_path| {
                dir_path.file_name().and_then(|n| n.to_str()).map(|id| {
                    let name = load_studio_meta(dir_path)
                        .map(|m| m.name.trim().to_string())
                        .filter(|n| !n.is_empty())
                        .unwrap_or_else(|| {
                            let short = id.chars().take(8).collect::<String>();
                            format!("Projet {short}")
                        });
                    ProjectMetaOut {
                        id: id.to_string(),
                        name,
                        path: dir_path.display().to_string(),
                    }
                })
            })
            .collect();
        let body = serde_json::to_string(&ProjectsListOut { projects }).unwrap_or_else(|_| "{}".to_string());
        return Some(json_response("200 OK", &body));
    }

    if method == "POST" && path_only == "/api/studio/projects" {
        let id = uuid::Uuid::new_v4().to_string();
        let dir = match resolve_studio_project_dir(data_dir, &id) {
            Ok(d) => d,
            Err(e) => {
                return Some(json_response(
                    "400 Bad Request",
                    &serde_json::json!({ "error": e }).to_string(),
                ));
            }
        };
        if let Err(e) = fs::create_dir_all(&dir) {
            return Some(json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            ));
        }
        let body_v = body.and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
        let name = body_v
            .as_ref()
            .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(String::from));
        let tech_stack: Option<String> = match body_v.as_ref().and_then(|v| v.get("tech_stack")) {
            None => None,
            Some(v) if v.is_null() => None,
            Some(v) => v.as_str().and_then(|s| {
                let t = s.trim();
                if t.is_empty() {
                    None
                } else {
                    Some(t.chars().take(MAX_TECH_STACK_CHARS).collect::<String>())
                }
            }),
        };
        let meta = StudioMeta {
            id: id.clone(),
            name: name.unwrap_or_else(|| "Untitled".to_string()),
            created_at: chrono::Utc::now().to_rfc3339(),
            evolutions: Vec::new(),
            tech_stack,
            verify_skip: false,
            verify_argv: None,
            verify_timeout_sec: None,
            evolution_summary: None,
            policy_notes: None,
        };
        let _ = save_studio_meta(&dir, &meta);
        let _ = write_initial_code_studio_plan(&dir, &meta.name, meta.tech_stack.as_deref());
        let specs_dir = dir.join("specs");
        if let Err(e) = fs::create_dir_all(&specs_dir) {
            tracing::warn!(
                error = %e,
                path = %specs_dir.display(),
                "failed to create default specs/ directory for new studio project"
            );
        }
        if !dir.join(".git").exists() {
            let init_main = git_output(&dir, &["init", "-b", "main"]).await;
            let init_ok = match init_main {
                Ok(o) if o.status.success() => true,
                _ => {
                    match git_output(&dir, &["init"]).await {
                        Ok(o2) if o2.status.success() => true,
                        Ok(_) => false,
                        Err(e) => {
                            tracing::warn!(error = %e, path = %dir.display(), "git init failed for new studio project");
                            false
                        }
                    }
                }
            };
            if init_ok {
                if let Err(e) = ensure_main_or_master_branch(&dir).await {
                    tracing::warn!(error = %e, path = %dir.display(), "failed to ensure main/master after git init");
                }
            }
        }
        schedule_studio_code_rag_index(data_dir, &id, &dir, false);
        let body = serde_json::json!({ "id": id, "path": dir.display().to_string() }).to_string();
        return Some(json_response("201 Created", &body));
    }

    // GET /api/studio/projects/:id
    if method == "GET" {
        if let Some(rest) = strip_studio_projects_prefix(path_only) {
            let segments: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
            if segments.len() == 1 {
                let id = segments[0];
                let root = match resolve_studio_project_dir(data_dir, id) {
                    Ok(d) => d,
                    Err(e) => {
                        return Some(json_response(
                            "400 Bad Request",
                            &serde_json::json!({ "error": e }).to_string(),
                        ));
                    }
                };
                if !root.is_dir() {
                    return Some(json_response("404 Not Found", r#"{"error":"project_not_found"}"#));
                }
                if let Err(e) = ensure_main_or_master_branch(&root).await {
                    tracing::warn!(error = %e, path = %root.display(), "failed to ensure main/master on project resume");
                }
                let meta = load_studio_meta(&root).unwrap_or(StudioMeta {
                    id: id.to_string(),
                    name: "Untitled".to_string(),
                    created_at: String::new(),
                    evolutions: Vec::new(),
                    tech_stack: None,
                    verify_skip: false,
                    verify_argv: None,
                    verify_timeout_sec: None,
                    evolution_summary: None,
                    policy_notes: None,
                });
                let mut body = serde_json::to_value(&meta).unwrap_or_else(|_| serde_json::json!({}));
                if let Some(obj) = body.as_object_mut() {
                    let (branch, clean, wt_lines) = git_branch_clean_worktree_lines(&root).await;
                    obj.insert("git_branch".into(), serde_json::json!(branch));
                    obj.insert("git_worktree_clean".into(), serde_json::json!(clean));
                    obj.insert("git_worktree_lines".into(), serde_json::json!(wt_lines));
                }
                return Some(json_response("200 OK", &body.to_string()));
            }
            // GET /api/studio/projects/:id/code-rag/status
            if segments.len() == 3 && segments[1] == "code-rag" && segments[2] == "status" {
                let id = segments[0];
                let root = match resolve_studio_project_dir(data_dir, id) {
                    Ok(d) => d,
                    Err(e) => {
                        return Some(json_response(
                            "400 Bad Request",
                            &serde_json::json!({ "error": e }).to_string(),
                        ));
                    }
                };
                if !root.is_dir() {
                    return Some(json_response("404 Not Found", r#"{"error":"project_not_found"}"#));
                }
                let data_dir_owned = data_dir.to_path_buf();
                let id_owned = id.to_string();
                let root_owned = root.clone();
                let status = tokio::task::spawn_blocking(move || {
                    let store = crate::code_rag::CodeRagStore::new(&data_dir_owned);
                    store.get_status(&id_owned, &root_owned)
                })
                .await
                .ok()
                .and_then(|r| r.ok());
                let Some(status) = status else {
                    return Some(json_response(
                        "500 Internal Server Error",
                        r#"{"error":"code_rag_status_failed"}"#,
                    ));
                };
                let body = serde_json::to_string(&status)
                .unwrap_or_else(|_| "{}".to_string());
                return Some(json_response("200 OK", &body));
            }
        }
    }

    // POST /api/studio/projects/:id/code-rag/reindex
    if method == "POST" {
        if let Some(rest) = strip_studio_projects_prefix(path_only) {
            let segments: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
            if segments.len() == 3 && segments[1] == "code-rag" && segments[2] == "reindex" {
                let id = segments[0];
                let root = match resolve_studio_project_dir(data_dir, id) {
                    Ok(d) => d,
                    Err(e) => {
                        return Some(json_response(
                            "400 Bad Request",
                            &serde_json::json!({ "error": e }).to_string(),
                        ));
                    }
                };
                if !root.is_dir() {
                    return Some(json_response("404 Not Found", r#"{"error":"project_not_found"}"#));
                }
                let data_dir_owned = data_dir.to_path_buf();
                let id_owned = id.to_string();
                let root_owned = root.clone();
                let force = body
                    .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
                    .and_then(|v| v.get("force").and_then(|x| x.as_bool()).or(Some(true)))
                    .unwrap_or(true);
                let status = tokio::task::spawn_blocking(move || {
                    let store = crate::code_rag::CodeRagStore::new(&data_dir_owned);
                    store.ensure_index(&id_owned, &root_owned, force)
                })
                .await
                .ok()
                .and_then(|r| r.ok());
                let Some(status) = status else {
                    return Some(json_response(
                        "500 Internal Server Error",
                        r#"{"error":"code_rag_reindex_failed"}"#,
                    ));
                };
                let body = serde_json::to_string(&status)
                .unwrap_or_else(|_| "{}".to_string());
                return Some(json_response("200 OK", &body));
            }
        }
    }

    // PATCH /api/studio/projects/:id — optional `name` and/or `tech_stack` (string or null clears stack).
    if method == "PATCH" {
        if let Some(rest) = strip_studio_projects_prefix(path_only) {
            let segments: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
            if segments.len() == 1 {
                let id = segments[0];
                let root = match resolve_studio_project_dir(data_dir, id) {
                    Ok(d) => d,
                    Err(e) => {
                        return Some(json_response(
                            "400 Bad Request",
                            &serde_json::json!({ "error": e }).to_string(),
                        ));
                    }
                };
                if !root.is_dir() {
                    return Some(json_response("404 Not Found", r#"{"error":"project_not_found"}"#));
                }
                let body_v = match body.and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok()) {
                    Some(v) => v,
                    None => {
                        return Some(json_response("400 Bad Request", r#"{"error":"json body required"}"#));
                    }
                };
                let has_name = body_v.get("name").is_some();
                let has_stack = body_v.get("tech_stack").is_some();
                let has_verify_skip = body_v.get("verify_skip").is_some();
                let has_verify_argv = body_v.get("verify_argv").is_some();
                let has_verify_timeout = body_v.get("verify_timeout_sec").is_some();
                let has_evolution_summary = body_v.get("evolution_summary").is_some();
                let has_policy_notes = body_v.get("policy_notes").is_some();
                if !has_name
                    && !has_stack
                    && !has_verify_skip
                    && !has_verify_argv
                    && !has_verify_timeout
                    && !has_evolution_summary
                    && !has_policy_notes
                {
                    return Some(json_response(
                        "400 Bad Request",
                        r#"{"error":"provide at least one of: name, tech_stack, verify_skip, verify_argv, verify_timeout_sec, evolution_summary, policy_notes"}"#,
                    ));
                }
                let mut meta = load_studio_meta(&root).unwrap_or(StudioMeta {
                    id: id.to_string(),
                    name: "Untitled".to_string(),
                    created_at: chrono::Utc::now().to_rfc3339(),
                    evolutions: Vec::new(),
                    tech_stack: None,
                    verify_skip: false,
                    verify_argv: None,
                    verify_timeout_sec: None,
                    evolution_summary: None,
                    policy_notes: None,
                });
                if has_name {
                    let new_name = match body_v.get("name").and_then(|x| x.as_str()).map(str::trim) {
                        Some(s) if !s.is_empty() && s.len() <= 200 => s.to_string(),
                        _ => {
                            return Some(json_response(
                                "400 Bad Request",
                                r#"{"error":"name must be non-empty string, max 200 chars"}"#,
                            ));
                        }
                    };
                    meta.name = new_name;
                }
                if has_stack {
                    match body_v.get("tech_stack") {
                        Some(v) if v.is_null() => {
                            meta.tech_stack = None;
                        }
                        Some(v) => {
                            let s = match v.as_str() {
                                Some(t) => t,
                                None => {
                                    return Some(json_response(
                                        "400 Bad Request",
                                        r#"{"error":"tech_stack must be string or null"}"#,
                                    ));
                                }
                            };
                            if s.len() > MAX_TECH_STACK_CHARS {
                                return Some(json_response(
                                    "400 Bad Request",
                                    &serde_json::json!({ "error": "tech_stack too long", "max": MAX_TECH_STACK_CHARS })
                                        .to_string(),
                                ));
                            }
                            let t = s.trim();
                            meta.tech_stack = if t.is_empty() {
                                None
                            } else {
                                Some(t.to_string())
                            };
                        }
                        None => {}
                    }
                }
                if has_verify_skip {
                    meta.verify_skip = body_v
                        .get("verify_skip")
                        .and_then(|x| x.as_bool())
                        .unwrap_or(false);
                }
                if has_verify_argv {
                    match body_v.get("verify_argv") {
                        Some(v) if v.is_null() => {
                            meta.verify_argv = None;
                        }
                        Some(v) => {
                            let Some(a) = v.as_array() else {
                                return Some(json_response(
                                    "400 Bad Request",
                                    r#"{"error":"verify_argv must be array or null"}"#,
                                ));
                            };
                            let mut out: Vec<String> = Vec::new();
                            for x in a {
                                let Some(s) = x.as_str() else {
                                    return Some(json_response(
                                        "400 Bad Request",
                                        r#"{"error":"verify_argv elements must be strings"}"#,
                                    ));
                                };
                                out.push(s.to_string());
                            }
                            meta.verify_argv = if out.is_empty() { None } else { Some(out) };
                        }
                        None => {}
                    }
                }
                if has_verify_timeout {
                    match body_v.get("verify_timeout_sec") {
                        Some(v) if v.is_null() => {
                            meta.verify_timeout_sec = None;
                        }
                        Some(v) => {
                            let n = v.as_u64().or_else(|| v.as_i64().map(|x| x.max(1) as u64));
                            match n {
                                Some(n) if n > 0 && n <= 3600 => meta.verify_timeout_sec = Some(n),
                                _ => {
                                    return Some(json_response(
                                        "400 Bad Request",
                                        r#"{"error":"verify_timeout_sec must be 1..=3600 or null"}"#,
                                    ));
                                }
                            }
                        }
                        None => {}
                    }
                }
                if has_evolution_summary {
                    match body_v.get("evolution_summary") {
                        Some(v) if v.is_null() => {
                            meta.evolution_summary = None;
                        }
                        Some(v) => {
                            let s = match v.as_str() {
                                Some(t) => t,
                                None => {
                                    return Some(json_response(
                                        "400 Bad Request",
                                        r#"{"error":"evolution_summary must be string or null"}"#,
                                    ));
                                }
                            };
                            if s.chars().count() > MAX_EVOLUTION_SUMMARY_CHARS {
                                return Some(json_response(
                                    "400 Bad Request",
                                    &serde_json::json!({ "error": "evolution_summary too long", "max": MAX_EVOLUTION_SUMMARY_CHARS })
                                        .to_string(),
                                ));
                            }
                            let t = s.trim();
                            meta.evolution_summary = if t.is_empty() {
                                None
                            } else {
                                Some(t.to_string())
                            };
                        }
                        None => {}
                    }
                }
                if has_policy_notes {
                    match body_v.get("policy_notes") {
                        Some(v) if v.is_null() => {
                            meta.policy_notes = None;
                        }
                        Some(v) => {
                            let s = match v.as_str() {
                                Some(t) => t,
                                None => {
                                    return Some(json_response(
                                        "400 Bad Request",
                                        r#"{"error":"policy_notes must be string or null"}"#,
                                    ));
                                }
                            };
                            if s.chars().count() > MAX_POLICY_NOTES_CHARS {
                                return Some(json_response(
                                    "400 Bad Request",
                                    &serde_json::json!({ "error": "policy_notes too long", "max": MAX_POLICY_NOTES_CHARS })
                                        .to_string(),
                                ));
                            }
                            let t = s.trim();
                            meta.policy_notes = if t.is_empty() {
                                None
                            } else {
                                Some(t.to_string())
                            };
                        }
                        None => {}
                    }
                }
                if let Err(e) = save_studio_meta(&root, &meta) {
                    return Some(json_response(
                        "500 Internal Server Error",
                        &serde_json::json!({ "error": e }).to_string(),
                    ));
                }
                let body = serde_json::json!({
                    "ok": true,
                    "id": id,
                    "name": meta.name,
                    "tech_stack": meta.tech_stack,
                    "verify_skip": meta.verify_skip,
                    "verify_argv": meta.verify_argv,
                    "verify_timeout_sec": meta.verify_timeout_sec,
                    "evolution_summary": meta.evolution_summary,
                    "policy_notes": meta.policy_notes,
                })
                .to_string();
                return Some(json_response("200 OK", &body));
            }
        }
    }

    // GET /api/studio/projects/:id/files
    if method == "GET" && path_only.ends_with("/files") {
        if let Some(rest) = strip_studio_projects_prefix(path_only) {
            let id = rest.strip_suffix("/files").unwrap_or(rest);
            let root = match resolve_studio_project_dir(data_dir, id) {
                Ok(d) => d,
                Err(e) => {
                    return Some(json_response(
                        "400 Bad Request",
                        &serde_json::json!({ "error": e }).to_string(),
                    ));
                }
            };
            if !root.is_dir() {
                return Some(json_response("404 Not Found", r#"{"error":"project_not_found"}"#));
            }
            let mut files: Vec<String> = Vec::new();
            collect_files_recursive(&root, Path::new(""), 0, &mut files);
            files.sort();
            let body = serde_json::json!({ "files": files }).to_string();
            return Some(json_response("200 OK", &body));
        }
    }

    // GET /api/studio/projects/:id/raw?path=
    if method == "GET" && path_only.ends_with("/raw") {
        if let Some(rest) = strip_studio_projects_prefix(path_only) {
            let id = rest.strip_suffix("/raw").unwrap_or(rest);
            let root = match resolve_studio_project_dir(data_dir, id) {
                Ok(d) => d,
                Err(e) => {
                    return Some(json_response(
                        "400 Bad Request",
                        &serde_json::json!({ "error": e }).to_string(),
                    ));
                }
            };
            let Some(rel) = query_param(query_str, "path").map(|c| c.to_string()).filter(|s| !s.is_empty()) else {
                return Some(json_response("400 Bad Request", r#"{"error":"path query required"}"#));
            };
            if rel.contains("..") {
                return Some(json_response("400 Bad Request", r#"{"error":"invalid path"}"#));
            }
            let full = strip_verbatim(&root.join(&rel));
            if !is_strictly_under_studio_root(&full, &root) {
                return Some(json_response("400 Bad Request", r#"{"error":"path outside project"}"#));
            }
            if !full.is_file() {
                return Some(json_response("404 Not Found", r#"{"error":"not_found"}"#));
            }
            match fs::read(&full) {
                Ok(bytes) if bytes.len() <= MAX_RAW_BYTES => {
                    let (mime, is_text) = guess_mime_and_text(&rel, &bytes);
                    let body = if is_text {
                        serde_json::json!({
                            "path": rel,
                            "mime": mime,
                            "content": String::from_utf8_lossy(&bytes),
                        })
                        .to_string()
                    } else {
                        use base64::Engine;
                        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                        serde_json::json!({
                            "path": rel,
                            "mime": mime,
                            "content_base64": b64,
                        })
                        .to_string()
                    };
                    return Some(json_response("200 OK", &body));
                }
                Ok(bytes) => {
                    return Some(json_response(
                        "413 Payload Too Large",
                        &serde_json::json!({ "error": "file_too_large", "size": bytes.len() }).to_string(),
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
    }

    // PUT /api/studio/projects/:id/raw?path=
    // Body: `{ "content": "<utf-8 text>" }` — same path rules and size cap as GET.
    if method == "PUT" && path_only.ends_with("/raw") {
        if let Some(rest) = strip_studio_projects_prefix(path_only) {
            let id = rest.strip_suffix("/raw").unwrap_or(rest);
            let root = match resolve_studio_project_dir(data_dir, id) {
                Ok(d) => d,
                Err(e) => {
                    return Some(json_response(
                        "400 Bad Request",
                        &serde_json::json!({ "error": e }).to_string(),
                    ));
                }
            };
            let Some(rel) = query_param(query_str, "path").map(|c| c.to_string()).filter(|s| !s.trim().is_empty()) else {
                return Some(json_response("400 Bad Request", r#"{"error":"path query required"}"#));
            };
            if rel.contains("..") {
                return Some(json_response("400 Bad Request", r#"{"error":"invalid path"}"#));
            }
            let full = strip_verbatim(&root.join(&rel));
            if !is_strictly_under_studio_root(&full, &root) {
                return Some(json_response("400 Bad Request", r#"{"error":"path outside project"}"#));
            }
            if full.is_dir() {
                return Some(json_response("400 Bad Request", r#"{"error":"path is a directory"}"#));
            }
            let body_v = match body.and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok()) {
                Some(v) => v,
                None => {
                    return Some(json_response("400 Bad Request", r#"{"error":"json body required"}"#));
                }
            };
            let content = match body_v.get("content") {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(serde_json::Value::Null) => String::new(),
                None => {
                    return Some(json_response("400 Bad Request", r#"{"error":"content field required"}"#));
                }
                _ => {
                    return Some(json_response(
                        "400 Bad Request",
                        r#"{"error":"content must be a JSON string"}"#,
                    ));
                }
            };
            let bytes = content.as_bytes();
            if bytes.len() > MAX_RAW_BYTES {
                return Some(json_response(
                    "413 Payload Too Large",
                    &serde_json::json!({ "error": "content_too_large", "size": bytes.len() }).to_string(),
                ));
            }
            if let Some(parent) = full.parent() {
                if let Err(e) = fs::create_dir_all(parent) {
                    return Some(json_response(
                        "500 Internal Server Error",
                        &serde_json::json!({ "error": e.to_string() }).to_string(),
                    ));
                }
            }
            match fs::write(&full, bytes) {
                Ok(()) => {
                    schedule_studio_code_rag_index(data_dir, id, &root, false);
                    let body = serde_json::json!({ "ok": true, "path": rel }).to_string();
                    return Some(json_response("200 OK", &body));
                }
                Err(e) => {
                    return Some(json_response(
                        "500 Internal Server Error",
                        &serde_json::json!({ "error": e.to_string() }).to_string(),
                    ));
                }
            }
        }
    }

    // DELETE /api/studio/projects/:id/raw?path= — supprime un fichier (même règles de chemin que GET/PUT).
    if method == "DELETE" && path_only.ends_with("/raw") {
        if let Some(rest) = strip_studio_projects_prefix(path_only) {
            let id = rest.strip_suffix("/raw").unwrap_or(rest);
            let root = match resolve_studio_project_dir(data_dir, id) {
                Ok(d) => d,
                Err(e) => {
                    return Some(json_response(
                        "400 Bad Request",
                        &serde_json::json!({ "error": e }).to_string(),
                    ));
                }
            };
            let Some(rel) = query_param(query_str, "path").map(|c| c.to_string()).filter(|s| !s.trim().is_empty()) else {
                return Some(json_response("400 Bad Request", r#"{"error":"path query required"}"#));
            };
            if rel.contains("..") {
                return Some(json_response("400 Bad Request", r#"{"error":"invalid path"}"#));
            }
            let full = strip_verbatim(&root.join(&rel));
            if !is_strictly_under_studio_root(&full, &root) {
                return Some(json_response("400 Bad Request", r#"{"error":"path outside project"}"#));
            }
            if full.is_dir() {
                return Some(json_response("400 Bad Request", r#"{"error":"path is a directory"}"#));
            }
            match fs::remove_file(&full) {
                Ok(()) => {
                    schedule_studio_code_rag_index(data_dir, id, &root, false);
                    let body = serde_json::json!({ "ok": true, "path": rel }).to_string();
                    return Some(json_response("200 OK", &body));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Some(json_response("404 Not Found", r#"{"error":"not_found"}"#));
                }
                Err(e) => {
                    return Some(json_response(
                        "500 Internal Server Error",
                        &serde_json::json!({ "error": e.to_string() }).to_string(),
                    ));
                }
            }
        }
    }

    // POST /api/studio/projects/:id/fs/rename — renomme ou déplace un fichier ou un répertoire (rename atomique).
    if method == "POST" && path_only.ends_with("/fs/rename") {
        if let Some(rest) = strip_studio_projects_prefix(path_only) {
            let id = rest.strip_suffix("/fs/rename").unwrap_or(rest).trim_end_matches('/');
            let root = match resolve_studio_project_dir(data_dir, id) {
                Ok(d) => d,
                Err(e) => {
                    return Some(json_response(
                        "400 Bad Request",
                        &serde_json::json!({ "error": e }).to_string(),
                    ));
                }
            };
            if !root.is_dir() {
                return Some(json_response("404 Not Found", r#"{"error":"project_not_found"}"#));
            }
            let body_v = match body.and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok()) {
                Some(v) => v,
                None => {
                    return Some(json_response("400 Bad Request", r#"{"error":"json body required"}"#));
                }
            };
            let from_rel = match body_v.get("from").and_then(|x| x.as_str()) {
                Some(s) if !s.trim().is_empty() => s.trim().replace('\\', "/"),
                _ => {
                    return Some(json_response("400 Bad Request", r#"{"error":"from required"}"#));
                }
            };
            let to_rel = match body_v.get("to").and_then(|x| x.as_str()) {
                Some(s) if !s.trim().is_empty() => s.trim().replace('\\', "/"),
                _ => {
                    return Some(json_response("400 Bad Request", r#"{"error":"to required"}"#));
                }
            };
            if from_rel.contains("..") || to_rel.contains("..") {
                return Some(json_response("400 Bad Request", r#"{"error":"invalid path"}"#));
            }
            if from_rel == to_rel {
                return Some(json_response("400 Bad Request", r#"{"error":"same_path"}"#));
            }
            fn blocked_studio_rel_segment(rel: &str) -> bool {
                rel.split('/').any(|seg| {
                    seg == ".git" || seg == "node_modules" || seg == ".akasha-studio.json"
                })
            }
            if blocked_studio_rel_segment(&from_rel) || blocked_studio_rel_segment(&to_rel) {
                return Some(json_response(
                    "400 Bad Request",
                    r#"{"error":"path segment not allowed"}"#,
                ));
            }
            let from_full = strip_verbatim(&root.join(&from_rel));
            let to_full = strip_verbatim(&root.join(&to_rel));
            if !is_strictly_under_studio_root(&from_full, &root) || !is_strictly_under_studio_root(&to_full, &root) {
                return Some(json_response("400 Bad Request", r#"{"error":"path outside project"}"#));
            }
            if !from_full.exists() {
                return Some(json_response("404 Not Found", r#"{"error":"not_found"}"#));
            }
            if to_full.exists() {
                return Some(json_response(
                    "400 Bad Request",
                    r#"{"error":"destination_exists"}"#,
                ));
            }
            let from_s = from_full.to_string_lossy().replace('\\', "/");
            let to_s = to_full.to_string_lossy().replace('\\', "/");
            if from_full.is_dir() && to_s.starts_with(&(from_s.clone() + "/")) {
                return Some(json_response(
                    "400 Bad Request",
                    r#"{"error":"cannot_move_into_subdirectory"}"#,
                ));
            }
            if let Some(parent) = to_full.parent() {
                if let Err(e) = fs::create_dir_all(parent) {
                    return Some(json_response(
                        "500 Internal Server Error",
                        &serde_json::json!({ "error": e.to_string() }).to_string(),
                    ));
                }
            }
            match fs::rename(&from_full, &to_full) {
                Ok(()) => {
                    schedule_studio_code_rag_index(data_dir, id, &root, false);
                    let body = serde_json::json!({ "ok": true, "from": from_rel, "to": to_rel }).to_string();
                    return Some(json_response("200 OK", &body));
                }
                Err(e) => {
                    return Some(json_response(
                        "500 Internal Server Error",
                        &serde_json::json!({ "error": e.to_string() }).to_string(),
                    ));
                }
            }
        }
    }

    // POST /api/studio/projects/:id/git/clone
    if method == "POST" && path_only.contains("/api/studio/projects/") && path_only.ends_with("/git/clone") {
        let rest = path_only
            .strip_prefix("/api/studio/projects/")
            .unwrap_or("");
        let id = rest.strip_suffix("/git/clone").unwrap_or(rest).trim_end_matches('/');
        let id = id.split('/').next().unwrap_or("");
        let root = match resolve_studio_project_dir(data_dir, id) {
            Ok(d) => d,
            Err(e) => {
                return Some(json_response(
                    "400 Bad Request",
                    &serde_json::json!({ "error": e }).to_string(),
                ));
            }
        };
        let _ = fs::create_dir_all(&root);
        let body_v = match body.and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok()) {
            Some(v) => v,
            None => {
                return Some(json_response("400 Bad Request", r#"{"error":"json body required"}"#));
            }
        };
        let url = match body_v
            .get("repo_url")
            .and_then(|u| u.as_str())
            .filter(|s| !s.is_empty())
        {
            Some(u) => u,
            None => {
                return Some(json_response("400 Bad Request", r#"{"error":"repo_url required"}"#));
            }
        };
        if !url.starts_with("https://") {
            return Some(json_response(
                "400 Bad Request",
                r#"{"error":"only https repo_url supported"}"#,
            ));
        }
        // Studio-managed entries that are created at project-creation time and can be safely
        // cleared before cloning (git clone requires an empty target directory).
        const STUDIO_METADATA_ENTRIES: &[&str] =
            &[".akasha-studio.json", "CODE_STUDIO_PLAN.md", "specs", ".git"];
        let mut has_user_content = false;
        if let Ok(rd) = fs::read_dir(&root) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_string();
                if !STUDIO_METADATA_ENTRIES.contains(&n.as_str()) {
                    has_user_content = true;
                    break;
                }
            }
        }
        if has_user_content {
            return Some(json_response(
                "409 Conflict",
                r#"{"error":"project_dir_not_empty"}"#,
            ));
        }
        // Save .akasha-studio.json to restore after the clone (it stores project metadata).
        let meta_backup = tokio::fs::read(root.join(".akasha-studio.json")).await.ok();
        // Wipe only the known studio-managed entries so git clone finds an empty target.
        // Use DirEntry::file_type() to avoid following symlinks when deciding remove strategy.
        if let Ok(rd) = fs::read_dir(&root) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_string();
                if !STUDIO_METADATA_ENTRIES.contains(&n.as_str()) {
                    continue; // unexpected entry — skip to avoid accidental data loss
                }
                let p = e.path();
                let ft = e.file_type().ok();
                if ft.map_or(false, |t| t.is_dir()) {
                    let _ = fs::remove_dir_all(&p);
                } else {
                    let _ = fs::remove_file(&p);
                }
            }
        }
        let _permit = studio_ops_semaphore().acquire().await.ok();
        let mut cmd = Command::new("git");
        cmd.arg("clone").arg("--depth").arg("1");
        if let Some(b) = body_v.get("branch").and_then(|x| x.as_str()).filter(|s| !s.is_empty()) {
            cmd.arg("--branch").arg(b);
        }
        cmd.arg(url).arg(&root);
        cmd.kill_on_drop(true);
        match cmd.status().await {
            Ok(s) if s.success() => {
                // Restore project metadata so the daemon can still track this project.
                if let Some(bytes) = meta_backup {
                    let _ = tokio::fs::write(root.join(".akasha-studio.json"), bytes).await;
                }
                return Some(json_response("200 OK", r#"{"ok":true,"message":"cloned"}"#));
            }
            Ok(s) => {
                return Some(json_response(
                    "500 Internal Server Error",
                    &serde_json::json!({ "error": format!("git clone failed: {}", s) }).to_string(),
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

    // POST /api/studio/projects/:id/preview/stop — kill dev server started for this project.
    if method == "POST" && path_only.contains("/api/studio/projects/") && path_only.ends_with("/preview/stop") {
        let rest = path_only.strip_prefix("/api/studio/projects/").unwrap_or("");
        let id = rest
            .strip_suffix("/preview/stop")
            .unwrap_or(rest)
            .trim_end_matches('/');
        let id = id.split('/').next().unwrap_or("");
        if id.is_empty() {
            return Some(json_response("400 Bad Request", r#"{"error":"invalid path"}"#));
        }
        let mut map = studio_preview_registry().lock().await;
        if let Some(mut prev) = map.remove(id) {
            let _ = prev.child.kill().await;
            return Some(json_response("200 OK", r#"{"ok":true,"stopped":true}"#));
        }
        return Some(json_response("200 OK", r#"{"ok":true,"stopped":false}"#));
    }

    // GET /api/studio/projects/:id/preview/logs — stdout/stderr du serveur dev (tampon borné).
    if method == "GET" && path_only.contains("/api/studio/projects/") && path_only.ends_with("/preview/logs") {
        let rest = path_only.strip_prefix("/api/studio/projects/").unwrap_or("");
        let id = rest
            .strip_suffix("/preview/logs")
            .unwrap_or(rest)
            .trim_end_matches('/');
        let id = id.split('/').next().unwrap_or("");
        if id.is_empty() {
            return Some(json_response("400 Bad Request", r#"{"error":"invalid path"}"#));
        }
        if resolve_studio_project_dir(data_dir, id).is_err() {
            return Some(json_response(
                "400 Bad Request",
                r#"{"error":"invalid studio_project_id"}"#,
            ));
        }
        let mut map = studio_preview_registry().lock().await;
        if let Some(prev) = map.get_mut(id) {
            let running = match prev.child.try_wait() {
                Ok(None) => true,
                Ok(Some(_)) => false,
                Err(_) => false,
            };
            let log_text = prev.log.lock().await.clone();
            let body = serde_json::json!({
                "running": running,
                "log": log_text,
            });
            return Some(json_response("200 OK", &body.to_string()));
        }
        return Some(json_response(
            "200 OK",
            r#"{"running":false,"log":"","preview_inactive":true}"#,
        ));
    }

    // GET /api/studio/projects/:id/preview/proxy?token=... — signed short-lived redirect to dev preview.
    if method == "GET" && path_only.contains("/api/studio/projects/") && path_only.ends_with("/preview/proxy") {
        let rest = path_only.strip_prefix("/api/studio/projects/").unwrap_or("");
        let id = rest.strip_suffix("/preview/proxy").unwrap_or(rest).trim_end_matches('/');
        let id = id.split('/').next().unwrap_or("");
        if id.is_empty() {
            return Some(json_response("400 Bad Request", r#"{"error":"invalid path"}"#));
        }
        let token = query_param(query_str, "token")
            .map(|v| v.to_string())
            .unwrap_or_default();
        if token.trim().is_empty() {
            return Some(json_response("401 Unauthorized", r#"{"error":"token_required"}"#));
        }
        let port = match validate_preview_proxy_token(id, token.trim()).await {
            Ok(p) => p,
            Err(e) => {
                return Some(json_response(
                    "401 Unauthorized",
                    &serde_json::json!({ "error": e }).to_string(),
                ));
            }
        };
        let location = format!("http://127.0.0.1:{port}");
        let body = serde_json::json!({
            "ok": true,
            "proxy": true,
            "redirect_to": location
        })
        .to_string();
        return Some(format!(
            "HTTP/1.1 307 Temporary Redirect\r\nContent-Type: application/json; charset=utf-8\r\nAccess-Control-Allow-Origin: *\r\nLocation: {location}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.as_bytes().len(),
            body
        ));
    }

    // POST /api/studio/projects/:id/preview/install — npm install uniquement (pas de serveur dev).
    if method == "POST" && path_only.contains("/api/studio/projects/") && path_only.ends_with("/preview/install") {
        let rest = path_only.strip_prefix("/api/studio/projects/").unwrap_or("");
        let id = rest
            .strip_suffix("/preview/install")
            .unwrap_or(rest)
            .trim_end_matches('/');
        let id = id.split('/').next().unwrap_or("");
        if id.is_empty() {
            return Some(json_response("400 Bad Request", r#"{"error":"invalid path"}"#));
        }
        let root = match resolve_studio_project_dir(data_dir, id) {
            Ok(d) => d,
            Err(e) => {
                return Some(json_response(
                    "400 Bad Request",
                    &serde_json::json!({ "error": e }).to_string(),
                ));
            }
        };
        if !root.is_dir() {
            return Some(json_response("404 Not Found", r#"{"error":"project_not_found"}"#));
        }
        let pkg = root.join("package.json");
        if !pkg.is_file() {
            return Some(json_response(
                "400 Bad Request",
                r#"{"error":"package_json_required_for_preview"}"#,
            ));
        }
        let body_v = body.and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
        let force = body_v
            .as_ref()
            .and_then(|v| v.get("force"))
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        let node_modules = root.join("node_modules");
        if !force && node_modules.is_dir() {
            return Some(json_response(
                "200 OK",
                r#"{"ok":true,"skipped":true,"reason":"node_modules_present"}"#,
            ));
        }
        let _permit = match studio_ops_semaphore().acquire().await {
            Ok(p) => p,
            Err(_) => {
                return Some(json_response(
                    "503 Service Unavailable",
                    r#"{"error":"studio_ops_semaphore_closed"}"#,
                ));
            }
        };
        let argv = vec!["npm".to_string(), "install".to_string()];
        match studio_run_command_capture(&root, &argv, NPM_INSTALL_TIMEOUT_SEC).await {
            Ok((code, stdout, stderr)) => {
                let ok = code == Some(0);
                let body = serde_json::json!({
                    "ok": ok,
                    "install": {
                        "exit_code": code,
                        "stdout": stdout,
                        "stderr": stderr,
                    }
                });
                let status = if ok { "200 OK" } else { "500 Internal Server Error" };
                return Some(json_response(status, &body.to_string()));
            }
            Err(e) => {
                return Some(json_response(
                    "500 Internal Server Error",
                    &serde_json::json!({ "error": e }).to_string(),
                ));
            }
        }
    }

    // POST /api/studio/projects/:id/preview/start — npm install if needed, then npm run dev (background).
    if method == "POST" && path_only.contains("/api/studio/projects/") && path_only.ends_with("/preview/start") {
        let rest = path_only.strip_prefix("/api/studio/projects/").unwrap_or("");
        let id = rest
            .strip_suffix("/preview/start")
            .unwrap_or(rest)
            .trim_end_matches('/');
        let id = id.split('/').next().unwrap_or("");
        if id.is_empty() {
            return Some(json_response("400 Bad Request", r#"{"error":"invalid path"}"#));
        }
        let root = match resolve_studio_project_dir(data_dir, id) {
            Ok(d) => d,
            Err(e) => {
                return Some(json_response(
                    "400 Bad Request",
                    &serde_json::json!({ "error": e }).to_string(),
                ));
            }
        };
        if !root.is_dir() {
            return Some(json_response("404 Not Found", r#"{"error":"project_not_found"}"#));
        }
        let pkg = root.join("package.json");
        if !pkg.is_file() {
            return Some(json_response(
                "400 Bad Request",
                r#"{"error":"package_json_required_for_preview","hint":"Use static HTML preview in the UI or add a package.json with a dev script."}"#,
            ));
        }
        let body_v = body.and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
        let force_install = body_v
            .as_ref()
            .and_then(|v| v.get("force_install"))
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        let preferred_port = body_v
            .as_ref()
            .and_then(|v| v.get("port"))
            .and_then(|x| x.as_u64())
            .and_then(|n| u16::try_from(n).ok());
        let _permit = match studio_ops_semaphore().acquire().await {
            Ok(p) => p,
            Err(_) => {
                return Some(json_response(
                    "503 Service Unavailable",
                    r#"{"error":"studio_ops_semaphore_closed"}"#,
                ));
            }
        };
        let node_modules = root.join("node_modules");
        let mut install_block: Option<serde_json::Value> = None;
        if force_install || !node_modules.is_dir() {
            let argv = vec!["npm".to_string(), "install".to_string()];
            match studio_run_command_capture(&root, &argv, NPM_INSTALL_TIMEOUT_SEC).await {
                Ok((code, stdout, stderr)) => {
                    let ok = code == Some(0);
                    install_block = Some(serde_json::json!({
                        "exit_code": code,
                        "stdout": stdout,
                        "stderr": stderr,
                    }));
                    if !ok {
                        return Some(json_response(
                            "500 Internal Server Error",
                            &serde_json::json!({
                                "error": "npm_install_failed",
                                "install": install_block,
                            })
                            .to_string(),
                        ));
                    }
                }
                Err(e) => {
                    return Some(json_response(
                        "500 Internal Server Error",
                        &serde_json::json!({ "error": e }).to_string(),
                    ));
                }
            }
        }
        let Some(port) = pick_preview_port(preferred_port) else {
            return Some(json_response(
                "503 Service Unavailable",
                r#"{"error":"no_free_preview_port"}"#,
            ));
        };
        let mut map = studio_preview_registry().lock().await;
        if let Some(mut old) = map.remove(id) {
            let _ = old.child.kill().await;
        }
        let port_s = port.to_string();
        let argv = vec![
            "npm".to_string(),
            "run".to_string(),
            "dev".to_string(),
            "--".to_string(),
            "--host".to_string(),
            "127.0.0.1".to_string(),
            "--port".to_string(),
            port_s.clone(),
        ];
        if !argv_looks_safe(&argv) {
            return Some(json_response(
                "400 Bad Request",
                r#"{"error":"invalid preview argv"}"#,
            ));
        }
        let log = Arc::new(Mutex::new(String::new()));
        let mut cmd = studio_command_from_argv(&argv);
        cmd.current_dir(&root).kill_on_drop(false);
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            cmd.as_std_mut().creation_flags(CREATE_NO_WINDOW);
        }
        match cmd.spawn() {
            Ok(mut child) => {
                let stdout = child.stdout.take();
                let stderr = child.stderr.take();
                let log_out = Arc::clone(&log);
                let log_err = Arc::clone(&log);
                if let Some(s) = stdout {
                    tokio::spawn(studio_pump_preview_stream(s, log_out, "stdout"));
                }
                if let Some(s) = stderr {
                    tokio::spawn(studio_pump_preview_stream(s, log_err, "stderr"));
                }
                map.insert(id.to_string(), StudioPreviewProcess { child, log });
                let token = issue_preview_proxy_token(id, port).await;
                let url = format!("/api/studio/projects/{id}/preview/proxy?token={token}");
                let mut body = serde_json::json!({
                    "ok": true,
                    "url": url,
                    "port": port,
                    "proxy_signed": true,
                });
                if let Some(ib) = install_block {
                    body["installed"] = serde_json::Value::Bool(true);
                    body["install"] = ib;
                } else {
                    body["installed"] = serde_json::Value::Bool(false);
                }
                return Some(json_response("200 OK", &body.to_string()));
            }
            Err(e) => {
                return Some(json_response(
                    "500 Internal Server Error",
                    &serde_json::json!({ "error": e.to_string() }).to_string(),
                ));
            }
        }
    }

    // POST /api/studio/projects/:id/build
    if method == "POST" && path_only.contains("/api/studio/projects/") && path_only.ends_with("/build") {
        let rest = path_only.strip_prefix("/api/studio/projects/").unwrap_or("");
        let id = rest.strip_suffix("/build").unwrap_or(rest).trim_end_matches('/');
        let id = id.split('/').next().unwrap_or("");
        let root = match resolve_studio_project_dir(data_dir, id) {
            Ok(d) => d,
            Err(e) => {
                return Some(json_response(
                    "400 Bad Request",
                    &serde_json::json!({ "error": e }).to_string(),
                ));
            }
        };
        if !root.is_dir() {
            return Some(json_response("404 Not Found", r#"{"error":"project_not_found"}"#));
        }
        let body_v = match body.and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok()) {
            Some(v) => v,
            None => {
                return Some(json_response("400 Bad Request", r#"{"error":"json body required"}"#));
            }
        };
        let argv: Vec<String> = body_v
            .get("argv")
            .and_then(|a| a.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        if !argv_looks_safe(&argv) {
            return Some(json_response(
                "400 Bad Request",
                r#"{"error":"invalid or unsafe argv"}"#,
            ));
        }
        let timeout_sec = body_v
            .get("timeout_sec")
            .and_then(|x| x.as_u64())
            .or_else(|| {
                body_v
                    .get("timeout_sec")
                    .and_then(|x| x.as_i64())
                    .map(|x| x.max(1) as u64)
            })
            .unwrap_or(DEFAULT_BUILD_TIMEOUT_SEC)
            .min(3600);
        if argv.first().is_none() {
            return Some(json_response("400 Bad Request", r#"{"error":"argv required"}"#));
        }
        let _permit = match studio_ops_semaphore().acquire().await {
            Ok(p) => p,
            Err(_) => {
                return Some(json_response(
                    "503 Service Unavailable",
                    r#"{"error":"studio_ops_semaphore_closed"}"#,
                ));
            }
        };
        let containerized = body_v
            .get("containerized")
            .and_then(|x| x.as_bool())
            .unwrap_or(true);
        let allow_host_fallback = body_v
            .get("allow_host_fallback")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        let result = if containerized {
            match run_build_in_container(&root, &argv, timeout_sec).await {
                Ok(v) => Ok((v.0, v.1, v.2, "container")),
                Err(container_err) => {
                    if allow_host_fallback {
                        match studio_run_command_capture(&root, &argv, timeout_sec).await {
                            Ok((code, out, err)) => Ok((code, out, err, "host_fallback")),
                            Err(host_err) => Err(format!("container: {container_err}; host_fallback: {host_err}")),
                        }
                    } else {
                        Err(format!("containerized_build_failed: {container_err}"))
                    }
                }
            }
        } else {
            match studio_run_command_capture(&root, &argv, timeout_sec).await {
                Ok((code, out, err)) => Ok((code, out, err, "host")),
                Err(e) => Err(e),
            }
        };
        let (http_status, body) = match result {
            Ok((exit_code, stdout, stderr, execution_mode)) => {
                let body = serde_json::json!({
                    "exit_code": exit_code,
                    "stdout": stdout,
                    "stderr": stderr,
                    "execution_mode": execution_mode
                })
                .to_string();
                let ok = exit_code == Some(0);
                let http_status = if ok { "200 OK" } else { "500 Internal Server Error" };
                (http_status, body)
            }
            Err(e) => (
                "500 Internal Server Error",
                serde_json::json!({ "error": e, "exit_code": -1 }).to_string(),
            ),
        };
        return Some(json_response(http_status, &body));
    }

    // POST /api/studio/projects/:id/patch/hunks — apply selected unified-diff hunks under studio root.
    if method == "POST" && path_only.contains("/api/studio/projects/") && path_only.ends_with("/patch/hunks") {
        let rest = path_only.strip_prefix("/api/studio/projects/").unwrap_or("");
        let id = rest.strip_suffix("/patch/hunks").unwrap_or(rest).trim_end_matches('/');
        let id = id.split('/').next().unwrap_or("");
        let root = match resolve_studio_project_dir(data_dir, id) {
            Ok(d) => d,
            Err(e) => {
                return Some(json_response(
                    "400 Bad Request",
                    &serde_json::json!({ "error": e }).to_string(),
                ));
            }
        };
        if !root.is_dir() {
            return Some(json_response("404 Not Found", r#"{"error":"project_not_found"}"#));
        }
        let body_v = match body.and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok()) {
            Some(v) => v,
            None => return Some(json_response("400 Bad Request", r#"{"error":"json body required"}"#)),
        };
        let dry_run = body_v.get("dry_run").and_then(|v| v.as_bool()).unwrap_or(false);
        let patches: Vec<String> = body_v
            .get("patches")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();
        if patches.is_empty() {
            return Some(json_response("400 Bad Request", r#"{"error":"patches_required"}"#));
        }
        fn invalid_patch_path(path: &str) -> bool {
            if path.is_empty() || path == "/dev/null" {
                return false;
            }
            if path.starts_with('/') || path.starts_with('\\') {
                return true;
            }
            if path.len() >= 3 {
                let b = path.as_bytes();
                if b[1] == b':' && (b[2] == b'\\' || b[2] == b'/') && b[0].is_ascii_alphabetic() {
                    return true;
                }
            }
            path.split(&['/', '\\'][..]).any(|part| part == "..")
        }
        fn patch_has_invalid_paths(patch: &str) -> bool {
            for line in patch.lines() {
                if let Some(rest) = line.strip_prefix("diff --git ") {
                    let mut parts = rest.split_whitespace();
                    let left = match parts.next() {
                        Some(v) => v,
                        None => return true,
                    };
                    let right = match parts.next() {
                        Some(v) => v,
                        None => return true,
                    };
                    let left = match left.strip_prefix("a/") {
                        Some(v) => v,
                        None => return true,
                    };
                    let right = match right.strip_prefix("b/") {
                        Some(v) => v,
                        None => return true,
                    };
                    if invalid_patch_path(left) || invalid_patch_path(right) {
                        return true;
                    }
                } else if let Some(path) = line.strip_prefix("--- ") {
                    if path != "/dev/null" {
                        let path = match path.strip_prefix("a/") {
                            Some(v) => v,
                            None => return true,
                        };
                        if invalid_patch_path(path) {
                            return true;
                        }
                    }
                } else if let Some(path) = line.strip_prefix("+++ ") {
                    if path != "/dev/null" {
                        let path = match path.strip_prefix("b/") {
                            Some(v) => v,
                            None => return true,
                        };
                        if invalid_patch_path(path) {
                            return true;
                        }
                    }
                }
            }
            false
        }
        for p in &patches {
            if !p.contains("diff --git a/") || p.contains("..\\") || p.contains("../") || patch_has_invalid_paths(p) {
                return Some(json_response("400 Bad Request", r#"{"error":"invalid_patch_content"}"#));
            }
        }
        let mut applied = 0usize;
        let mut errors: Vec<String> = Vec::new();
        for (idx, patch) in patches.iter().enumerate() {
            let patch_file = root.join(format!(".akasha-hunk-{idx}.patch"));
            if let Err(e) = fs::write(&patch_file, patch) {
                errors.push(format!("write_patch_{idx}: {e}"));
                continue;
            }
            let argv = if dry_run {
                vec!["git".to_string(), "apply".to_string(), "--check".to_string(), patch_file.display().to_string()]
            } else {
                vec!["git".to_string(), "apply".to_string(), "--whitespace=nowarn".to_string(), patch_file.display().to_string()]
            };
            match studio_run_command_capture(&root, &argv, 90).await {
                Ok((Some(0), _, _)) => {
                    applied += 1;
                }
                Ok((code, out, err)) => {
                    errors.push(format!("patch_{idx}_failed code={code:?} stdout={out} stderr={err}"));
                }
                Err(e) => errors.push(format!("patch_{idx}_error: {e}")),
            }
            let _ = fs::remove_file(&patch_file);
        }
        let status = if errors.is_empty() { "200 OK" } else { "207 Multi-Status" };
        let body = serde_json::json!({
            "ok": errors.is_empty(),
            "dry_run": dry_run,
            "requested": patches.len(),
            "applied": applied,
            "errors": errors,
        })
        .to_string();
        return Some(json_response(status, &body));
    }

    // GET /api/studio/projects/:id/evolutions
    if method == "GET" && path_only.ends_with("/evolutions") {
        if let Some(rest) = strip_studio_projects_prefix(path_only) {
            let id = rest.strip_suffix("/evolutions").unwrap_or(rest);
            let root = match resolve_studio_project_dir(data_dir, id) {
                Ok(d) => d,
                Err(e) => {
                    return Some(json_response(
                        "400 Bad Request",
                        &serde_json::json!({ "error": e }).to_string(),
                    ));
                }
            };
            if !root.is_dir() {
                return Some(json_response("404 Not Found", r#"{"error":"project_not_found"}"#));
            }
            let meta = load_studio_meta(&root).unwrap_or(StudioMeta {
                id: id.to_string(),
                name: String::new(),
                created_at: String::new(),
                evolutions: Vec::new(),
                tech_stack: None,
                verify_skip: false,
                verify_argv: None,
                verify_timeout_sec: None,
                evolution_summary: None,
                policy_notes: None,
            });
            let body = serde_json::json!({ "evolutions": meta.evolutions }).to_string();
            return Some(json_response("200 OK", &body));
        }
    }

    // POST /api/studio/projects/:id/evolutions
    if method == "POST" && path_only.ends_with("/evolutions") && !path_only.contains("/evolutions/") {
        if let Some(rest) = strip_studio_projects_prefix(path_only) {
            let id = rest.strip_suffix("/evolutions").unwrap_or(rest);
            let root = match resolve_studio_project_dir(data_dir, id) {
                Ok(d) => d,
                Err(e) => {
                    return Some(json_response(
                        "400 Bad Request",
                        &serde_json::json!({ "error": e }).to_string(),
                    ));
                }
            };
            if !root.is_dir() {
                return Some(json_response("404 Not Found", r#"{"error":"project_not_found"}"#));
            }
            if !is_git_repo(&root).await {
                return Some(json_response(
                    "409 Conflict",
                    r#"{"error":"not_a_git_repository"}"#,
                ));
            }
            let body_v = match body.and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok()) {
                Some(v) => v,
                None => serde_json::json!({}),
            };
            let label = body_v
                .get("label")
                .and_then(|x| x.as_str())
                .map(|s| s.trim())
                .filter(|s| !s.is_empty());
            let evo_id = uuid::Uuid::new_v4().to_string();
            let slug: String = label
                .map(|s| {
                    s.chars()
                        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                        .take(40)
                        .collect::<String>()
                })
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| evo_id.chars().take(8).collect());
            let branch = format!("studio/{slug}");
            let _permit = studio_ops_semaphore().acquire().await.ok();
            if let Err(e) = ensure_main_or_master_branch(&root).await {
                return Some(json_response(
                    "500 Internal Server Error",
                    &serde_json::json!({ "error": e }).to_string(),
                ));
            }
            let o = git_output(&root, &["checkout", "-b", &branch]).await;
            match o {
                Ok(o) if o.status.success() => {}
                Ok(o) => {
                    let msg = String::from_utf8_lossy(&o.stderr);
                    return Some(json_response(
                        "500 Internal Server Error",
                        &serde_json::json!({ "error": format!("git checkout -b failed: {}", msg) }).to_string(),
                    ));
                }
                Err(e) => {
                    return Some(json_response(
                        "500 Internal Server Error",
                        &serde_json::json!({ "error": e }).to_string(),
                    ));
                }
            }
            let mut meta = load_studio_meta(&root).unwrap_or(StudioMeta {
                id: id.to_string(),
                name: String::new(),
                created_at: chrono::Utc::now().to_rfc3339(),
                evolutions: Vec::new(),
                tech_stack: None,
                verify_skip: false,
                verify_argv: None,
                verify_timeout_sec: None,
                evolution_summary: None,
                policy_notes: None,
            });
            meta.evolutions.push(StudioEvolution {
                id: evo_id.clone(),
                branch: branch.clone(),
                status: "open".to_string(),
                created_at: chrono::Utc::now().to_rfc3339(),
                root_task_id: None,
            });
            if let Err(e) = save_studio_meta(&root, &meta) {
                return Some(json_response(
                    "500 Internal Server Error",
                    &serde_json::json!({ "error": e }).to_string(),
                ));
            }
            let body = serde_json::json!({
                "evolution_id": evo_id,
                "branch": branch,
            })
            .to_string();
            return Some(json_response("201 Created", &body));
        }
    }

    // POST .../evolutions/:eid/merge
    if method == "POST" && path_only.contains("/evolutions/") && path_only.ends_with("/merge") {
        if let Some(rest) = strip_studio_projects_prefix(path_only) {
            // rest = :id/evolutions/:eid/merge
            let inner = rest.strip_suffix("/merge").unwrap_or(rest);
            let parts: Vec<&str> = inner.split('/').filter(|s| !s.is_empty()).collect();
            if parts.len() == 3 && parts[1] == "evolutions" {
                let proj_id = parts[0];
                let eid = parts[2];
                let root = match resolve_studio_project_dir(data_dir, proj_id) {
                    Ok(d) => d,
                    Err(e) => {
                        return Some(json_response(
                            "400 Bad Request",
                            &serde_json::json!({ "error": e }).to_string(),
                        ));
                    }
                };
                let mut meta = match load_studio_meta(&root) {
                    Some(m) => m,
                    None => {
                        return Some(json_response("404 Not Found", r#"{"error":"meta_not_found"}"#));
                    }
                };
                let branch = meta
                    .evolutions
                    .iter()
                    .find(|e| e.id == eid)
                    .map(|e| e.branch.clone());
                let Some(branch) = branch else {
                    return Some(json_response("404 Not Found", r#"{"error":"evolution_not_found"}"#));
                };
                if !is_git_repo(&root).await {
                    return Some(json_response(
                        "409 Conflict",
                        r#"{"error":"not_a_git_repository"}"#,
                    ));
                }
                let _permit = studio_ops_semaphore().acquire().await.ok();
                match ensure_evolution_branch_committed_before_merge(&root, &branch).await {
                    Ok(_) => {}
                    Err(e) => {
                        return Some(json_response(
                            "409 Conflict",
                            &serde_json::json!({ "error": "pre_merge_commit_failed", "detail": e }).to_string(),
                        ));
                    }
                }
                if ensure_main_or_master_branch(&root).await.is_err() {
                    return Some(json_response(
                        "500 Internal Server Error",
                        r#"{"error":"checkout_main_failed"}"#,
                    ));
                }
                let req = body.and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
                let design_check = req
                    .as_ref()
                    .and_then(|v| v.get("design_check").and_then(|x| x.as_bool()))
                    .unwrap_or(false);
                if design_check {
                    let base_raw = git_output(&root, &["show", "HEAD:DESIGN.md"])
                        .await
                        .ok()
                        .and_then(|o| {
                            if o.status.success() {
                                Some(String::from_utf8_lossy(&o.stdout).to_string())
                            } else {
                                None
                            }
                        })
                        .unwrap_or_default();
                    let branch_spec = format!("{branch}:DESIGN.md");
                    let branch_raw = git_output(&root, &["show", &branch_spec])
                        .await
                        .ok()
                        .and_then(|o| {
                            if o.status.success() {
                                Some(String::from_utf8_lossy(&o.stdout).to_string())
                            } else {
                                None
                            }
                        })
                        .unwrap_or_default();
                    let base_report = lint_design_doc(&base_raw);
                    let branch_report = lint_design_doc(&branch_raw);
                    let (base_errors, base_warnings) = extract_design_summary(&base_report);
                    let (branch_errors, branch_warnings) = extract_design_summary(&branch_report);
                    if branch_errors > base_errors || branch_warnings > base_warnings {
                        return Some(json_response(
                            "409 Conflict",
                            &json!({
                                "error":"design_regression",
                                "detail":"DESIGN.md lint worsened on evolution branch",
                                "base": base_report,
                                "branch": branch_report
                            })
                            .to_string(),
                        ));
                    }
                }
                let o = git_output(&root, &["merge", "--no-ff", &branch, "-m", "Akasha Code Studio: merge evolution"])
                    .await;
                match o {
                    Ok(o) if o.status.success() => {
                        for e in meta.evolutions.iter_mut() {
                            if e.id == eid {
                                e.status = "merged".to_string();
                            }
                        }
                        let _ = save_studio_meta(&root, &meta);
                        return Some(json_response("200 OK", r#"{"ok":true,"message":"merged"}"#));
                    }
                    Ok(o) => {
                        let msg = String::from_utf8_lossy(&o.stderr);
                        return Some(json_response(
                            "409 Conflict",
                            &serde_json::json!({ "error": "merge_failed", "detail": msg.to_string() }).to_string(),
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
        }
    }

    // POST /api/studio/projects/:id/design/validate
    if method == "POST" && path_only.ends_with("/design/validate") {
        if let Some(rest) = strip_studio_projects_prefix(path_only) {
            let id = rest
                .strip_suffix("/design/validate")
                .unwrap_or(rest)
                .trim_end_matches('/');
            let root = match resolve_studio_project_dir(data_dir, id) {
                Ok(d) => d,
                Err(e) => {
                    return Some(json_response(
                        "400 Bad Request",
                        &serde_json::json!({ "error": e }).to_string(),
                    ));
                }
            };
            if !root.is_dir() {
                return Some(json_response("404 Not Found", r#"{"error":"project_not_found"}"#));
            }
            let req = body.and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
            let raw = req
                .as_ref()
                .and_then(|v| v.get("content").and_then(|x| x.as_str()))
                .map(|s| s.to_string())
                .or_else(|| fs::read_to_string(root.join("DESIGN.md")).ok())
                .unwrap_or_default();
            let report = lint_design_doc(&raw);
            return Some(json_response("200 OK", &report.to_string()));
        }
    }

    // POST .../evolutions/:eid/abandon
    if method == "POST" && path_only.contains("/evolutions/") && path_only.ends_with("/abandon") {
        if let Some(rest) = strip_studio_projects_prefix(path_only) {
            let inner = rest.strip_suffix("/abandon").unwrap_or(rest);
            let parts: Vec<&str> = inner.split('/').filter(|s| !s.is_empty()).collect();
            if parts.len() == 3 && parts[1] == "evolutions" {
                let proj_id = parts[0];
                let eid = parts[2];
                let root = match resolve_studio_project_dir(data_dir, proj_id) {
                    Ok(d) => d,
                    Err(e) => {
                        return Some(json_response(
                            "400 Bad Request",
                            &serde_json::json!({ "error": e }).to_string(),
                        ));
                    }
                };
                let mut meta = match load_studio_meta(&root) {
                    Some(m) => m,
                    None => {
                        return Some(json_response("404 Not Found", r#"{"error":"meta_not_found"}"#));
                    }
                };
                let branch = meta.evolutions.iter().find(|e| e.id == eid).map(|e| e.branch.clone());
                let Some(branch) = branch else {
                    return Some(json_response("404 Not Found", r#"{"error":"evolution_not_found"}"#));
                };
                let _permit = studio_ops_semaphore().acquire().await.ok();
                let _ = ensure_main_or_master_branch(&root).await;
                let _ = git_output(&root, &["branch", "-D", &branch]).await;
                for e in meta.evolutions.iter_mut() {
                    if e.id == eid {
                        e.status = "abandoned".to_string();
                    }
                }
                let _ = save_studio_meta(&root, &meta);
                return Some(json_response("200 OK", r#"{"ok":true,"message":"abandoned"}"#));
            }
        }
    }

    None
}