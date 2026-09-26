//! Tool call execution (`execute_tool_call_impl`) extracted from `api.rs` (P3 / v0.11).

use crate::agents::OrchestratorTask;
use crate::api::{
    debug_log, guess_image_mime_from_path, is_workspace_virtual_path, load_budget_settings,
    normalize_tool_path_hint, parse_generate_image_tool_args, parse_memory_store_explicit_links,
    parse_plugin_tool_invocation, parse_search_replace_payload, parse_write_file_request,
    path_extension_is_pdf, path_for_agent_write_extension_check, pdf_extract_message_from_disk,
    resolve_tool_disk_path, strip_file_search_flags, strip_markdown_fences_from_write_content,
    strip_verbatim_prefix, vision_inject_max_chars, vision_payload_within_cap, BackgroundResultCell,
    BudgetSettings, ProcessRegistry, TaskWorkspaceStore, AVAILABLE_TOOLS,
};
use crate::api_path_utils::{
    normalize_apostrophes, parse_read_file_args, READ_FILE_DEFAULT_MAX_LINES,
    READ_FILE_FULL_OUTPUT_MAX_BYTES, READ_FILE_PARTIAL_DEFAULT_MARKER,
};
use crate::api_tool_dispatch::fs_ops::{
    rewrite_workspace_plan_key_to_lineage_root, rewrite_workspace_plan_path_str,
    sync_workspace_store_from_disk, workspace_lineage_root_task_id,
};
use crate::api_tool_dispatch::process_ops::{parse_run_command_args, resolve_run_command_working_dir};
use crate::api_tool_dispatch::web_device_memory_ops::parse_device_invoke_params;
use crate::memory_actor::LongTermMemoryClient;
use akasha_store::{Schedule, ScheduleStore, Task, TaskStatus, TaskStore, WorkspaceGraphStore};
use akasha_vault::Vault;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use uuid::Uuid;

pub(crate) async fn execute_tool_call_impl(
    executor: &std::sync::Arc<akasha_tools::ToolExecutor>,
    tool_name: &str,
    args: &[String],
    process_registry: Option<&ProcessRegistry>,
    long_term_client: Option<&LongTermMemoryClient>,
    task_id: Uuid,
    store_path: Option<&std::path::Path>,
    conv_tx: Option<mpsc::Sender<OrchestratorTask>>,
    message_webhook_url: Option<&str>,
    plugin_registry: Option<&std::sync::Arc<crate::plugins::PluginRegistry>>,
    device_bridge: Option<&std::sync::Arc<crate::device_bridge::DeviceBridge>>,
    workspace_store: Option<&TaskWorkspaceStore>,
    browser_registry: Option<&crate::browser::BrowserSessionRegistry>,
    workspace_root: Option<&std::path::Path>,
    session_id: Option<&str>,
) -> (bool, String, Option<String>) {
    let is_studio_workspace =
        crate::api_studio::studio_ticket_tool_workspace_root(store_path, workspace_root).is_some();
    let assigned_agent_for_task = if is_studio_workspace {
        store_path
            .and_then(|sp| TaskStore::open(sp).ok())
            .and_then(|store| store.get(task_id).ok().flatten())
            .map(|t| t.assigned_agent.to_ascii_lowercase())
    } else {
        None
    };
    if assigned_agent_for_task.as_deref() == Some("studio_reviewer") {
        let blocked = matches!(
            tool_name,
            "write_file"
                | "write_code"
                | "delete_file"
                | "rename_path"
                | "move_tree"
                | "edit_file"
                | "apply_patch"
                | "search_replace"
                | "delegate_to_agent"
        );
        if blocked {
            return (
                false,
                format!(
                    "[{}] forbidden for `studio_reviewer`: review agent is read-only and must only validate then update ticket feedback/status.",
                    tool_name
                ),
                None,
            );
        }
    }
    if tool_name.starts_with("mcp_") {
        if std::env::var("AKASHA_MCP_TOOLS_ENABLED").ok().as_deref() == Some("0") {
            return (
                false,
                "[mcp] MCP tools disabled (AKASHA_MCP_TOOLS_ENABLED=0)".to_string(),
                None,
            );
        }
        let Some((server, mcp_tool)) = crate::mcp_runtime::parse_mcp_tool_name(tool_name) else {
            return (
                false,
                format!("[{tool_name}] invalid MCP tool name (expected mcp_<server>_<tool>)"),
                None,
            );
        };
        if !executor.policy.can_use_mcp_tool(&server, &mcp_tool) {
            return (
                false,
                format!(
                    "[{tool_name}] MCP server/tool denied by tools_policy.yaml (mcp_servers)"
                ),
                None,
            );
        }
        if let Some(max) = executor.policy.mcp_max_calls_per_task_for(&server) {
            let used = crate::mcp_budget::count(task_id);
            if used >= max {
                return (
                    false,
                    format!(
                        "[{tool_name}] MCP budget exceeded ({used}/{max} calls this task)"
                    ),
                    None,
                );
            }
        }
        crate::mcp_budget::record_call(task_id);
        let args_json = if args.is_empty() {
            serde_json::json!({})
        } else {
            serde_json::json!({ "input": args.join(" ") })
        };
        return match crate::mcp_runtime::tools_call(&mcp_tool, args_json).await {
            Ok(v) => {
                let preview = v.to_string();
                let (p, total, trunc) = crate::tool_output::truncate_utf8_by_bytes(&preview, 12_000);
                let msg = crate::tool_output::with_truncation_footer(
                    format!("[{tool_name}] {p}"),
                    trunc,
                    total,
                    "MCP response truncated",
                );
                (true, msg, None)
            }
            Err(e) => (false, format!("[{tool_name}] MCP error: {e}"), None),
        };
    }
    let plugin_invocation = parse_plugin_tool_invocation(plugin_registry, tool_name, args);
    let is_plugin_candidate = plugin_invocation.is_some();
    let studio_ticket_tool_ok = matches!(
        tool_name,
        "studio_list_tickets" | "studio_create_ticket" | "studio_update_ticket"
    ) && crate::api_studio::studio_ticket_tool_workspace_root(store_path, workspace_root).is_some();
    let can_use_named_tool =
        executor.policy.can_use_tool(tool_name) || studio_ticket_tool_ok;
    let can_use_plugin_call =
        executor.policy.can_use_tool("plugin.call") || executor.policy.can_use_tool("plugin_call");
    if is_plugin_candidate && !can_use_plugin_call {
        return (
            false,
            "[plugin] tool not allowed by current profile (enable plugin.call)".to_string(),
            None,
        );
    }
    let is_mcp_tool = tool_name.starts_with("mcp_");
    if !can_use_named_tool && !is_plugin_candidate && !is_mcp_tool {
        return (
            false,
            format!("[{}] tool not allowed by current profile", tool_name),
            None,
        );
    }
    let path_arg = |i: usize| args.get(i).map(|s| Path::new(s.as_str()));
    // For tools that take a single path arg: rejoin args so paths with spaces (e.g. "Cas d'usage.pdf") work when the LLM splits them.
    let path_arg_joined = |args: &[String]| -> String {
        if args.is_empty() {
            String::new()
        } else {
            args.join(" ").trim().to_string()
        }
    };
    let result = match tool_name {
        "workspace_graph_search" => {
            let Some(sp) = store_path else {
                return (
                    false,
                    "[workspace_graph_search] no task store path".to_string(),
                    None,
                );
            };
            let sp = sp.to_path_buf();
            let mut ws_id: Option<String> = None;
            let mut rest: Vec<String> = Vec::new();
            let mut it = args.iter().peekable();
            while let Some(a) = it.next() {
                if a == "--workspace" {
                    ws_id = it
                        .next()
                        .cloned()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty());
                } else {
                    rest.push(a.clone());
                }
            }
            let query = rest.join(" ").trim().to_string();
            if query.is_empty() {
                (
                    false,
                    "[workspace_graph_search] usage: workspace_graph_search <query> [--workspace <uuid>]"
                        .to_string(),
                    None,
                )
            } else {
                let limit = 20usize;
                match tokio::task::spawn_blocking(move || {
                    let store = WorkspaceGraphStore::open(&sp)?;
                    store.search_graph_context(&query, limit, ws_id.as_deref())
                })
                .await
                {
                    Ok(Ok(lines)) => {
                        if lines.is_empty() {
                            (
                                true,
                                "[workspace_graph_search] no matching nodes".to_string(),
                                None,
                            )
                        } else {
                            (
                                true,
                                format!("[workspace_graph_search]\n{}", lines.join("\n")),
                                None,
                            )
                        }
                    }
                    Ok(Err(e)) => (false, format!("[workspace_graph_search] {}", e), None),
                    Err(e) => (
                        false,
                        format!("[workspace_graph_search] join: {}", e),
                        None,
                    ),
                }
            }
        }
        "user_rag_search" => {
            let Some(data_dir) = store_path.and_then(|p| p.parent()) else {
                return (
                    false,
                    "[user_rag_search] no data dir".to_string(),
                    None,
                );
            };
            let data_dir = data_dir.to_path_buf();
            let mut top_k = 5usize;
            let mut rest: Vec<String> = Vec::new();
            for a in args {
                if let Ok(k) = a.parse::<usize>() {
                    top_k = k.clamp(1, 20);
                } else {
                    rest.push(a.clone());
                }
            }
            let query = rest.join(" ").trim().to_string();
            if query.is_empty() {
                return (
                    false,
                    "[user_rag_search] usage: user_rag_search <query> [top_k]".to_string(),
                    None,
                );
            }
            match tokio::task::spawn_blocking(move || {
                let store = crate::user_rag::UserRagStore::new(&data_dir);
                store.retrieve(&query, top_k)
            })
            .await
            {
                Ok(Ok(chunks)) if chunks.is_empty() => (
                    true,
                    "[user_rag_search] no matching excerpts".to_string(),
                    None,
                ),
                Ok(Ok(chunks)) => (
                    true,
                    format!(
                        "[user_rag_search]\n{}",
                        chunks
                            .iter()
                            .enumerate()
                            .map(|(i, c)| format!("{}. {}", i + 1, c))
                            .collect::<Vec<_>>()
                            .join("\n\n")
                    ),
                    None,
                ),
                Ok(Err(e)) => (false, format!("[user_rag_search] {}", e), None),
                Err(e) => (false, format!("[user_rag_search] join: {}", e), None),
            }
        }
        "notes_list" => {
            let Some(data_dir) = store_path.and_then(|p| p.parent()) else {
                return (false, "[notes_list] no data dir".to_string(), None);
            };
            let data_dir = data_dir.to_path_buf();
            match tokio::task::spawn_blocking(move || {
                let store = crate::notes::NotesStore::new(&data_dir);
                store.list()
            })
            .await
            {
                Ok(Ok(notes)) if notes.is_empty() => (true, "[notes_list] no notes".to_string(), None),
                Ok(Ok(notes)) => {
                    let lines: Vec<String> = notes
                        .iter()
                        .map(|n| format!("{} — {}", n.id, n.title))
                        .collect();
                    (true, format!("[notes_list]\n{}", lines.join("\n")), None)
                }
                Ok(Err(e)) => (false, format!("[notes_list] {}", e), None),
                Err(e) => (false, format!("[notes_list] join: {}", e), None),
            }
        }
        "notes_read" => {
            let Some(data_dir) = store_path.and_then(|p| p.parent()) else {
                return (false, "[notes_read] no data dir".to_string(), None);
            };
            let id = args.first().map(String::as_str).unwrap_or("").trim();
            if id.is_empty() {
                return (
                    false,
                    "[notes_read] usage: notes_read <id>".to_string(),
                    None,
                );
            }
            let data_dir = data_dir.to_path_buf();
            let id = id.to_string();
            let id_for_err = id.clone();
            match tokio::task::spawn_blocking(move || {
                let store = crate::notes::NotesStore::new(&data_dir);
                store.read(&id)
            })
            .await
            {
                Ok(Ok(Some(doc))) => (
                    true,
                    format!(
                        "[notes_read] {} — {}\n\n{}",
                        doc.meta.id, doc.meta.title, doc.content
                    ),
                    None,
                ),
                Ok(Ok(None)) => (false, format!("[notes_read] note not found: {id_for_err}"), None),
                Ok(Err(e)) => (false, format!("[notes_read] {}", e), None),
                Err(e) => (false, format!("[notes_read] join: {}", e), None),
            }
        }
        "notes_write" => {
            let Some(data_dir) = store_path.and_then(|p| p.parent()) else {
                return (false, "[notes_write] no data dir".to_string(), None);
            };
            let id = args.first().map(String::as_str).unwrap_or("").trim();
            if id.is_empty() {
                return (
                    false,
                    "[notes_write] usage: notes_write <id> puis contenu markdown".to_string(),
                    None,
                );
            }
            let content = if args.len() > 1 {
                args[1..].join("\n")
            } else {
                String::new()
            };
            let data_dir = data_dir.to_path_buf();
            let id = id.to_string();
            let id_for_err = id.clone();
            match tokio::task::spawn_blocking(move || {
                let store = crate::notes::NotesStore::new(&data_dir);
                store.update(&id, None, Some(&content))
            })
            .await
            {
                Ok(Ok(Some(doc))) => (
                    true,
                    format!(
                        "[notes_write] updated {} — {} ({} chars)",
                        doc.meta.id,
                        doc.meta.title,
                        doc.content.len()
                    ),
                    None,
                ),
                Ok(Ok(None)) => (false, format!("[notes_write] note not found: {id_for_err}"), None),
                Ok(Err(e)) => (false, format!("[notes_write] {}", e), None),
                Err(e) => (false, format!("[notes_write] join: {}", e), None),
            }
        }
        "notes_search" => {
            let Some(data_dir) = store_path.and_then(|p| p.parent()) else {
                return (false, "[notes_search] no data dir".to_string(), None);
            };
            let mut limit = 10usize;
            let mut rest: Vec<String> = Vec::new();
            for a in args {
                if let Ok(k) = a.parse::<usize>() {
                    limit = k.clamp(1, 50);
                } else {
                    rest.push(a.clone());
                }
            }
            let query = rest.join(" ").trim().to_string();
            if query.is_empty() {
                return (
                    false,
                    "[notes_search] usage: notes_search <query> [limit]".to_string(),
                    None,
                );
            }
            let data_dir = data_dir.to_path_buf();
            match tokio::task::spawn_blocking(move || {
                let store = crate::notes::NotesStore::new(&data_dir);
                store.search(&query, limit)
            })
            .await
            {
                Ok(Ok(hits)) if hits.is_empty() => (
                    true,
                    "[notes_search] no matching notes".to_string(),
                    None,
                ),
                Ok(Ok(hits)) => (
                    true,
                    format!(
                        "[notes_search]\n{}",
                        hits.iter()
                            .enumerate()
                            .map(|(i, h)| format!(
                                "{}. {} — {}\n   {}",
                                i + 1,
                                h.id,
                                h.title,
                                h.snippet
                            ))
                            .collect::<Vec<_>>()
                            .join("\n")
                    ),
                    None,
                ),
                Ok(Err(e)) => (false, format!("[notes_search] {}", e), None),
                Err(e) => (false, format!("[notes_search] join: {}", e), None),
            }
        }
        "studio_list_tickets" => {
            let Some(root) =
                crate::api_studio::studio_ticket_tool_workspace_root(store_path, workspace_root)
            else {
                return (
                    false,
                    "[studio_list_tickets] Code Studio project workspace required".to_string(),
                    None,
                );
            };
            let tickets = crate::api_studio::studio_list_tickets(&root);
            let slim: Vec<serde_json::Value> = tickets
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "id": t.id,
                        "title": t.title,
                        "status": t.status,
                        "depends_on_ticket_ids": crate::api_studio::ticket_dependency_ids(t),
                        "assigned_agent": t.assigned_agent,
                        "review_agent": t.review_agent,
                    })
                })
                .collect();
            let pretty = serde_json::to_string_pretty(&slim).unwrap_or_else(|_| "[]".to_string());
            (
                true,
                format!("[studio_list_tickets]\n{}", pretty),
                None,
            )
        }
        "studio_create_ticket" => {
            let Some(root) =
                crate::api_studio::studio_ticket_tool_workspace_root(store_path, workspace_root)
            else {
                return (
                    false,
                    "[studio_create_ticket] Code Studio project workspace required".to_string(),
                    None,
                );
            };
            let payload = args.join(" ");
            let v: serde_json::Value = match serde_json::from_str(payload.trim()) {
                Ok(x) => x,
                Err(e) => {
                    return (
                        false,
                        format!(
                            "[studio_create_ticket] invalid JSON (single JSON object as args): {}",
                            e
                        ),
                        None,
                    );
                }
            };
            match crate::api_studio::studio_tool_create_ticket_json(&root, &v) {
                Ok(ticket) => {
                    let enc = serde_json::to_string_pretty(&ticket).unwrap_or_default();
                    (true, format!("[studio_create_ticket]\n{}", enc), None)
                }
                Err(e) => (false, format!("[studio_create_ticket] {}", e), None),
            }
        }
        "studio_update_ticket" => {
            let Some(root) =
                crate::api_studio::studio_ticket_tool_workspace_root(store_path, workspace_root)
            else {
                return (
                    false,
                    "[studio_update_ticket] Code Studio project workspace required".to_string(),
                    None,
                );
            };
            let payload = args.join(" ");
            let v: serde_json::Value = match serde_json::from_str(payload.trim()) {
                Ok(x) => x,
                Err(e) => {
                    return (
                        false,
                        format!(
                            "[studio_update_ticket] invalid JSON (single JSON object as args): {}",
                            e
                        ),
                        None,
                    );
                }
            };
            match crate::api_studio::studio_tool_apply_ticket_patch_json(&root, &v) {
                Ok(ticket) => {
                    let enc = serde_json::to_string_pretty(&ticket).unwrap_or_default();
                    (true, format!("[studio_update_ticket]\n{}", enc), None)
                }
                Err(e) => (false, format!("[studio_update_ticket] {}", e), None),
            }
        }
        "read_file" => {
            let (path_tokens, explicit_window, want_full) = parse_read_file_args(args);
            let path_input = path_tokens.join(" ");
            let path_str = normalize_tool_path_hint(&path_input);
            let slice_for_line_window = |content: &str, path_label: &str, offset: usize, limit: usize| -> String {
                let lines: Vec<&str> = content.lines().collect();
                let total = lines.len();
                if total == 0 {
                    return format!("[read_file {}] file is empty", path_label);
                }
                let req_start_1 = if offset == 0 { 1 } else { offset };
                let req_start_idx = req_start_1.saturating_sub(1);
                let start_idx = if req_start_idx >= total {
                    total.saturating_sub(limit)
                } else {
                    req_start_idx
                };
                let end_idx_excl = (start_idx + limit).min(total);
                let body = if start_idx < end_idx_excl {
                    lines[start_idx..end_idx_excl].join("\n")
                } else {
                    String::new()
                };
                let actual_start_1 = start_idx + 1;
                let actual_end_1 = end_idx_excl;
                let req_end_1 = req_start_1.saturating_add(limit.saturating_sub(1));
                if actual_start_1 != req_start_1 || actual_end_1 != req_end_1 {
                    format!(
                        "[read_file {}] requested lines {}..{}; returned lines {}..{} (file has {} lines):\n{}",
                        path_label, req_start_1, req_end_1, actual_start_1, actual_end_1, total, body
                    )
                } else {
                    format!(
                        "[read_file {}] lines {}..{} (file has {} lines):\n{}",
                        path_label, actual_start_1, actual_end_1, total, body
                    )
                }
            };
            let format_read_content = |content: &str, path_label: &str| -> String {
                if want_full {
                    let line_count = content.lines().count();
                    let total_bytes = content.len();
                    if total_bytes <= READ_FILE_FULL_OUTPUT_MAX_BYTES {
                        format!(
                            "[read_file {}] full file ({} lines, {} bytes):\n{}",
                            path_label, line_count, total_bytes, content
                        )
                    } else {
                        let (frag, total, truncated) = crate::tool_output::truncate_utf8_by_bytes(
                            content,
                            READ_FILE_FULL_OUTPUT_MAX_BYTES,
                        );
                        let frag_s = frag.into_owned();
                        let mut body = format!(
                            "[read_file {}] --full: first {} UTF-8 bytes shown ({} lines in file, {} bytes total):\n{}",
                            path_label,
                            frag_s.len(),
                            line_count,
                            total,
                            frag_s
                        );
                        if truncated {
                            body.push('\n');
                            body.push_str(&crate::tool_output::truncation_footer_bytes(
                                total,
                                "narrow with grep_content/search_files or read_file with a line window",
                            ));
                        }
                        body
                    }
                } else {
                    let (off, lim) = explicit_window.unwrap_or((1, READ_FILE_DEFAULT_MAX_LINES));
                    let total_lines = content.lines().count();
                    let mut out = slice_for_line_window(content, path_label, off, lim);
                    if explicit_window.is_none() && total_lines > READ_FILE_DEFAULT_MAX_LINES {
                        out.push_str(&format!(
                            "\n{} — {} lines total. Use TOOL: read_file <same_path> --full for entire file, or TOOL: read_file <same_path> <offset> <limit> for a chunk.",
                            READ_FILE_PARTIAL_DEFAULT_MARKER, total_lines
                        ));
                    }
                    out
                }
            };
            if path_str.is_empty() {
                (
                    false,
                    "[read_file] usage: read_file <path> [--full] [<offset_line> <limit_lines>]".to_string(),
                    None,
                )
            } else if is_workspace_virtual_path(&path_str) {
                let key = path_str
                    .trim_start_matches("workspace:/")
                    .trim_start_matches("workspace:")
                    .trim_start_matches('/')
                    .to_string();
                let key = normalize_apostrophes(&key);
                let lineage_id = workspace_lineage_root_task_id(task_id, store_path);
                let (key, _) = rewrite_workspace_plan_key_to_lineage_root(&key, lineage_id);
                if let Some(ws) = workspace_store {
                    let guard = ws.read().await;
                    // Prefer lineage root (where write_file stores); fall back to this task id.
                    let mem_map = guard
                        .get(&lineage_id)
                        .or_else(|| guard.get(&task_id));
                    if let Some(map) = mem_map {
                        if let Some(content) = map.get(&key) {
                            // Empty in-memory entry must not mask the real file on disk (orchestrator
                            // writes `.akasha/plan_*.md` with tokio::fs, not via this map).
                            if !content.is_empty() {
                                return (
                                    true,
                                    format_read_content(content, &format!("workspace:{}", key)),
                                    None,
                                );
                            }
                        }
                    }
                } else {
                    return (false, "[read_file] workspace paths require a workspace store.".to_string(), None);
                }
                let disk_path = strip_verbatim_prefix(
                    workspace_root
                        .map(|root| root.join(&key))
                        .or_else(|| std::env::current_dir().ok().map(|cwd| cwd.join(&key)))
                        .unwrap_or_else(|| Path::new(&key).to_path_buf()),
                );
                if !executor.policy.can_read(&disk_path) {
                    return (false, format!("[read_file workspace] path not allowed: {} (allowed_read_paths)", key), None);
                }
                if path_extension_is_pdf(&disk_path) {
                    return pdf_extract_message_from_disk(&disk_path, "read_file").await;
                }
                match executor.read_file(&disk_path).await {
                    Ok((content, res)) => {
                        let msg = if res.success {
                            format_read_content(&content, &disk_path.display().to_string())
                        } else {
                            format!("[read_file workspace] failed: {}", res.summary)
                        };
                        return (res.success, msg, None);
                    }
                    Err(e) => {
                        let detail = format!("{:#}", e);
                        let hint = if detail.to_ascii_lowercase().contains("not found")
                            || detail.to_ascii_lowercase().contains("cannot find")
                            || detail.to_ascii_lowercase().contains("no such file")
                        {
                            " (hint: verify path/casing and use search_files . <filename> first)"
                        } else {
                            ""
                        };
                        return (
                            false,
                            format!(
                                "[read_file workspace] failed: read error for {} at {}: {}{}",
                                key,
                                disk_path.display(),
                                detail,
                                hint
                            ),
                            None,
                        );
                    }
                };
            } else {
                let p = Path::new(&path_str);
                if path_extension_is_pdf(p) && executor.policy.can_read(p) {
                    pdf_extract_message_from_disk(p, "read_file").await
                } else {
                    match executor.read_file(p).await {
                        Ok((content, res)) => {
                            let msg = if res.success {
                                format_read_content(&content, &p.display().to_string())
                            } else {
                                format!("[read_file] failed: {}", res.summary)
                            };
                            (res.success, msg, None)
                        }
                        Err(e) => {
                            let detail = format!("{:#}", e);
                            let hint = if detail.to_ascii_lowercase().contains("not found")
                                || detail.to_ascii_lowercase().contains("cannot find")
                                || detail.to_ascii_lowercase().contains("no such file")
                            {
                                " (hint: verify path/casing and use search_files . <filename> first)"
                            } else {
                                ""
                            };
                            (
                                false,
                                format!("[read_file] failed: {}{}", detail, hint),
                                None,
                            )
                        }
                    }
                }
            }
        }
        "run_command" => {
            let (vault_specs, cwd_flag, cmd, cmd_args) = parse_run_command_args(args);
            let cwd_resolved = match resolve_run_command_working_dir(cwd_flag.as_deref(), workspace_root, &executor.policy) {
                Ok(c) => c,
                Err(e) => return (false, format!("[run_command] {}", e), None),
            };
            let cwd_ref = cwd_resolved.as_deref();
            let extra_env = if vault_specs.is_empty() {
                None
            } else {
                let data_dir = store_path.and_then(|p| p.parent());
                match data_dir.and_then(|d| akasha_vault::open_vault(d).ok()) {
                    Some(vault) => {
                        let mut env = Vec::new();
                        for (vault_key, env_var) in &vault_specs {
                            let value = vault.get(vault_key).or_else(|_| {
                                // Fallback: common keys may be stored with different casing (e.g. github_token vs GITHUB_TOKEN)
                                if vault_key.eq_ignore_ascii_case("GITHUB_TOKEN") && vault_key != "github_token" {
                                    vault.get("github_token")
                                } else if vault_key == "github_token" {
                                    vault.get("GITHUB_TOKEN")
                                } else {
                                    Err(akasha_vault::VaultError::NotFound(vault_key.to_string()))
                                }
                            });
                            match value {
                                Ok(v) => env.push((env_var.clone(), v)),
                                Err(_) => {
                                    return (
                                        false,
                                        format!("[run_command] vault key not found: {}", vault_key),
                                        None,
                                    );
                                }
                            }
                        }
                        Some(env)
                    }
                    None => {
                        return (
                            false,
                            "[run_command] vault not available (no store_path or open failed)".to_string(),
                            None,
                        );
                    }
                }
            };
            let env_ref = extra_env.as_deref();
            match executor.run_command(&cmd, &cmd_args, cwd_ref, env_ref).await {
                Ok((out, res)) => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    let cwd_note = cwd_ref
                        .map(|p| format!(" cwd={}", p.display()))
                        .unwrap_or_default();
                    let exit = out
                        .status
                        .code()
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "?".to_string());
                    let msg = if res.success {
                        crate::tool_output::shell_tool_success(
                            "run_command",
                            &cmd,
                            &cwd_note,
                            &exit,
                            stdout.as_ref(),
                            stderr.as_ref(),
                        )
                    } else {
                        crate::tool_output::shell_tool_failure(
                            "run_command",
                            &res.summary,
                            &exit,
                            stdout.as_ref(),
                            stderr.as_ref(),
                        )
                    };
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[run_command] failed: {}", e), None),
            }
        }
        "run_terminal" => {
            let (_vault_specs, cwd_flag, cmd, cmd_args) = parse_run_command_args(args);
            let cwd_resolved = match resolve_run_command_working_dir(cwd_flag.as_deref(), workspace_root, &executor.policy) {
                Ok(c) => c,
                Err(e) => return (false, format!("[run_terminal] {}", e), None),
            };
            let cwd_ref = cwd_resolved.as_deref();
            match executor.run_command(&cmd, &cmd_args, cwd_ref, None).await {
                Ok((out, res)) => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    let cwd_note = cwd_ref
                        .map(|p| format!(" cwd={}", p.display()))
                        .unwrap_or_default();
                    let exit = out
                        .status
                        .code()
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "?".to_string());
                    let msg = if res.success {
                        crate::tool_output::shell_tool_success(
                            "run_terminal",
                            &cmd,
                            &cwd_note,
                            &exit,
                            stdout.as_ref(),
                            stderr.as_ref(),
                        )
                    } else {
                        crate::tool_output::shell_tool_failure(
                            "run_terminal",
                            &res.summary,
                            &exit,
                            stdout.as_ref(),
                            stderr.as_ref(),
                        )
                    };
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[run_terminal] failed: {}", e), None),
            }
        }
        "run_command_background" => {
            let (_vault_specs, cwd_flag, cmd, cmd_args) = parse_run_command_args(args);
            let cwd_resolved = match resolve_run_command_working_dir(cwd_flag.as_deref(), workspace_root, &executor.policy) {
                Ok(c) => c,
                Err(e) => return (false, format!("[run_command_background] {}", e), None),
            };
            let cmd_display = format!("{} {}", cmd, cmd_args.join(" "));
            match process_registry {
                Some(reg) => {
                    let session_id = Uuid::new_v4();
                    let exec = executor.clone();
                    let cell: BackgroundResultCell = Arc::new(RwLock::new(None));
                    let cell_clone = cell.clone();
                    let reg_clone = reg.clone();
                    let task = tokio::spawn(async move {
                        let cwd_ref = cwd_resolved.as_deref();
                        let result = exec.run_command(&cmd, &cmd_args, cwd_ref, None).await;
                        let (success, exit_code) = match &result {
                            Ok((out, res)) => (res.success, out.status.code()),
                            Err(_) => (false, None),
                        };
                        *cell_clone.write().await = Some(result);
                        crate::process_watch::push_event(crate::process_watch::ProcessWatchEvent {
                            ts_rfc3339: chrono::Utc::now().to_rfc3339(),
                            session_id: session_id.to_string(),
                            cmd_line: format!("{} {}", cmd, cmd_args.join(" ")),
                            success,
                            exit_code,
                        })
                        .await;
                        // Auto-cleanup after a TTL to prevent leaking sessions the client never polls.
                        tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                        reg_clone.write().await.remove(&session_id);
                    });
                    reg.write().await.insert(session_id, (task, cell));
                    (true, format!("[run_command_background] session_id: {} (cmd: {})", session_id, cmd_display), None)
                }
                None => (false, "[run_command_background] process registry not available".to_string(), None),
            }
        }
        "terminal_session" => {
            let sub = args.get(0).map(String::as_str).unwrap_or("");
            match sub {
                "list" => {
                    match tokio::task::spawn_blocking(move || crate::terminal_pty::PtyManager::global().list_sessions()).await {
                        Ok(Ok(v)) => (true, serde_json::to_string_pretty(&v).unwrap_or_else(|_| "[]".to_string()), None),
                        Ok(Err(e)) => (false, format!("[terminal_session list] {}", e), None),
                        Err(e) => (false, format!("[terminal_session list] join: {}", e), None),
                    }
                }
                "start" => {
                    let create = crate::terminal_pty::PtyCreateBody {
                        argv: None,
                        cwd: None,
                        transcript_name: Some(format!("session-{}", uuid::Uuid::new_v4())),
                        idle_timeout_secs: Some(900),
                        cols: 80,
                        rows: 24,
                    };
                    match tokio::task::spawn_blocking(move || crate::terminal_pty::PtyManager::global().create(create)).await {
                        Ok(Ok(v)) => (
                            true,
                            format!(
                                "[terminal_session start] session_id={} idle_timeout_secs={} (use terminal_session read|write|resize|stop)",
                                v.session_id, v.idle_timeout_secs
                            ),
                            None
                        ),
                        Ok(Err(e)) => (false, format!("[terminal_session start] {}", e), None),
                        Err(e) => (false, format!("[terminal_session start] join: {}", e), None),
                    }
                }
                "read" => {
                    let sid = args.get(1).cloned().unwrap_or_default();
                    if sid.trim().is_empty() {
                        return (false, "[terminal_session read] usage: terminal_session read <session_id> [max]".to_string(), None);
                    }
                    let max = args.get(2).and_then(|x| x.parse::<usize>().ok()).unwrap_or(4096);
                    match tokio::task::spawn_blocking(move || crate::terminal_pty::PtyManager::global().read_output(&sid, max)).await {
                        Ok(Ok(v)) => (true, serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string()), None),
                        Ok(Err(e)) => (false, format!("[terminal_session read] {}", e), None),
                        Err(e) => (false, format!("[terminal_session read] join: {}", e), None),
                    }
                }
                "write" => {
                    let sid = args.get(1).cloned().unwrap_or_default();
                    let payload = args.get(2..).unwrap_or(&[]).join(" ");
                    if sid.trim().is_empty() || payload.is_empty() {
                        return (false, "[terminal_session write] usage: terminal_session write <session_id> <text...>".to_string(), None);
                    }
                    let body = crate::terminal_pty::PtyInputBody { text: Some(payload), bytes_b64: None };
                    match tokio::task::spawn_blocking(move || crate::terminal_pty::PtyManager::global().write_input(&sid, body)).await {
                        Ok(Ok(_)) => (true, "[terminal_session write] ok".to_string(), None),
                        Ok(Err(e)) => (false, format!("[terminal_session write] {}", e), None),
                        Err(e) => (false, format!("[terminal_session write] join: {}", e), None),
                    }
                }
                "resize" => {
                    let sid = args.get(1).cloned().unwrap_or_default();
                    let cols = args.get(2).and_then(|x| x.parse::<u16>().ok()).unwrap_or(80);
                    let rows = args.get(3).and_then(|x| x.parse::<u16>().ok()).unwrap_or(24);
                    if sid.trim().is_empty() {
                        return (false, "[terminal_session resize] usage: terminal_session resize <session_id> <cols> <rows>".to_string(), None);
                    }
                    let body = crate::terminal_pty::PtyResizeBody { cols, rows };
                    match tokio::task::spawn_blocking(move || crate::terminal_pty::PtyManager::global().resize(&sid, body)).await {
                        Ok(Ok(_)) => (true, "[terminal_session resize] ok".to_string(), None),
                        Ok(Err(e)) => (false, format!("[terminal_session resize] {}", e), None),
                        Err(e) => (false, format!("[terminal_session resize] join: {}", e), None),
                    }
                }
                "stop" => {
                    let sid = args.get(1).cloned().unwrap_or_default();
                    if sid.trim().is_empty() {
                        return (false, "[terminal_session stop] usage: terminal_session stop <session_id>".to_string(), None);
                    }
                    match tokio::task::spawn_blocking(move || crate::terminal_pty::PtyManager::global().close(&sid)).await {
                        Ok(Ok(_)) => (true, "[terminal_session stop] ok".to_string(), None),
                        Ok(Err(e)) => (false, format!("[terminal_session stop] {}", e), None),
                        Err(e) => (false, format!("[terminal_session stop] join: {}", e), None),
                    }
                }
                _ => (true, "[terminal_session] usage: terminal_session start|list|read|write|resize|stop. HTTP API: GET /api/terminal/capabilities.".to_string(), None),
            }
        },
        "process" => {
            let sub = args.get(0).map(String::as_str).unwrap_or("");
            match (process_registry, sub) {
                (Some(reg), "list") => {
                    let ids: Vec<String> = reg.read().await.keys().map(|u| u.to_string()).collect();
                    (true, format!("[process list] {} session(s): {:?}", ids.len(), ids), None)
                }
                (Some(reg), "poll") => {
                    let session_id = args.get(1).and_then(|s| Uuid::parse_str(s).ok());
                    match session_id {
                        Some(id) => {
                            let cell_opt = {
                                let g = reg.read().await;
                                g.get(&id).map(|(_task, cell)| cell.clone())
                            };
                            let Some(cell) = cell_opt else {
                                return (false, format!("[process poll] unknown session_id: {}", id), None);
                            };
                            let result_opt = cell.write().await.take();
                            match result_opt {
                                Some(Ok((out, res))) => {
                                    reg.write().await.remove(&id);
                                    let stdout = String::from_utf8_lossy(&out.stdout);
                                    let stderr = String::from_utf8_lossy(&out.stderr);
                                    let exit = out
                                        .status
                                        .code()
                                        .map(|c| c.to_string())
                                        .unwrap_or_else(|| "?".to_string());
                                    let (so, se, trunc, note) = crate::tool_output::format_truncated_streams(
                                        stdout.as_ref(),
                                        stderr.as_ref(),
                                        crate::tool_output::RUN_COMMAND_STDOUT_MAX,
                                        crate::tool_output::RUN_COMMAND_STDERR_MAX,
                                    );
                                    let mut base_msg = format!(
                                        "[process poll {}] done — exit {} stdout: {} stderr: {}",
                                        id, exit, so, se
                                    );
                                    if trunc {
                                        base_msg.push('\n');
                                        base_msg.push_str(&note);
                                    }
                                    if res.success {
                                        (true, base_msg, None)
                                    } else {
                                        (
                                            false,
                                            format!("{} | summary: {}", base_msg, res.summary),
                                            None,
                                        )
                                    }
                                }
                                Some(Err(e)) => {
                                    reg.write().await.remove(&id);
                                    (false, format!("[process poll {}] error: {}", id, e), None)
                                }
                                None => (true, format!("[process poll {}] still running", id), None),
                            }
                        }
                        None => (false, "[process poll] usage: process poll <session_id>".to_string(), None),
                    }
                }
                (Some(reg), "kill") => {
                    let session_id = args.get(1).and_then(|s| Uuid::parse_str(s).ok());
                    match session_id {
                        Some(id) => {
                            let mut g = reg.write().await;
                            if let Some((task, _cell)) = g.remove(&id) {
                                task.abort();
                                (true, format!("[process kill {}] aborted", id), None)
                            } else {
                                (false, format!("[process kill] unknown session_id: {}", id), None)
                            }
                        }
                        None => (false, "[process kill] usage: process kill <session_id>".to_string(), None),
                    }
                }
                (_, _) => (false, "[process] usage: process list | process poll <session_id> | process kill <session_id>".to_string(), None),
            }
        }
        "memory_search" => {
            let query_str = args.get(0).map(|a| a.as_str()).unwrap_or("").trim();
            let top_k = args.get(1).and_then(|s| s.parse::<usize>().ok()).unwrap_or(5).min(20);
            if query_str.is_empty() {
                return (false, "[memory_search] usage: memory_search <query> [top_k]".to_string(), None);
            }
            match long_term_client {
                Some(client) => {
                    let client = client.clone();
                    let query = query_str.to_string();
                    let results = tokio::task::spawn_blocking(move || client.search(query, top_k, None))
                        .await
                        .ok()
                        .unwrap_or_default();
                    if results.is_empty() {
                        (true, format!("[memory_search] no results for \"{}\"", query_str), None)
                    } else {
                        let preview: Vec<String> = results.iter().take(5).map(|(id, content)| format!("id: {} — {}", id, content.replace('\n', " "))).collect();
                        (true, format!("[memory_search] {} result(s): {}", results.len(), preview.join(" | ")), None)
                    }
                }
                None => (false, "[memory_search] long-term memory not available".to_string(), None),
            }
        }
        "memory_store" => {
            let content = args.get(0).map(|a| a.as_str()).unwrap_or("");
            let source = args.get(1).map(|a| a.as_str()).unwrap_or("agent");
            if content.is_empty() {
                return (false, "[memory_store] usage: memory_store <content> <source> [link_to: uuid+kind,...|uuid,...] [link_kind: default_for_plain_uuids]".to_string(), None);
            }
            let explicit_links = parse_memory_store_explicit_links(args.get(2..).unwrap_or(&[]));
            match long_term_client {
                Some(client) => {
                    let client = client.clone();
                    let content = content.to_string();
                    let source = source.to_string();
                    let out = tokio::task::spawn_blocking(move || client.promote(content, source, None, None, None, None, None, None, explicit_links))
                        .await
                        .ok()
                        .and_then(|r| r.ok());
                    match out {
                        Some(Some(id)) => (true, format!("[memory_store] stored id={id}"), None),
                        Some(None) => (true, "[memory_store] stored (duplicate skipped)".to_string(), None),
                        None => (false, "[memory_store] failed or memory not available".to_string(), None),
                    }
                }
                None => (false, "[memory_store] long-term memory not available".to_string(), None),
            }
        }
        "memory_delete" => {
            let id = args.get(0).map(|a| a.as_str()).unwrap_or("").trim();
            if id.is_empty() {
                return (false, "[memory_delete] usage: memory_delete <id> (UUID de l'entrée)".to_string(), None);
            }
            match long_term_client {
                Some(client) => {
                    let client = client.clone();
                    let id = id.to_string();
                    let out = tokio::task::spawn_blocking(move || client.delete(id))
                        .await
                        .ok()
                        .and_then(|r| r.ok());
                    match out {
                        Some(()) => (true, "[memory_delete] deleted".to_string(), None),
                        None => (false, "[memory_delete] failed or not found (vérifiez l'id)".to_string(), None),
                    }
                }
                None => (false, "[memory_delete] long-term memory not available".to_string(), None),
            }
        }
        "memory_update" => {
            let id = args.get(0).map(|a| a.as_str()).unwrap_or("").trim();
            let content = args.get(1..).map(|a| a.join(" ")).unwrap_or_default();
            if id.is_empty() || content.is_empty() {
                return (false, "[memory_update] usage: memory_update <id> <new_content>".to_string(), None);
            }
            match long_term_client {
                Some(client) => {
                    let client = client.clone();
                    let emit_client = client.clone();
                    let id = id.to_string();
                    let id_for_emit = id.clone();
                    let content = content.trim().to_string();
                    let out = tokio::task::spawn_blocking(move || client.update(id, content))
                        .await
                        .ok()
                        .and_then(|r| r.ok());
                    match out {
                        Some(()) => {
                            let _ = emit_client.emit_event(
                                "memory_updated".to_string(),
                                format!("{{\"id\":\"{id_for_emit}\"}}"),
                                None,
                                None,
                                None,
                                None,
                                Some(2),
                                Some("global_user".to_string()),
                                Some("memory_update".to_string()),
                            );
                            (true, "[memory_update] updated".to_string(), None)
                        }
                        None => (false, "[memory_update] failed or not found".to_string(), None),
                    }
                }
                None => (false, "[memory_update] long-term memory not available".to_string(), None),
            }
        }
        "memory_forget" => {
            let query = args.get(0).map(|a| a.as_str()).unwrap_or("").trim();
            if query.is_empty() {
                return (false, "[memory_forget] usage: memory_forget <query> (mots-clés)".to_string(), None);
            }
            match long_term_client {
                Some(client) => {
                    let client = client.clone();
                    let q = query.to_string();
                    let out = tokio::task::spawn_blocking(move || client.forget_by_query(q)).await.ok().and_then(|r| r.ok());
                    match out {
                        Some(n) => (true, format!("[memory_forget] {} entrée(s) supprimée(s)", n), None),
                        None => (false, "[memory_forget] failed or long-term memory not available".to_string(), None),
                    }
                }
                None => (false, "[memory_forget] long-term memory not available".to_string(), None),
            }
        }
        "memory_stats" => {
            match long_term_client {
                Some(client) => {
                    let client = client.clone();
                    let out = tokio::task::spawn_blocking(move || client.stats()).await.ok().and_then(|r| r.ok());
                    match out {
                        Some((count, size)) => (true, format!("[memory_stats] {} entrée(s), ~{} octets", count, size), None),
                        None => (false, "[memory_stats] failed or long-term memory not available".to_string(), None),
                    }
                }
                None => (false, "[memory_stats] long-term memory not available".to_string(), None),
            }
        }
        "memory_gc" => {
            let retention_days = args.get(0).and_then(|s| s.parse::<u32>().ok()).unwrap_or(90);
            let protect_sources: Vec<String> = args.iter().skip(1).map(|a| a.as_str().trim().to_string()).filter(|s| !s.is_empty()).collect();
            let protect = if protect_sources.is_empty() { None } else { Some(protect_sources) };
            match long_term_client {
                Some(client) => {
                    let client = client.clone();
                    let out = tokio::task::spawn_blocking(move || client.gc(retention_days, protect)).await.ok().and_then(|r| r.ok());
                    match out {
                        Some(n) => (true, format!("[memory_gc] {} entrée(s) supprimée(s) (rétention {} j)", n, retention_days), None),
                        None => (false, "[memory_gc] failed or long-term memory not available".to_string(), None),
                    }
                }
                None => (false, "[memory_gc] long-term memory not available".to_string(), None),
            }
        }
        "sessions_list" => {
            let limit = args.get(0).and_then(|s| s.parse::<usize>().ok()).unwrap_or(20).min(50);
            match store_path {
                Some(path) => match TaskStore::open(path) {
                    Ok(store) => match store.get_all() {
                        Ok(tasks) => {
                            let list: Vec<String> = tasks
                                .into_iter()
                                .rev()
                                .take(limit)
                                .map(|t| format!("{} {} {}", t.id, t.status.as_str(), t.assigned_agent))
                                .collect();
                            (true, format!("[sessions_list] {} task(s): {}", list.len(), list.join(" ; ")), None)
                        }
                        Err(e) => (false, format!("[sessions_list] error: {}", e), None),
                    },
                    Err(e) => (false, format!("[sessions_list] store error: {}", e), None),
                },
                None => (false, "[sessions_list] store not available".to_string(), None),
            }
        }
        "session_status" => {
            let task_id_str = args.get(0).map(String::as_str).unwrap_or("");
            let id = task_id_str.parse::<Uuid>().ok();
            match (store_path, id) {
                (Some(path), Some(id)) => match TaskStore::open(path) {
                    Ok(store) => match store.get(id) {
                        Ok(Some(t)) => (true, format!(
                            "[session_status] {} status={} agent={}",
                            t.id,
                            t.status.as_str(),
                            t.assigned_agent
                        ), None),
                        Ok(None) => (false, format!("[session_status] task {} not found", id), None),
                        Err(e) => (false, format!("[session_status] error: {}", e), None),
                    },
                    Err(e) => (false, format!("[session_status] store error: {}", e), None),
                },
                (_, _) => (false, "[session_status] usage: session_status <task_id>".to_string(), None),
            }
        }
        "sessions_spawn" => {
            let message = args.get(0).map(|a| a.as_str()).unwrap_or("").to_string();
            let child_session_id = args.get(1).map(|a| a.as_str()).unwrap_or("").to_string();
            if message.is_empty() {
                return (false, "[sessions_spawn] usage: sessions_spawn <message> [session_id]".to_string(), None);
            }
            match (store_path, conv_tx) {
                (Some(path), Some(tx)) => {
                    let new_id = Uuid::new_v4();
                    let now = chrono::Utc::now();
                    const MAX_MSG: usize = 500;
                    let initial_message = if message.len() > MAX_MSG {
                        Some(message.chars().take(MAX_MSG).chain(std::iter::once('…')).collect::<String>())
                    } else if message.is_empty() {
                        None
                    } else {
                        Some(message.clone())
                    };
                    let task = Task {
                        id: new_id,
                        parent_task_id: Some(task_id),
                        status: TaskStatus::Pending,
                        assigned_agent: "conversation".to_string(),
                        created_at: now,
                        updated_at: now,
                        initial_message,
                        studio_project_id: None,
                    };
                    match TaskStore::open(path) {
                        Ok(store) => {
                            if store.insert(&task).is_err() {
                                return (false, "[sessions_spawn] failed to insert task".to_string(), None);
                            }
                            let sid = if child_session_id.is_empty() {
                                new_id.to_string()
                            } else {
                                child_session_id
                            };
                            if tx
                                .send(OrchestratorTask {
                                    task_id: new_id,
                                    message,
                                    session_id: sid,
                                    image_data_urls: None,
                                    execution_mode: None,
                                    preferred_task_type: None,
                                    incognito: false,
                                })
                                .await
                                .is_err()
                            {
                                return (false, "[sessions_spawn] failed to send to conversation queue".to_string(), None);
                            }
                            (true, format!("[sessions_spawn] task_id: {} (queued)", new_id), None)
                        }
                        Err(e) => (false, format!("[sessions_spawn] store error: {}", e), None),
                    }
                }
                (_, _) => (false, "[sessions_spawn] store or conversation channel not available".to_string(), None),
            }
        }
        "schedule_task" => {
            let cron = args.get(0).map(String::as_str).unwrap_or("").trim().to_string();
            let mut title = "Agent scheduled task".to_string();
            let mut prompt_parts: Vec<&str> = Vec::new();
            let mut i = 1;
            while i < args.len() {
                if args[i] == "--title" {
                    if let Some(value) = args.get(i + 1) {
                        title = value.clone();
                        i += 2;
                        continue;
                    }
                }
                prompt_parts.push(args[i].as_str());
                i += 1;
            }
            let prompt = prompt_parts.join(" ");
            let prompt = prompt.trim();
            if cron.is_empty() || prompt.is_empty() {
                return (
                    false,
                    "[schedule_task] usage: schedule_task <cron> <prompt...> [--title <title>]".to_string(),
                    None,
                );
            }
            match store_path {
                Some(path) => match ScheduleStore::open(path) {
                    Ok(store) => {
                        let now = chrono::Utc::now();
                        let s = Schedule {
                            id: Uuid::new_v4(),
                            name: title,
                            description: prompt.to_string(),
                            timezone: "UTC".to_string(),
                            rrule: cron.to_string(),
                            interval_seconds: None,
                            start_at: now,
                            end_at: None,
                            channel_context: Some(
                                serde_json::json!({
                                    "message": prompt,
                                    "session_id": format!("task:{}", task_id),
                                    "source": "tool:schedule_task",
                                    "created_by_task_id": task_id.to_string()
                                })
                                .to_string(),
                            ),
                            enabled: true,
                            created_at: now,
                            updated_at: now,
                        };
                        match store.insert_schedule(&s) {
                            Ok(()) => (
                                true,
                                format!(
                                    "[schedule_task] created schedule {} ({})",
                                    s.id, cron
                                ),
                                None,
                            ),
                            Err(e) => (false, format!("[schedule_task] {}", e), None),
                        }
                    }
                    Err(e) => (false, format!("[schedule_task] store error: {}", e), None),
                },
                None => (false, "[schedule_task] store not available".to_string(), None),
            }
        }
        "list_scheduled_tasks" => {
            let limit = args
                .get(0)
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(20)
                .min(100);
            match store_path {
                Some(path) => match ScheduleStore::open(path) {
                    Ok(store) => match store.list_schedules() {
                        Ok(list) => {
                            let lines: Vec<String> = list
                                .into_iter()
                                .rev()
                                .take(limit)
                                .map(|s| {
                                    format!(
                                        "{} {} {} {}",
                                        s.id,
                                        if s.enabled { "enabled" } else { "disabled" },
                                        "cron",
                                        s.rrule
                                    )
                                })
                                .collect();
                            (
                                true,
                                format!(
                                    "[list_scheduled_tasks] {} schedule(s): {}",
                                    lines.len(),
                                    lines.join(" ; ")
                                ),
                                None,
                            )
                        }
                        Err(e) => (false, format!("[list_scheduled_tasks] {}", e), None),
                    },
                    Err(e) => (false, format!("[list_scheduled_tasks] store error: {}", e), None),
                },
                None => (false, "[list_scheduled_tasks] store not available".to_string(), None),
            }
        }
        "cancel_scheduled_task" => {
            let id = args
                .get(0)
                .and_then(|s| Uuid::parse_str(s).ok());
            match (store_path, id) {
                (Some(path), Some(schedule_id)) => match ScheduleStore::open(path) {
                    Ok(store) => match store.get_schedule(schedule_id) {
                        Ok(Some(_)) => match store.delete_schedule(schedule_id) {
                            Ok(()) => (
                                true,
                                format!("[cancel_scheduled_task] deleted {}", schedule_id),
                                None,
                            ),
                            Err(e) => (false, format!("[cancel_scheduled_task] {}", e), None),
                        },
                        Ok(None) => (
                            false,
                            format!("[cancel_scheduled_task] {} not found", schedule_id),
                            None,
                        ),
                        Err(e) => (false, format!("[cancel_scheduled_task] {}", e), None),
                    },
                    Err(e) => (false, format!("[cancel_scheduled_task] store error: {}", e), None),
                },
                (_, _) => (
                    false,
                    "[cancel_scheduled_task] usage: cancel_scheduled_task <schedule_id>".to_string(),
                    None,
                ),
            }
        }
        "mcp_server_add" => {
            let name = args.get(0).map(String::as_str).unwrap_or("").trim();
            let command = args.get(1).map(String::as_str).unwrap_or("").trim();
            if name.is_empty() || command.is_empty() {
                return (
                    false,
                    "[mcp_server_add] usage: mcp_server_add <name> <command> [args...]".to_string(),
                    None,
                );
            }
            let mcp_args: Vec<serde_json::Value> = args
                .get(2..)
                .unwrap_or(&[])
                .iter()
                .map(|s| serde_json::Value::String(s.clone()))
                .collect();
            let entry = if mcp_args.is_empty() {
                serde_json::json!({ "command": command })
            } else {
                serde_json::json!({ "command": command, "args": mcp_args })
            };
            match store_path.and_then(|sp| sp.parent()) {
                Some(data_dir) => match crate::mcp::add_mcp_server(data_dir, name, &entry) {
                    Ok(msg) => {
                        crate::http_get_cache::invalidate_mcp_status();
                        (true, format!("[mcp_server_add] {msg}"), None)
                    }
                    Err(e) => (false, format!("[mcp_server_add] {e}"), None),
                },
                None => (false, "[mcp_server_add] data directory not available".to_string(), None),
            }
        }
        "mcp_server_remove" => {
            let name = args.get(0).map(String::as_str).unwrap_or("").trim();
            if name.is_empty() {
                return (
                    false,
                    "[mcp_server_remove] usage: mcp_server_remove <name>".to_string(),
                    None,
                );
            }
            match store_path.and_then(|sp| sp.parent()) {
                Some(data_dir) => match crate::mcp::remove_mcp_server(data_dir, name) {
                    Ok(msg) => {
                        crate::http_get_cache::invalidate_mcp_status();
                        (true, format!("[mcp_server_remove] {msg}"), None)
                    }
                    Err(e) => (false, format!("[mcp_server_remove] {e}"), None),
                },
                None => (
                    false,
                    "[mcp_server_remove] data directory not available".to_string(),
                    None,
                ),
            }
        }
        "wake_in" => {
            let minutes = args
                .get(0)
                .and_then(|s| s.parse::<i64>().ok())
                .filter(|&m| m > 0 && m <= 525_600);
            let message = if args.len() > 1 {
                args[1..].join(" ")
            } else {
                String::new()
            };
            let message = message.trim().to_string();
            if minutes.is_none() || message.is_empty() {
                return (
                    false,
                    "[wake_in] usage: wake_in <minutes> <message>".to_string(),
                    None,
                );
            }
            let minutes = minutes.unwrap();
            match store_path {
                Some(path) => {
                    use akasha_store::platform_extras::{Wakeup, WakeupStore};
                    let sid = session_id
                        .map(String::from)
                        .unwrap_or_else(|| format!("task:{}", task_id));
                    let fire_at =
                        chrono::Utc::now() + chrono::Duration::minutes(minutes);
                    let w = Wakeup {
                        id: Uuid::new_v4(),
                        session_id: sid,
                        fire_at,
                        message: message.clone(),
                        status: "pending".into(),
                        created_by_task_id: Some(task_id),
                        rrule: None,
                        created_at: chrono::Utc::now(),
                    };
                    match WakeupStore::open(path) {
                        Ok(store) => match store.insert(&w) {
                            Ok(()) => (
                                true,
                                format!(
                                    "[wake_in] scheduled wakeup {} in {} min (at {})",
                                    w.id,
                                    minutes,
                                    fire_at.to_rfc3339()
                                ),
                                None,
                            ),
                            Err(e) => (false, format!("[wake_in] {}", e), None),
                        },
                        Err(e) => (false, format!("[wake_in] store error: {}", e), None),
                    }
                }
                None => (false, "[wake_in] store not available".to_string(), None),
            }
        }
        "calendar_query" => {
            if !executor.policy.calendar_read_enabled {
                return (
                    false,
                    "[calendar_query] calendar_read_enabled is false in tools_policy.yaml".to_string(),
                    None,
                );
            }
            let from_s = args.first().map(String::as_str).unwrap_or("");
            let to_s = args.get(1).map(String::as_str).unwrap_or("");
            let account_id = args
                .get(2)
                .and_then(|s| Uuid::parse_str(s.trim()).ok());
            let from = chrono::DateTime::parse_from_rfc3339(from_s)
                .map(|d| d.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now());
            let to = chrono::DateTime::parse_from_rfc3339(to_s)
                .map(|d| d.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| from + chrono::Duration::days(1));
            match store_path {
                Some(path) => (
                    true,
                    crate::api_routes_calendar::calendar_query_compact(path, from, to, account_id),
                    None,
                ),
                None => (false, "[calendar_query] store not available".to_string(), None),
            }
        }
        "calendar_create" => {
            if !executor.policy.calendar_write_enabled {
                return (
                    false,
                    "[calendar_create] calendar_write_enabled is false (enable in tools_policy.yaml)".to_string(),
                    None,
                );
            }
            if args.len() < 3 {
                return (
                    false,
                    "[calendar_create] usage: calendar_create <summary> <from_iso> <to_iso> [account_id] [description]".to_string(),
                    None,
                );
            }
            let summary = args[0].clone();
            let from_s = &args[1];
            let to_s = &args[2];
            let mut account_id = akasha_store::default_ics_account_id();
            let description = if args.len() > 3 {
                if let Ok(parsed) = Uuid::parse_str(args[3].trim()) {
                    account_id = parsed;
                    if args.len() > 4 {
                        Some(args[4..].join(" "))
                    } else {
                        None
                    }
                } else {
                    Some(args[3..].join(" "))
                }
            } else {
                None
            };
            let dtstart = chrono::DateTime::parse_from_rfc3339(from_s)
                .map(|d| d.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now());
            let dtend = chrono::DateTime::parse_from_rfc3339(to_s)
                .map(|d| d.with_timezone(&chrono::Utc))
                .ok();
            match store_path {
                Some(path) => match akasha_store::ExternalCalendarStore::open(path) {
                    Ok(store) => {
                        let id = Uuid::new_v4();
                        let uid = format!("akasha-{}", id);
                        let row = akasha_store::ExternalCalendarEvent {
                            id,
                            account_id,
                            uid,
                            href: None,
                            etag: None,
                            summary,
                            description,
                            location: None,
                            dtstart,
                            dtend,
                            timezone: None,
                            rrule: None,
                            exdates_json: "[]".to_string(),
                            source: "local".to_string(),
                            synced_at: Some(chrono::Utc::now()),
                            deleted: false,
                        };
                        match store.upsert_event(&row) {
                            Ok(()) => {
                                let payload = serde_json::to_string(&row).unwrap_or_default();
                                let _ = store.enqueue_outbox(&akasha_store::CalDavOutboxRow {
                                    id: Uuid::new_v4(),
                                    account_id: row.account_id,
                                    event_id: Some(id),
                                    operation: "create".to_string(),
                                    payload_json: payload,
                                    created_at: chrono::Utc::now(),
                                    applied_at: None,
                                    error: None,
                                });
                                (
                                    true,
                                    format!("[calendar_create] created event_id={id}"),
                                    None,
                                )
                            }
                            Err(e) => (false, format!("[calendar_create] {e}"), None),
                        }
                    }
                    Err(e) => (false, format!("[calendar_create] store: {e}"), None),
                },
                None => (false, "[calendar_create] store not available".to_string(), None),
            }
        }
        "calendar_update" => {
            if !executor.policy.calendar_write_enabled {
                return (
                    false,
                    "[calendar_update] calendar_write_enabled is false".to_string(),
                    None,
                );
            }
            if args.len() < 2 {
                return (
                    false,
                    "[calendar_update] usage: calendar_update <event_id> <json>".to_string(),
                    None,
                );
            }
            let Ok(event_id) = Uuid::parse_str(args[0].trim()) else {
                return (false, "[calendar_update] invalid event_id".to_string(), None);
            };
            let json_part = args[1..].join(" ");
            let Ok(j) = serde_json::from_str::<serde_json::Value>(&json_part) else {
                return (false, "[calendar_update] invalid json".to_string(), None);
            };
            match store_path {
                Some(path) => match akasha_store::ExternalCalendarStore::open(path) {
                    Ok(store) => {
                        let Some(mut row) = store.get_event(event_id).ok().flatten() else {
                            return (false, "[calendar_update] not found".to_string(), None);
                        };
                        if let Some(s) = j.get("summary").and_then(|v| v.as_str()) {
                            row.summary = s.to_string();
                        }
                        if let Some(s) = j.get("description").and_then(|v| v.as_str()) {
                            row.description = Some(s.to_string());
                        }
                        if let Some(s) = j.get("location").and_then(|v| v.as_str()) {
                            row.location = Some(s.to_string());
                        }
                        if let Some(s) = j.get("dtstart").and_then(|v| v.as_str()) {
                            if let Ok(d) = chrono::DateTime::parse_from_rfc3339(s) {
                                row.dtstart = d.with_timezone(&chrono::Utc);
                            }
                        }
                        if let Some(s) = j.get("dtend").and_then(|v| v.as_str()) {
                            if let Ok(d) = chrono::DateTime::parse_from_rfc3339(s) {
                                row.dtend = Some(d.with_timezone(&chrono::Utc));
                            }
                        }
                        row.synced_at = Some(chrono::Utc::now());
                        match store.upsert_event(&row) {
                            Ok(()) => {
                                let payload = serde_json::to_string(&row).unwrap_or_default();
                                let _ = store.enqueue_outbox(&akasha_store::CalDavOutboxRow {
                                    id: Uuid::new_v4(),
                                    account_id: row.account_id,
                                    event_id: Some(event_id),
                                    operation: "update".to_string(),
                                    payload_json: payload,
                                    created_at: chrono::Utc::now(),
                                    applied_at: None,
                                    error: None,
                                });
                                (true, format!("[calendar_update] updated {event_id}"), None)
                            }
                            Err(e) => (false, format!("[calendar_update] {e}"), None),
                        }
                    }
                    Err(e) => (false, format!("[calendar_update] store: {e}"), None),
                },
                None => (false, "[calendar_update] store not available".to_string(), None),
            }
        }
        "calendar_delete" => {
            if !executor.policy.calendar_write_enabled {
                return (
                    false,
                    "[calendar_delete] calendar_write_enabled is false".to_string(),
                    None,
                );
            }
            let Ok(event_id) = Uuid::parse_str(args.first().map(String::as_str).unwrap_or("").trim()) else {
                return (
                    false,
                    "[calendar_delete] usage: calendar_delete <event_id>".to_string(),
                    None,
                );
            };
            match store_path {
                Some(path) => match akasha_store::ExternalCalendarStore::open(path) {
                    Ok(store) => {
                        let row = store.get_event(event_id).ok().flatten();
                        match store.soft_delete_event(event_id) {
                            Ok(true) => {
                                if let Some(ref r) = row {
                                    let payload = serde_json::json!({
                                        "event_id": event_id.to_string(),
                                        "href": r.href,
                                        "uid": r.uid,
                                    });
                                    let _ = store.enqueue_outbox(&akasha_store::CalDavOutboxRow {
                                        id: Uuid::new_v4(),
                                        account_id: r.account_id,
                                        event_id: Some(event_id),
                                        operation: "delete".to_string(),
                                        payload_json: payload.to_string(),
                                        created_at: chrono::Utc::now(),
                                        applied_at: None,
                                        error: None,
                                    });
                                }
                                (true, format!("[calendar_delete] deleted {event_id}"), None)
                            }
                            Ok(false) => (false, "[calendar_delete] not found".to_string(), None),
                            Err(e) => (false, format!("[calendar_delete] {e}"), None),
                        }
                    }
                    Err(e) => (false, format!("[calendar_delete] store: {e}"), None),
                },
                None => (false, "[calendar_delete] store not available".to_string(), None),
            }
        }
        "budget_status" => {
            let session_id = args.get(0).cloned().unwrap_or_default();
            let settings = match store_path {
                Some(path) => path.parent().map(load_budget_settings).unwrap_or_default(),
                None => BudgetSettings::default(),
            };
            let scope = if session_id.trim().is_empty() {
                "global".to_string()
            } else {
                format!("session {}", session_id)
            };
            (
                false,
                format!(
                    "[budget_status] limit={} warn_ratio={:.2} auto_concise={} scope={} usage_unavailable=true message=\"live usage is not available from this tool path; query the local /api/budget endpoint for accurate totals\"",
                    settings.daily_token_limit,
                    settings.warn_ratio,
                    settings.auto_concise,
                    scope
                ),
                None,
            )
        }
        "message" => {
            let sub = args.get(0).map(String::as_str).unwrap_or("");
            if sub != "send" || args.len() < 3 {
                return (false, "[message] usage: message send <channel> <text>".to_string(), None);
            }
            let channel = args.get(1).map(String::as_str).unwrap_or("");
            let text = args.get(2..).map(|a| a.join(" ")).unwrap_or_default();
            match message_webhook_url {
                Some(url) => {
                    let body = serde_json::json!({ "channel": channel, "text": text });
                    let client = match reqwest::Client::builder()
                        .timeout(std::time::Duration::from_secs(10))
                        .build()
                    {
                        Ok(c) => c,
                        Err(e) => return (false, format!("[message] client error: {}", e), None),
                    };
                    match client.post(url).json(&body).send().await {
                        Ok(res) if res.status().is_success() => (true, "[message] sent".to_string(), None),
                        Ok(res) => (false, format!("[message] send failed: {}", res.status()), None),
                        Err(e) => (false, format!("[message] error: {}", e), None),
                    }
                }
                None => (false, "[message] AKASHA_MESSAGE_WEBHOOK_URL not set".to_string(), None),
            }
        }
        "browser" => {
            let sub = args.get(0).map(String::as_str).unwrap_or("").trim();
            if !executor.policy.browser_enabled {
                return (
                    false,
                    "[browser] Browser automation is disabled. Set browser_enabled: true in tools_policy.yaml and install Playwright (npx playwright install chromium).".to_string(),
                    None,
                );
            }
            let Some(registry) = browser_registry else {
                return (false, "[browser] Browser registry not available.".to_string(), None);
            };
            let Some(runner_path) = crate::browser::find_playwright_runner_path() else {
                return (
                    false,
                    "[browser] Playwright runner not found. Install: copy scripts/playwright-runner next to the executable (playwright-runner/run.mjs), or under %USERPROFILE%\\akasha\\playwright-runner, or set AKASHA_PLAYWRIGHT_RUNNER / AKASHA_DATA_DIR (see docs). Then npm install in that folder.".to_string(),
                    None,
                );
            };
            let headless = executor.policy.browser_headless;
            let action_timeout = executor.policy.browser_action_timeout_secs;
            let session_timeout = executor.policy.browser_session_timeout_secs;

            // Enforce per-session max duration: if the existing session has exceeded the
            // configured timeout, close and evict it before dispatching the command.
            {
                let mut g = registry.write().await;
                if let Some(s) = g.get(&task_id) {
                    if s.started_at.elapsed().as_secs() >= session_timeout {
                        tracing::info!(task_id = %task_id, timeout_secs = session_timeout, "[browser] session timed out, closing");
                        if let Some(mut sess) = g.remove(&task_id) {
                            let _ = sess.close().await;
                        }
                    }
                }
            }

            if sub == "navigate" {
                let Some(url_arg) = args.get(1) else {
                    return (false, "[browser] usage: browser navigate <url>".to_string(), None);
                };
                let url = url_arg.trim();
                if !url.starts_with("http://") && !url.starts_with("https://") {
                    return (false, "[browser] navigate requires an http or https URL".to_string(), None);
                }
                let host = url.parse::<url::Url>().ok().and_then(|u| u.host_str().map(String::from)).unwrap_or_default();
                if !executor.policy.can_use_browser_domain(&host) {
                    let hint = crate::browser::format_domain_denied(
                        &host,
                        &executor.policy.browser_allowed_domains,
                        &executor.policy.browser_blocked_domains,
                    );
                    return (false, format!("[browser] {}", hint), None);
                }
                let mut g = registry.write().await;
                let session = if let Some(mut s) = g.remove(&task_id) {
                    drop(g);
                    let res = s.send_command(&serde_json::json!({ "cmd": "navigate", "params": { "url": url, "timeout_secs": action_timeout } })).await;
                    // Always reinsert the session — a navigate failure doesn't mean the browser is dead.
                    registry.write().await.insert(task_id, s);
                    res
                } else {
                    drop(g);
                    match crate::browser::create_browser_session(&runner_path, headless, action_timeout).await {
                        Ok(mut new_session) => {
                            let res = new_session
                                .send_command(&serde_json::json!({ "cmd": "navigate", "params": { "url": url, "timeout_secs": action_timeout } }))
                                .await;
                            // Only register if IPC still works; on Err (e.g. runner stdout EOF) the child may
                            // already be reaped — inserting a dead session poisons later navigate/snapshot calls.
                            if res.is_ok() {
                                let mut g = registry.write().await;
                                g.insert(task_id, new_session);
                            }
                            res
                        }
                        Err(e) => {
                            let msg = crate::browser::format_runner_error(
                                &e,
                                action_timeout,
                                session_timeout,
                            );
                            return (false, format!("[browser] error: {}", msg), None);
                        }
                    }
                };
                match session {
                    Ok(resp) => {
                        let ok = resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                        if ok {
                            let result = resp.get("result");
                            let msg = if let Some(r) = result {
                                if let Some(title) = r.get("title").and_then(|v| v.as_str()) {
                                    format!("[browser] Navigated to {} (title: {}).", url, title)
                                } else {
                                    format!("[browser] Navigated to {}.", url)
                                }
                            } else {
                                format!("[browser] Navigated to {}.", url)
                            };
                            (true, msg, None)
                        } else {
                            let err = resp.get("error").and_then(|v| v.as_str()).unwrap_or("Navigate failed");
                            let err = crate::browser::format_runner_error(err, action_timeout, session_timeout);
                            (false, format!("[browser] {}", err), None)
                        }
                    }
                    Err(e) => {
                        let msg = crate::browser::format_runner_error(&e, action_timeout, session_timeout);
                        (false, format!("[browser] error: {}", msg), None)
                    }
                }
            } else if sub == "snapshot" {
                let mut g = registry.write().await;
                let Some(mut session) = g.remove(&task_id) else {
                    return (false, "[browser] Navigate to a page first (browser navigate <url>).".to_string(), None);
                };
                drop(g);
                let resp = session.send_command(&serde_json::json!({ "cmd": "snapshot" })).await;
                registry.write().await.insert(task_id, session);
                match resp {
                    Ok(resp) => {
                        let ok = resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                        if ok {
                            let result = resp.get("result").and_then(|r| r.get("text").and_then(|t| t.as_str())).unwrap_or("");
                            let (preview, total, trunc) =
                                crate::tool_output::truncate_utf8_by_bytes(result, crate::tool_output::BROWSER_SNAPSHOT_TEXT_MAX);
                            let base = format!(
                                "[browser] Snapshot ({} bytes):\n{}",
                                total,
                                preview
                            );
                            let msg = crate::tool_output::with_truncation_footer(
                                base,
                                trunc,
                                total,
                                "use web_fetch on static URLs when applicable or navigate to a narrower page",
                            );
                            (true, msg, None)
                        } else {
                            let err = resp.get("error").and_then(|v| v.as_str()).unwrap_or("Snapshot failed");
                            let err = crate::browser::format_runner_error(err, action_timeout, session_timeout);
                            (false, format!("[browser] {}", err), None)
                        }
                    }
                    Err(e) => {
                        let msg = crate::browser::format_runner_error(&e, action_timeout, session_timeout);
                        (false, format!("[browser] error: {}", msg), None)
                    }
                }
            } else if sub == "screenshot" {
                let mut g = registry.write().await;
                let Some(mut session) = g.remove(&task_id) else {
                    return (false, "[browser] Navigate to a page first (browser navigate <url>).".to_string(), None);
                };
                drop(g);
                let resp = session
                    .send_command(&serde_json::json!({ "cmd": "screenshot", "params": { "full_page": false } }))
                    .await;
                registry.write().await.insert(task_id, session);
                match resp {
                    Ok(resp) => {
                        let ok = resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                        if ok {
                            let b64 = resp
                                .pointer("/result/data_base64")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let vision_attach_enabled = std::env::var("AKASHA_BROWSER_SCREENSHOT_VISION")
                                .map(|v| {
                                    let l = v.to_lowercase();
                                    l != "0" && l != "false" && l != "no"
                                })
                                .unwrap_or(true);
                            let mut captured_for_llm: Option<String> = None;
                            if vision_attach_enabled && !b64.is_empty() {
                                let url =
                                    format!("data:image/png;base64,{}", b64);
                                if vision_payload_within_cap(&url) {
                                    captured_for_llm = Some(url);
                                } else {
                                    tracing::info!(
                                        len = url.len(),
                                        cap = vision_inject_max_chars(),
                                        "browser screenshot: vision attachment skipped (exceeds AKASHA_VISION_INJECT_MAX_CHARS)"
                                    );
                                }
                            }
                            let (preview, total, trunc) = crate::tool_output::truncate_utf8_by_bytes(
                                b64,
                                crate::tool_output::BROWSER_SCREENSHOT_B64_MAX,
                            );
                            let base = format!(
                                "[browser] Screenshot PNG (base64, {} bytes of payload):\n{}",
                                total, preview
                            );
                            let msg = crate::tool_output::with_truncation_footer(
                                base,
                                trunc,
                                total,
                                "truncated base64 in text; full frame attached for vision on next model turn when enabled (AKASHA_BROWSER_SCREENSHOT_VISION)",
                            );
                            (true, msg, captured_for_llm)
                        } else {
                            let err = resp.get("error").and_then(|v| v.as_str()).unwrap_or("screenshot failed");
                            let err = crate::browser::format_runner_error(err, action_timeout, session_timeout);
                            (false, format!("[browser] {}", err), None)
                        }
                    }
                    Err(e) => {
                        let msg = crate::browser::format_runner_error(&e, action_timeout, session_timeout);
                        (false, format!("[browser] error: {}", msg), None)
                    }
                }
            } else if sub == "click" {
                let selector = args[1..].join(" ").trim().to_string();
                if selector.is_empty() {
                    return (false, "[browser] usage: browser click <css_selector>".to_string(), None);
                }
                let mut g = registry.write().await;
                let Some(mut session) = g.remove(&task_id) else {
                    return (false, "[browser] Navigate to a page first (browser navigate <url>).".to_string(), None);
                };
                drop(g);
                let resp = session
                    .send_command(&serde_json::json!({
                        "cmd": "click",
                        "params": { "selector": selector, "timeout_secs": action_timeout }
                    }))
                    .await;
                registry.write().await.insert(task_id, session);
                match resp {
                    Ok(resp) => {
                        let ok = resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                        if ok {
                            (true, "[browser] Click OK.".to_string(), None)
                        } else {
                            let err = resp.get("error").and_then(|v| v.as_str()).unwrap_or("click failed");
                            let err = crate::browser::format_runner_error(err, action_timeout, session_timeout);
                            (false, format!("[browser] {}", err), None)
                        }
                    }
                    Err(e) => {
                        let msg = crate::browser::format_runner_error(&e, action_timeout, session_timeout);
                        (false, format!("[browser] error: {}", msg), None)
                    }
                }
            } else if sub == "fill" {
                let Some(sel) = args.get(1).map(|s| s.as_str()) else {
                    return (
                        false,
                        "[browser] usage: browser fill <css_selector> <text>".to_string(),
                        None,
                    );
                };
                let sel = sel.trim();
                if sel.is_empty() || args.len() < 3 {
                    return (
                        false,
                        "[browser] usage: browser fill <css_selector> <text>".to_string(),
                        None,
                    );
                }
                let value: String = args[2..].join(" ");
                let mut g = registry.write().await;
                let Some(mut session) = g.remove(&task_id) else {
                    return (false, "[browser] Navigate to a page first (browser navigate <url>).".to_string(), None);
                };
                drop(g);
                let resp = session
                    .send_command(&serde_json::json!({
                        "cmd": "fill",
                        "params": { "selector": sel, "value": value, "timeout_secs": action_timeout }
                    }))
                    .await;
                registry.write().await.insert(task_id, session);
                match resp {
                    Ok(resp) => {
                        let ok = resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                        if ok {
                            (true, "[browser] Fill OK.".to_string(), None)
                        } else {
                            let err = resp.get("error").and_then(|v| v.as_str()).unwrap_or("fill failed");
                            let err = crate::browser::format_runner_error(err, action_timeout, session_timeout);
                            (false, format!("[browser] {}", err), None)
                        }
                    }
                    Err(e) => {
                        let msg = crate::browser::format_runner_error(&e, action_timeout, session_timeout);
                        (false, format!("[browser] error: {}", msg), None)
                    }
                }
            } else if sub == "wait" {
                let arg = args[1..].join(" ").trim().to_string();
                if arg.is_empty() {
                    return (
                        false,
                        "[browser] usage: browser wait <css_selector> | browser wait <milliseconds>".to_string(),
                        None,
                    );
                }
                let mut g = registry.write().await;
                let Some(mut session) = g.remove(&task_id) else {
                    return (false, "[browser] Navigate to a page first (browser navigate <url>).".to_string(), None);
                };
                drop(g);
                let cmd = if arg.chars().all(|c| c.is_ascii_digit()) {
                    let ms: u64 = arg.parse().unwrap_or(0);
                    serde_json::json!({ "cmd": "wait", "params": { "milliseconds": ms } })
                } else {
                    serde_json::json!({
                        "cmd": "wait",
                        "params": { "selector": arg, "timeout_secs": action_timeout }
                    })
                };
                let resp = session.send_command(&cmd).await;
                registry.write().await.insert(task_id, session);
                match resp {
                    Ok(resp) => {
                        let ok = resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                        if ok {
                            (true, "[browser] Wait completed.".to_string(), None)
                        } else {
                            let err = resp.get("error").and_then(|v| v.as_str()).unwrap_or("wait failed");
                            let err = crate::browser::format_runner_error(err, action_timeout, session_timeout);
                            (false, format!("[browser] {}", err), None)
                        }
                    }
                    Err(e) => {
                        let msg = crate::browser::format_runner_error(&e, action_timeout, session_timeout);
                        (false, format!("[browser] error: {}", msg), None)
                    }
                }
            } else {
                (false, "[browser] usage: browser navigate <url> | browser snapshot | browser screenshot | browser click <selector> | browser fill <selector> <text> | browser wait <selector|ms>".to_string(), None)
            }
        }
        "install_playwright" => {
            if !executor.policy.browser_enabled {
                return (
                    false,
                    "[install_playwright] Browser automation is disabled. Set browser_enabled: true in tools_policy.yaml.".to_string(),
                    None,
                );
            }
            let Some(runner_path) = crate::browser::find_playwright_runner_path() else {
                return (
                    false,
                    "[install_playwright] Playwright runner not found. Copy scripts/playwright-runner beside the executable or under ~/akasha/playwright-runner, or set AKASHA_PLAYWRIGHT_RUNNER / AKASHA_DATA_DIR.".to_string(),
                    None,
                );
            };
            let Some(runner_dir) = crate::browser::playwright_runner_dir(&runner_path) else {
                return (
                    false,
                    "[install_playwright] Could not resolve runner directory.".to_string(),
                    None,
                );
            };
            match crate::browser::ensure_playwright_chromium(&runner_dir).await {
                Ok(()) => (
                    true,
                    format!(
                        "[install_playwright] npm install and playwright install chromium completed in {}.",
                        runner_dir.display()
                    ),
                    None,
                ),
                Err(e) => (false, format!("[install_playwright] {}", e), None),
            }
        }
        "image" => {
            let path_str = path_arg_joined(args);
            let path_str = path_str.trim();
            if path_str.is_empty() {
                (
                    false,
                    "[image] usage: image <path> [prompt] — read a local image (allowed_read_paths / workspace:/) and return metadata + data URL for vision.".to_string(),
                    None,
                )
            } else if path_str.starts_with("http://") || path_str.starts_with("https://") {
                (
                    false,
                    "[image] remote URLs are not supported — use a local path in allowed_read_paths or workspace:/".to_string(),
                    None,
                )
            } else {
                let disk_path = resolve_tool_disk_path(path_str, workspace_root);
                if !executor.policy.can_read(&disk_path) {
                    (
                        false,
                        "[image] path not allowed by policy (allowed_read_paths)".to_string(),
                        None,
                    )
                } else {
                    match tokio::fs::read(&disk_path).await {
                        Ok(bytes) => {
                            let mime = guess_image_mime_from_path(&disk_path);
                            let b64 = base64::Engine::encode(
                                &base64::engine::general_purpose::STANDARD,
                                &bytes,
                            );
                            let data_url = format!("data:{mime};base64,{b64}");
                            (
                                true,
                                format!(
                                    "[image {}] {} bytes, mime={mime} — data URL ready for vision injection",
                                    disk_path.display(),
                                    bytes.len()
                                ),
                                Some(data_url),
                            )
                        }
                        Err(e) => (
                            false,
                            format!(
                                "[image] read failed: {e} (ensure file exists at {})",
                                disk_path.display()
                            ),
                            None,
                        ),
                    }
                }
            }
        }
        "pdf" => {
            let path_str = path_arg_joined(args);
            let path_str = path_str.trim();
            if path_str.is_empty() {
                (false, "[pdf] usage: pdf <path> — path must be in allowed_read_paths".to_string(), None)
            } else if path_str.starts_with("workspace:/") || path_str.starts_with("workspace:") {
                let key = path_str
                    .trim_start_matches("workspace:/")
                    .trim_start_matches("workspace:")
                    .trim_start_matches('/')
                    .to_string();
                let key = normalize_apostrophes(&key);
                let disk_path = strip_verbatim_prefix(
                    workspace_root
                        .map(|root| root.join(&key))
                        .or_else(|| std::env::current_dir().ok().map(|cwd| cwd.join(&key)))
                        .unwrap_or_else(|| Path::new(&key).to_path_buf()),
                );
                if !executor.policy.can_read(&disk_path) {
                    (false, "[pdf] path not allowed by policy (allowed_read_paths)".to_string(), None)
                } else {
                    match tokio::fs::read(&disk_path).await {
                        Ok(bytes) => match pdf_extract::extract_text_from_mem(&bytes) {
                            Ok(text) => {
                                let preview = if text.len() > 2000 { format!("{}…", text.chars().take(2000).collect::<String>()) } else { text.clone() };
                                (true, format!("[pdf {}] extracted {} chars:\n{}", disk_path.display(), text.len(), preview), None)
                            }
                            Err(e) => (false, format!("[pdf] extraction failed: {}", e), None),
                        },
                        Err(e) => (false, format!("[pdf] read failed: {} (ensure file exists at {})", e, disk_path.display()), None),
                    }
                }
            } else {
                let path = Path::new(path_str);
                if !executor.policy.can_read(path) {
                    (false, "[pdf] path not allowed by policy (allowed_read_paths)".to_string(), None)
                } else {
                    match tokio::fs::read(path).await {
                        Ok(bytes) => match pdf_extract::extract_text_from_mem(&bytes) {
                            Ok(text) => {
                                let preview = if text.len() > 2000 { format!("{}…", text.chars().take(2000).collect::<String>()) } else { text.clone() };
                                (true, format!("[pdf {}] extracted {} chars:\n{}", path.display(), text.len(), preview), None)
                            }
                            Err(e) => (false, format!("[pdf] extraction failed: {}", e), None),
                        },
                        Err(e) => (false, format!("[pdf] read failed: {}", e), None),
                    }
                }
            }
        }
        "search_files" => {
            let (args, respect_gitignore, _) = strip_file_search_flags(args);
            let raw_dir = args.get(0).map(String::as_str).unwrap_or(".");
            let dir_pb = resolve_tool_disk_path(raw_dir, workspace_root);
            let pattern = args.get(1).map(String::as_str).unwrap_or("*");
            let dir = dir_pb.as_path();
            match executor.search_files(dir, pattern, respect_gitignore).await {
                Ok((paths, res)) => {
                    let msg = if res.success {
                        let n = paths.len();
                        const SHOW: usize = 20;
                        let lines: Vec<String> = paths.iter().take(SHOW).map(|p| p.display().to_string()).collect();
                        let listing = lines.join("\n");
                        let mut m = format!(
                            "[search_files] count: {}\ndir: {}\npattern: {}\n",
                            n,
                            dir.display(),
                            pattern
                        );
                        if n == 0 {
                            m.push_str("matches: (none — 0 files)\n");
                        } else {
                            m.push_str("paths:\n");
                            m.push_str(&listing);
                            m.push('\n');
                            if n > SHOW {
                                m.push_str(&format!(
                                    "showing: {} of {} — narrow pattern or dir to list fewer\n",
                                    SHOW, n
                                ));
                            }
                        }
                        m
                    } else {
                        format!("[search_files] failed: {}", res.summary)
                    };
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[search_files] failed: {}", e), None),
            }
        }
        "grep_content" => {
            let (args, respect_gitignore, use_regex) = strip_file_search_flags(args);
            let raw_dir = args.get(0).map(String::as_str).unwrap_or(".");
            let dir_pb = resolve_tool_disk_path(raw_dir, workspace_root);
            let dir = dir_pb.as_path();
            let pattern = args.get(1).map(String::as_str).unwrap_or("");
            let file_glob = args.get(2).map(String::as_str).filter(|s| !s.is_empty());
            if pattern.is_empty() {
                return (
                    false,
                    "[grep_content] usage: grep_content <dir> <pattern> [file_glob] [--regex] [--no-ignore]".to_string(),
                    None,
                );
            }
            match executor
                .grep_content(dir, pattern, file_glob, 50, use_regex, respect_gitignore)
                .await
            {
                Ok((matches, res)) => {
                    let msg = if res.success {
                        let collected = matches.len();
                        const DISPLAY: usize = 30;
                        const CAP: usize = 50;
                        if collected == 0 {
                            format!(
                                "[grep_content] 0 matches (dir={} pattern={} regex={})",
                                dir.display(),
                                pattern,
                                use_regex
                            )
                        } else {
                            let lines: Vec<String> = matches
                                .iter()
                                .take(DISPLAY)
                                .map(|(p, n, line)| format!("{}:{}: {}", p.display(), n, line.trim()))
                                .collect();
                            let listing = lines.join("\n");
                            let mut m = format!(
                                "[grep_content] matches_collected: {} (showing up to {} lines)\n{}",
                                collected, DISPLAY, listing
                            );
                            if collected >= CAP {
                                m.push_str(&format!(
                                    "\n(at least {} matches — output capped; narrow pattern, dir, or file_glob)\n",
                                    CAP
                                ));
                            }
                            m
                        }
                    } else {
                        format!("[grep_content] failed: {}", res.summary)
                    };
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[grep_content] failed: {}", e), None),
            }
        }
        "write_file" | "write_code" => {
            let code_only = matches!(tool_name, "write_code");
            let usage_tag = if code_only { "write_code" } else { "write_file" };
            let Some((path_str, content)) = parse_write_file_request(args) else {
                return (
                    false,
                    format!(
                        "[{usage_tag}] usage: {usage_tag} <path> then file content on following lines"
                    ),
                    None,
                );
            };
            let content = strip_markdown_fences_from_write_content(&content);
            let ext_check_path = path_for_agent_write_extension_check(&path_str);
            if code_only && !crate::api_studio::path_has_agent_code_extension(&ext_check_path) {
                return (
                    false,
                    "[write_code] path must use a source-code extension (e.g. .ts, .tsx, .rs, .py); use write_file for markdown, JSON, or config files.".to_string(),
                    None,
                );
            }
            if is_workspace_virtual_path(&path_str) {
                match workspace_store {
                    Some(ws) => {
                        let key = path_str
                            .trim_start_matches("workspace:/")
                            .trim_start_matches("workspace:")
                            .trim_start_matches('/')
                            .to_string();
                        let mut key = key.trim().trim_matches('`').trim_matches('"').to_string();
                        if key.ends_with('#') {
                            key.pop();
                        }
                        let lineage_task_id = workspace_lineage_root_task_id(task_id, store_path);
                        let (key, _) =
                            rewrite_workspace_plan_key_to_lineage_root(&key, lineage_task_id);
                        let mut guard = ws.write().await;
                        let per_task = guard.entry(lineage_task_id).or_default();
                        let previous_mem = per_task.get(&key).cloned().unwrap_or_default();
                        // Only prefer the longer previous content for plan-trace files
                        // (.akasha/plan_*.md) where provider truncation is a known risk.
                        // For all other files, always use the new content so legitimate
                        // shortening edits (e.g. removing placeholder sections) are honoured.
                        let is_plan_trace = key.starts_with(".akasha/plan_") && key.ends_with(".md");
                        let effective_content = if is_plan_trace
                            && !previous_mem.is_empty()
                            && content.chars().count() < previous_mem.chars().count()
                        {
                            previous_mem.clone()
                        } else {
                            content.clone()
                        };
                        per_task.insert(key.clone(), effective_content.clone());
                        drop(guard);
                        // Also write to disk under data_dir (workspace root = store_path.parent())
                        let rel_path = Path::new(&key);
                        if executor.policy.can_write(rel_path) {
                            let disk_path_opt = workspace_root.map(|root| root.join(&key))
                                .or_else(|| std::env::current_dir().ok().map(|cwd| cwd.join(&key)))
                                .map(strip_verbatim_prefix);
                            if let Some(disk_path) = disk_path_opt {
                                if let Some(parent) = disk_path.parent() {
                                    let _ = tokio::fs::create_dir_all(parent).await;
                                }
                                let run_pollution_check = code_only
                                    || workspace_root
                                        .map(|r| {
                                            crate::studio::is_strictly_under_studio_root(&disk_path, r)
                                        })
                                        .unwrap_or(false);
                                if run_pollution_check {
                                    if let Some(msg) =
                                        crate::api_studio::studio_reject_polluted_code_content(
                                            &disk_path,
                                            &effective_content,
                                        )
                                    {
                                        return (false, format!("[{usage_tag}] {}", msg), None);
                                    }
                                }
                                if tokio::fs::write(&disk_path, &effective_content).await.is_ok() {
                                    return (
                                        true,
                                        format!("[{usage_tag} workspace:{}] saved (disk).", key),
                                        None,
                                    );
                                }
                            }
                        }
                        return (
                            true,
                            format!("[{usage_tag} workspace:{}] saved.", key),
                            None,
                        );
                    }
                    None => {
                        return (
                            false,
                            format!(
                                "[{usage_tag}] workspace paths require a workspace store."
                            ),
                            None,
                        );
                    }
                }
            }
            let disk_path = resolve_tool_disk_path(path_str.trim(), workspace_root);
            let run_pollution_check = code_only
                || workspace_root
                    .map(|root| crate::studio::is_strictly_under_studio_root(&disk_path, root))
                    .unwrap_or(false);
            if run_pollution_check {
                if let Some(msg) =
                    crate::api_studio::studio_reject_polluted_code_content(&disk_path, &content)
                {
                    return (false, format!("[{usage_tag}] {}", msg), None);
                }
            }
            match executor.write_file(&disk_path, &content).await {
                Ok(res) => {
                    let msg = if res.success {
                        format!("[{usage_tag} {}] {}", disk_path.display(), res.summary)
                    } else {
                        format!("[{usage_tag}] {}", res.summary)
                    };
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[{usage_tag}] error: {}", e), None),
            }
        }
        "delete_file" => {
            let path_str = path_arg_joined(args);
            if path_str.is_empty() {
                return (
                    false,
                    "[delete_file] usage: delete_file <path>".to_string(),
                    None,
                );
            }
            let path_str = normalize_tool_path_hint(path_str.trim());
            if path_str.contains("..") {
                return (
                    false,
                    "[delete_file] invalid path (..)".to_string(),
                    None,
                );
            }
            if is_workspace_virtual_path(&path_str) {
                match workspace_store {
                    Some(ws) => {
                        let key = path_str
                            .trim_start_matches("workspace:/")
                            .trim_start_matches("workspace:")
                            .trim_start_matches('/')
                            .to_string();
                        let mut key = key.trim().trim_matches('`').trim_matches('"').to_string();
                        if key.ends_with('#') {
                            key.pop();
                        }
                        let lineage_task_id = workspace_lineage_root_task_id(task_id, store_path);
                        let (key, _) =
                            rewrite_workspace_plan_key_to_lineage_root(&key, lineage_task_id);
                        {
                            let mut guard = ws.write().await;
                            if let Some(per_task) = guard.get_mut(&lineage_task_id) {
                                per_task.remove(&key);
                            }
                        }
                        let rel_path = Path::new(&key);
                        if !executor.policy.can_write(rel_path) {
                            return (
                                false,
                                "[delete_file] path not allowed by policy".to_string(),
                                None,
                            );
                        }
                        let disk_path_opt = workspace_root
                            .map(|root| root.join(&key))
                            .or_else(|| std::env::current_dir().ok().map(|cwd| cwd.join(&key)))
                            .map(strip_verbatim_prefix);
                        if let Some(disk_path) = disk_path_opt {
                            if disk_path.is_dir() {
                                return (
                                    false,
                                    "[delete_file] path is a directory".to_string(),
                                    None,
                                );
                            }
                            if let Some(r) = workspace_root {
                                if crate::studio::is_strictly_under_studio_root(&disk_path, r)
                                    && !disk_path.exists()
                                {
                                    return (
                                        true,
                                        format!("[delete_file workspace:{}] absent (disk).", key),
                                        None,
                                    );
                                }
                            }
                            match tokio::fs::remove_file(&disk_path).await {
                                Ok(()) => {
                                    return (
                                        true,
                                        format!("[delete_file workspace:{}] deleted (disk).", key),
                                        None,
                                    );
                                }
                                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                                    return (
                                        true,
                                        format!("[delete_file workspace:{}] absent (disk).", key),
                                        None,
                                    );
                                }
                                Err(e) => {
                                    return (
                                        false,
                                        format!("[delete_file] {}", e),
                                        None,
                                    );
                                }
                            }
                        }
                        return (
                            true,
                            format!("[delete_file workspace:{}] removed from workspace store.", key),
                            None,
                        );
                    }
                    None => {
                        return (
                            false,
                            "[delete_file] workspace paths require a workspace store.".to_string(),
                            None,
                        );
                    }
                }
            }
            let disk_path = resolve_tool_disk_path(path_str.trim(), workspace_root);
            if disk_path.is_dir() {
                return (
                    false,
                    "[delete_file] path is a directory".to_string(),
                    None,
                );
            }
            if !executor.policy.can_write(&disk_path) {
                return (
                    false,
                    "[delete_file] path not allowed by policy".to_string(),
                    None,
                );
            }
            if let Some(root) = workspace_root {
                if crate::studio::is_strictly_under_studio_root(&disk_path, root)
                    && !disk_path.exists()
                {
                    return (
                        true,
                        format!("[delete_file {}] absent.", disk_path.display()),
                        None,
                    );
                }
            }
            match tokio::fs::remove_file(&disk_path).await {
                Ok(()) => (
                    true,
                    format!("[delete_file {}] deleted.", disk_path.display()),
                    None,
                ),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (
                    true,
                    format!("[delete_file {}] absent.", disk_path.display()),
                    None,
                ),
                Err(e) => (false, format!("[delete_file] {}", e), None),
            }
        }
        "rename_path" => {
            if args.len() < 2 {
                return (
                    false,
                    "[rename_path] usage: rename_path <from> <to> — disque ou workspace:/ ; la cible est tout le texte après le premier argument (espaces OK dans <to>)."
                        .to_string(),
                    None,
                );
            }
            let from_s = normalize_tool_path_hint(args[0].trim());
            let to_s = normalize_tool_path_hint(args[1..].join(" ").trim());
            if from_s.is_empty() || to_s.is_empty() {
                return (
                    false,
                    "[rename_path] usage: rename_path <from> <to>".to_string(),
                    None,
                );
            }
            if from_s.contains("..") || to_s.contains("..") {
                return (
                    false,
                    "[rename_path] invalid path (..)".to_string(),
                    None,
                );
            }
            let from_is_workspace = is_workspace_virtual_path(&from_s);
            let to_is_workspace = is_workspace_virtual_path(&to_s);

            if from_is_workspace || to_is_workspace {
                if !(from_is_workspace && to_is_workspace) {
                    return (
                        false,
                        "[rename_path] both paths must use workspace:/ or neither".to_string(),
                        None,
                    );
                }
                match workspace_store {
                    Some(ws) => {
                        let normalize_ws_key = |s: &str| -> String {
                            let k = s
                                .trim_start_matches("workspace:/")
                                .trim_start_matches("workspace:")
                                .trim_start_matches('/');
                            let mut k = k.trim().trim_matches('`').trim_matches('"').to_string();
                            if k.ends_with('#') { k.pop(); }
                            k
                        };
                        let lineage_id = workspace_lineage_root_task_id(task_id, store_path);
                        let (from_key, _) = rewrite_workspace_plan_key_to_lineage_root(
                            &normalize_ws_key(&from_s), lineage_id,
                        );
                        let (to_key, _) = rewrite_workspace_plan_key_to_lineage_root(
                            &normalize_ws_key(&to_s), lineage_id,
                        );
                        // Phase 1: check preconditions and remove the key from the store under a
                        // brief write lock (acts as a reservation). The lock is released before
                        // the potentially-slow disk I/O so that unrelated read_file/write_file
                        // operations are not blocked.
                        let reserve_result = {
                            let mut guard = ws.write().await;
                            let per_task = guard.entry(lineage_id).or_default();
                            if per_task.contains_key(&to_key) {
                                Err(format!("[rename_path] destination workspace key already exists: {}", to_key))
                            } else if let Some(content) = per_task.remove(&from_key) {
                                Ok(Some(content))
                            } else {
                                Ok(None) // key absent from store
                            }
                            // write lock dropped here
                        };

                        match reserve_result {
                            Err(msg) => (false, msg, None),
                            Ok(Some(content)) => {
                                // Phase 2: disk rename without holding the lock.
                                let disk_result = if let Some(root) = workspace_root {
                                    let from_disk = root.join(&from_key);
                                    let to_disk = root.join(&to_key);
                                    match executor.rename_path(&from_disk, &to_disk).await {
                                        Ok(res) if !res.success => Err(res.summary),
                                        Ok(_) => Ok(()),
                                        Err(e) => Err(e.to_string()),
                                    }
                                } else {
                                    Ok(()) // no disk backing — in-memory rename only
                                };

                                // Phase 3: commit to_key or rollback from_key.
                                let mut guard = ws.write().await;
                                let per_task = guard.entry(lineage_id).or_default();
                                match disk_result {
                                    Ok(()) => {
                                        per_task.insert(to_key.clone(), content);
                                        (true, format!("[rename_path] workspace key renamed: {} -> {}", from_key, to_key), None)
                                    }
                                    Err(msg) => {
                                        per_task.insert(from_key.clone(), content);
                                        (false, format!("[rename_path] {}", msg), None)
                                    }
                                }
                            }
                            Ok(None) => {
                                // Key absent from store — try disk rename directly.
                                if let Some(root) = workspace_root {
                                    let from_disk = root.join(&from_key);
                                    let to_disk = root.join(&to_key);
                                    match executor.rename_path(&from_disk, &to_disk).await {
                                        Ok(res) => {
                                            let msg = format!("[rename_path] {}", res.summary);
                                            (res.success, msg, None)
                                        }
                                        Err(e) => (false, format!("[rename_path] {}", e), None),
                                    }
                                } else {
                                    (false, format!("[rename_path] workspace key not found: {}", from_key), None)
                                }
                            }
                        }
                    }
                    None => (false, "[rename_path] workspace paths require a workspace store.".to_string(), None),
                }
            } else {
                let from_disk = resolve_tool_disk_path(&from_s, workspace_root);
                let to_disk = resolve_tool_disk_path(&to_s, workspace_root);
                match executor.rename_path(&from_disk, &to_disk).await {
                    Ok(res) => {
                        let msg = format!("[rename_path] {}", res.summary);
                        (res.success, msg, None)
                    }
                    Err(e) => (false, format!("[rename_path] {}", e), None),
                }
            }
        }
        "move_tree" => {
            if args.len() < 2 {
                return (
                    false,
                    "[move_tree] usage: move_tree <from_dir> <to_dir> — répertoire source uniquement ; la cible est tout le texte après le premier argument."
                        .to_string(),
                    None,
                );
            }
            let from_s = normalize_tool_path_hint(args[0].trim());
            let to_s = normalize_tool_path_hint(args[1..].join(" ").trim());
            if from_s.is_empty() || to_s.is_empty() {
                return (
                    false,
                    "[move_tree] usage: move_tree <from_dir> <to_dir>".to_string(),
                    None,
                );
            }
            if from_s.contains("..") || to_s.contains("..") {
                return (
                    false,
                    "[move_tree] invalid path (..)".to_string(),
                    None,
                );
            }
            let from_s = rewrite_workspace_plan_path_str(&from_s, task_id, store_path);
            let to_s = rewrite_workspace_plan_path_str(&to_s, task_id, store_path);
            let from_disk = resolve_tool_disk_path(&from_s, workspace_root);
            let to_disk = resolve_tool_disk_path(&to_s, workspace_root);
            match executor.move_tree(&from_disk, &to_disk).await {
                Ok(res) => {
                    // After a successful disk move, rename all workspace store keys whose paths
                    // fall under the moved directory prefix (mirrors rename_path workspace sync).
                    if res.success && is_workspace_virtual_path(&from_s) && is_workspace_virtual_path(&to_s) {
                        if let Some(ws) = workspace_store {
                            let lineage_id = workspace_lineage_root_task_id(task_id, store_path);
                            let from_prefix = from_s
                                .trim_start_matches("workspace:/")
                                .trim_start_matches("workspace:")
                                .trim_start_matches('/')
                                .trim_end_matches('/')
                                .to_string();
                            let to_prefix = to_s
                                .trim_start_matches("workspace:/")
                                .trim_start_matches("workspace:")
                                .trim_start_matches('/')
                                .trim_end_matches('/')
                                .to_string();
                            let subtree_prefix = format!("{from_prefix}/");
                            let mut guard = ws.write().await;
                            let per_task = guard.entry(lineage_id).or_default();
                            // Collect moves first to avoid holding a mutable + immutable borrow.
                            let to_rename: Vec<(String, String, String)> = per_task
                                .iter()
                                .filter_map(|(key, val)| {
                                    if *key == from_prefix {
                                        // Exact directory entry.
                                        Some((key.clone(), to_prefix.clone(), val.clone()))
                                    } else if let Some(suffix) = key.strip_prefix(&subtree_prefix) {
                                        // Entry inside the subtree: suffix is the part after the "/".
                                        Some((key.clone(), format!("{to_prefix}/{suffix}"), val.clone()))
                                    } else {
                                        None
                                    }
                                })
                                .collect();
                            for (old_key, new_key, content) in to_rename {
                                per_task.remove(&old_key);
                                per_task.insert(new_key, content);
                            }
                        }
                    }
                    let msg = format!("[move_tree] {}", res.summary);
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[move_tree] {}", e), None),
            }
        }
        "search_replace" => {
            let path_str = match args.get(0) {
                Some(s) => s.as_str(),
                None => return (
                    false,
                    "[search_replace] usage: search_replace <path> <old_snippet> | <new_snippet> — delimiter must be SPACE PIPE SPACE (` | `); one TOOL line".to_string(),
                    None,
                ),
            };
            let is_workspace = path_str.starts_with("workspace:/") || path_str.starts_with("workspace:");
            let path_str_rewritten = rewrite_workspace_plan_path_str(path_str, task_id, store_path);
            let path_str = path_str_rewritten.as_str();
            let disk_path = resolve_tool_disk_path(path_str, workspace_root);
            let (search, replace) = match parse_search_replace_payload(args) {
                Ok(pair) => pair,
                Err("usage") => {
                    return (
                        false,
                        "[search_replace] usage: search_replace <path> <old_snippet> | <new_snippet> — use delimiter ` | ` (space-pipe-space) between the exact text to find and the replacement. Example: TOOL: search_replace workspace:/src/App.tsx const x = 1 | const x = 2".to_string(),
                        None,
                    );
                }
                Err("empty_search") => {
                    return (
                        false,
                        "[search_replace] search string is empty — the tokenizer often emits `|` as its own token after the path, so the payload must start with the OLD text to find, then ` | `, then the NEW text (not `| …` right after the path). Example: TOOL: search_replace workspace:/f.tsx oldLine | newLine".to_string(),
                        None,
                    );
                }
                Err(_) => {
                    return (
                        false,
                        "[search_replace] could not parse old/new segments".to_string(),
                        None,
                    );
                }
            };
            match executor.search_replace(&disk_path, &search, &replace).await {
                Ok(res) => {
                    if res.success && is_workspace {
                        sync_workspace_store_from_disk(workspace_store, task_id, store_path, path_str, &disk_path).await;
                    }
                    let msg = if res.success {
                        format!("[search_replace {}] {}", disk_path.display(), res.summary)
                    } else {
                        format!("[search_replace] {}", res.summary)
                    };
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[search_replace] error: {}", e), None),
            }
        }
        "edit_file" => {
            let path_str = match args.get(0) {
                Some(s) => s.as_str(),
                None => return (false, "[edit_file] usage: edit_file <path> <start_line> <end_line> <new_content>".to_string(), None),
            };
            let is_workspace = path_str.starts_with("workspace:/") || path_str.starts_with("workspace:");
            let path_str_rewritten = rewrite_workspace_plan_path_str(path_str, task_id, store_path);
            let path_str = path_str_rewritten.as_str();
            let disk_path = resolve_tool_disk_path(path_str, workspace_root);
            let start_line = args.get(1).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
            let end_line = args.get(2).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
            let new_content = args.get(3..).map(|a| a.join("\n")).unwrap_or_default();
            match executor.edit_file(&disk_path, start_line, end_line, &new_content).await {
                Ok(res) => {
                    if res.success && is_workspace {
                        sync_workspace_store_from_disk(workspace_store, task_id, store_path, path_str, &disk_path).await;
                    }
                    let msg = if res.success {
                        format!("[edit_file {}] {}", disk_path.display(), res.summary)
                    } else {
                        format!("[edit_file] {}", res.summary)
                    };
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[edit_file] error: {}", e), None),
            }
        }
        "apply_patch" => {
            let path_str = match args.get(0) {
                Some(s) => s.as_str(),
                None => return (false, "[apply_patch] usage: apply_patch <path> <patch_content>".to_string(), None),
            };
            let is_workspace = path_str.starts_with("workspace:/") || path_str.starts_with("workspace:");
            let path_str_rewritten = rewrite_workspace_plan_path_str(path_str, task_id, store_path);
            let path_str = path_str_rewritten.as_str();
            let disk_path = resolve_tool_disk_path(path_str, workspace_root);
            let patch_content = args.get(1..).map(|a| a.join("\n")).unwrap_or_default();
            match executor.apply_patch(&disk_path, &patch_content).await {
                Ok(res) => {
                    if res.success && is_workspace {
                        sync_workspace_store_from_disk(workspace_store, task_id, store_path, path_str, &disk_path).await;
                    }
                    let msg = if res.success {
                        format!("[apply_patch {}] {}", disk_path.display(), res.summary)
                    } else {
                        format!("[apply_patch] {}", res.summary)
                    };
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[apply_patch] error: {}", e), None),
            }
        }
        "file_diff" => {
            let path_a_str = args.get(0).map(String::as_str).unwrap_or("");
            let path_b_str = args.get(1).map(String::as_str).unwrap_or("");
            if path_a_str.is_empty() || path_b_str.is_empty() {
                return (false, "[file_diff] usage: file_diff <path_a> <path_b>".to_string(), None);
            }
            let disk_a = resolve_tool_disk_path(path_a_str, workspace_root);
            let disk_b = resolve_tool_disk_path(path_b_str, workspace_root);
            match executor.file_diff(&disk_a, &disk_b).await {
                Ok((diff, res)) => {
                    let msg = if res.success {
                        let preview = if diff.len() <= 400 { diff.as_str() } else { &diff[..diff.floor_char_boundary(400)] };
                        format!("[file_diff] {} — {}", res.summary, preview)
                    } else {
                        format!("[file_diff] {}", res.summary)
                    };
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[file_diff] error: {}", e), None),
            }
        }
        "diff_unified" => {
            let path_a_str = args.get(0).map(String::as_str).unwrap_or("");
            let path_b_str = args.get(1).map(String::as_str).unwrap_or("");
            if path_a_str.is_empty() || path_b_str.is_empty() {
                return (false, "[diff_unified] usage: diff_unified <path_a> <path_b> [context_lines]".to_string(), None);
            }
            let ctx = args
                .get(2)
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(3);
            let disk_a = resolve_tool_disk_path(path_a_str, workspace_root);
            let disk_b = resolve_tool_disk_path(path_b_str, workspace_root);
            match executor.file_diff_unified(&disk_a, &disk_b, ctx).await {
                Ok((diff, res)) => {
                    let msg = if res.success {
                        let preview = if diff.len() <= 800 {
                            diff.as_str()
                        } else {
                            &diff[..diff.floor_char_boundary(800)]
                        };
                        format!("[diff_unified] {} — {}", res.summary, preview)
                    } else {
                        format!("[diff_unified] {}", res.summary)
                    };
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[diff_unified] error: {}", e), None),
            }
        }
        "dir_compare" => {
            let dir_a_str = args.get(0).map(String::as_str).unwrap_or("");
            let dir_b_str = args.get(1).map(String::as_str).unwrap_or("");
            if dir_a_str.is_empty() || dir_b_str.is_empty() {
                return (false, "[dir_compare] usage: dir_compare <dir_a> <dir_b> [max_depth] [max_files]".to_string(), None);
            }
            let max_depth = args
                .get(2)
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(8);
            let max_files = args
                .get(3)
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(100);
            let disk_a = resolve_tool_disk_path(dir_a_str, workspace_root);
            let disk_b = resolve_tool_disk_path(dir_b_str, workspace_root);
            match executor
                .compare_dirs(&disk_a, &disk_b, max_depth, max_files)
                .await
            {
                Ok((report, res)) => {
                    let msg = if res.success {
                        let preview = if report.len() <= 1200 {
                            report.as_str()
                        } else {
                            &report[..report.floor_char_boundary(1200)]
                        };
                        format!("[dir_compare] {} — {}", res.summary, preview)
                    } else {
                        format!("[dir_compare] {}", res.summary)
                    };
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[dir_compare] error: {}", e), None),
            }
        }
        "git_status" => {
            let repo_str = args.get(0).map(String::as_str).unwrap_or("");
            if repo_str.is_empty() {
                return (false, "[git_status] usage: git_status <repo>".to_string(), None);
            }
            let disk = resolve_tool_disk_path(repo_str, workspace_root);
            match executor.git_status(&disk).await {
                Ok((out, res)) => {
                    let text = out.trim();
                    let (frag, total, trunc) =
                        crate::tool_output::truncate_utf8_by_bytes(text, crate::tool_output::GIT_TEXT_MAX);
                    let base = format!("[git_status] {} — {}", res.summary, frag);
                    let msg = crate::tool_output::with_truncation_footer(
                        base,
                        trunc,
                        total,
                        "output truncated — run git status in run_command with > file if you need the full text",
                    );
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[git_status] failed: {}", e), None),
            }
        }
        "git_diff" => {
            let repo_str = args.get(0).map(String::as_str).unwrap_or("");
            if repo_str.is_empty() {
                return (false, "[git_diff] usage: git_diff <repo> [--staged] [pathspec...]".to_string(), None);
            }
            let disk = resolve_tool_disk_path(repo_str, workspace_root);
            let mut staged = false;
            let mut rest_start = 1usize;
            if args.get(1).map(|s| s.as_str()) == Some("--staged") {
                staged = true;
                rest_start = 2;
            }
            let pathspecs: Vec<String> = args.get(rest_start..).map(|s| s.to_vec()).unwrap_or_default();
            match executor.git_diff(&disk, staged, &pathspecs).await {
                Ok((out, res)) => {
                    let text = out.trim();
                    let (frag, total, trunc) =
                        crate::tool_output::truncate_utf8_by_bytes(text, crate::tool_output::GIT_TEXT_MAX);
                    let base = format!("[git_diff] {} — {}", res.summary, frag);
                    let msg = crate::tool_output::with_truncation_footer(
                        base,
                        trunc,
                        total,
                        "narrow with pathspec arguments on git_diff or use file_diff for two files",
                    );
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[git_diff] failed: {}", e), None),
            }
        }
        "git_log" => {
            let repo_str = args.get(0).map(String::as_str).unwrap_or("");
            if repo_str.is_empty() {
                return (false, "[git_log] usage: git_log <repo> [n]".to_string(), None);
            }
            let n = args.get(1).and_then(|s| s.parse::<u32>().ok()).unwrap_or(20);
            let disk = resolve_tool_disk_path(repo_str, workspace_root);
            match executor.git_log(&disk, n).await {
                Ok((out, res)) => {
                    let text = out.trim();
                    let (frag, total, trunc) =
                        crate::tool_output::truncate_utf8_by_bytes(text, crate::tool_output::GIT_TEXT_MAX);
                    let base = format!("[git_log] {} — {}", res.summary, frag);
                    let msg = crate::tool_output::with_truncation_footer(
                        base,
                        trunc,
                        total,
                        "reduce n on git_log or save to a file and read_file with grep_content",
                    );
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[git_log] failed: {}", e), None),
            }
        }
        "git_rev_parse" => {
            let repo_str = args.get(0).map(String::as_str).unwrap_or("");
            if repo_str.is_empty() {
                return (false, "[git_rev_parse] usage: git_rev_parse <repo>".to_string(), None);
            }
            let disk = resolve_tool_disk_path(repo_str, workspace_root);
            match executor.git_rev_parse_head(&disk).await {
                Ok((out, res)) => {
                    let text = out.trim();
                    let (frag, total, trunc) =
                        crate::tool_output::truncate_utf8_by_bytes(text, crate::tool_output::GIT_TEXT_MAX);
                    let base = format!("[git_rev_parse] {} — {}", res.summary, frag);
                    let msg = crate::tool_output::with_truncation_footer(
                        base,
                        trunc,
                        total,
                        "output truncated — retry via run_command if needed",
                    );
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[git_rev_parse] failed: {}", e), None),
            }
        }
        "web_fetch" => {
            let url = args.get(0).map(String::as_str).unwrap_or("");
            if url.is_empty() {
                return (false, "[web_fetch] usage: web_fetch <url>".to_string(), None);
            }
            match executor.web_fetch(url).await {
                Ok((body, res)) => {
                    let msg = if res.success {
                        let (preview, total, trunc) = crate::tool_output::truncate_utf8_by_bytes(&body, 500);
                        let base = format!("[web_fetch] {} — {}", res.summary, preview);
                        crate::tool_output::with_truncation_footer(
                            base,
                            trunc,
                            total,
                            "use a more specific URL or browser snapshot for long pages",
                        )
                    } else {
                        format!("[web_fetch] failed: {}", res.summary)
                    };
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[web_fetch] failed: {}", e), None),
            }
        }
        "web_search" => {
            let (query, max_results) = if args.len() >= 2 {
                let last = args.last().unwrap();
                if last.parse::<u32>().is_ok() {
                    (args[..args.len() - 1].join(" "), last.parse().unwrap_or(5))
                } else {
                    (args.join(" "), 5u32)
                }
            } else {
                (args.join(" "), 5u32)
            };
            let query = query.trim();
            if query.is_empty() {
                return (false, "[web_search] usage: web_search <query> [max_results]".to_string(), None);
            }
            match executor.web_search(query.trim(), max_results).await {
                Ok((body, res)) => {
                    let msg = if res.success {
                        let (preview, total, trunc) = crate::tool_output::truncate_utf8_by_bytes(&body, 600);
                        let base = format!("[web_search] {} — {}", res.summary, preview);
                        crate::tool_output::with_truncation_footer(
                            base,
                            trunc,
                            total,
                            "use web_fetch on a result URL for full article text",
                        )
                    } else {
                        format!("[web_search] failed: {}", res.summary)
                    };
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[web_search] failed: {}", e), None),
            }
        }
        "web_crawl" => {
            let url = args.get(0).map(String::as_str).unwrap_or("").trim();
            if url.is_empty() {
                return (false, "[web_crawl] usage: web_crawl <url> [limit]".to_string(), None);
            }
            let limit = args
                .get(1)
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(50);
            match executor.web_crawl(url, limit).await {
                Ok((body, res)) => {
                    let msg = if res.success {
                        format!("[web_crawl] {}\n{}", res.summary, body)
                    } else {
                        format!("[web_crawl] {}", res.summary)
                    };
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[web_crawl] {}", e), None),
            }
        }
        "web_crawl_status" => {
            let job_id = args.get(0).map(String::as_str).unwrap_or("").trim();
            if job_id.is_empty() {
                return (
                    false,
                    "[web_crawl_status] usage: web_crawl_status <job_id>".to_string(),
                    None,
                );
            }
            match executor.web_crawl_job_status(job_id).await {
                Ok((body, res)) => {
                    let (preview, total, trunc) =
                        crate::tool_output::truncate_utf8_by_bytes(&body, 24_000);
                    let msg = if res.success {
                        crate::tool_output::with_truncation_footer(
                            format!("[web_crawl_status] {} — {}", res.summary, preview),
                            trunc,
                            total,
                            "response truncated; poll again if job still running",
                        )
                    } else {
                        format!("[web_crawl_status] {}", res.summary)
                    };
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[web_crawl_status] {}", e), None),
            }
        }
        "search_skills_catalog" => {
            let (query, max) = if args.len() >= 2 {
                let last = args.last().unwrap();
                if last.parse::<usize>().is_ok() {
                    (args[..args.len() - 1].join(" "), last.parse().unwrap_or(10))
                } else {
                    (args.join(" "), 10usize)
                }
            } else {
                (args.join(" "), 10usize)
            };
            if query.trim().is_empty() {
                return (false, "[search_skills_catalog] usage: search_skills_catalog <query> [max]".to_string(), None);
            }
            match executor.search_skills_catalog(query.trim(), max).await {
                Ok((body, res)) => (res.success, format!("[search_skills_catalog] {} — {}", res.summary, body), None),
                Err(e) => (false, format!("[search_skills_catalog] failed: {}", e), None),
            }
        }
        "github_repo_info" => {
            let owner = args.get(0).map(String::as_str).unwrap_or("").trim();
            let repo = args.get(1).map(String::as_str).unwrap_or("").trim();
            if owner.is_empty() || repo.is_empty() {
                return (false, "[github_repo_info] usage: github_repo_info <owner> <repo>".to_string(), None);
            }
            match executor.github_repo_info(owner, repo).await {
                Ok((body, res)) => (res.success, format!("[github_repo_info] {} — {}", res.summary, body), None),
                Err(e) => (false, format!("[github_repo_info] failed: {}", e), None),
            }
        }
        "analyze_table" => {
            let action = args.get(0).map(String::as_str).unwrap_or("").trim();
            let path = path_arg(1);
            if action.is_empty() || path.is_none() {
                return (false, "[analyze_table] usage: analyze_table <inspect|summary> <path.csv>".to_string(), None);
            }
            match executor.analyze_table(action, path.as_ref().unwrap()).await {
                Ok((body, res)) => (res.success, format!("[analyze_table] {} — {}", res.summary, body), None),
                Err(e) => (false, format!("[analyze_table] failed: {}", e), None),
            }
        }
        "arxiv_search" => {
            let (query, max) = if args.len() >= 2 {
                let last = args.last().unwrap();
                if last.parse::<u32>().is_ok() {
                    (args[..args.len() - 1].join(" "), last.parse().unwrap_or(10))
                } else {
                    (args.join(" "), 10u32)
                }
            } else {
                (args.join(" "), 10u32)
            };
            if query.trim().is_empty() {
                return (false, "[arxiv_search] usage: arxiv_search <query> [max]".to_string(), None);
            }
            match executor.arxiv_search(query.trim(), max).await {
                Ok((body, res)) => (res.success, format!("[arxiv_search] {} — {}", res.summary, body), None),
                Err(e) => (false, format!("[arxiv_search] failed: {}", e), None),
            }
        }
        "http_probe" => {
            let method = args.get(0).map(String::as_str).unwrap_or("GET").trim();
            let url = args.get(1).map(String::as_str).unwrap_or("").trim();
            let body_arg = if args.len() > 2 { Some(args[2..].join(" ")) } else { None };
            if url.is_empty() {
                return (false, "[http_probe] usage: http_probe <METHOD> <url> [body]".to_string(), None);
            }
            match executor.http_probe(method, url, body_arg.as_deref()).await {
                Ok((body, res)) => (res.success, format!("[http_probe] {} — {}", res.summary, body), None),
                Err(e) => (false, format!("[http_probe] failed: {}", e), None),
            }
        }
        "run_in_container" => {
            let work_dir = path_arg(0);
            let image = args.get(1).map(String::as_str).unwrap_or("");
            let command = args.get(2).map(String::as_str).unwrap_or("");
            if work_dir.is_none() || image.is_empty() || command.is_empty() {
                return (false, "[run_in_container] usage: run_in_container <work_dir> <image> <command> [args...]".to_string(), None);
            }
            let work_dir = work_dir.unwrap();
            let cmd_args: Vec<String> = args.iter().skip(3).cloned().collect();
            match executor.run_in_container(work_dir, image, command, &cmd_args, 300, 512).await {
                Ok((stdout, stderr, exit_code, _res)) => {
                    let out = String::from_utf8_lossy(&stdout);
                    let err = String::from_utf8_lossy(&stderr);
                    let success = exit_code == 0;
                    (success, format!(
                        "[run_in_container] exit {} — stdout: {} stderr: {}",
                        exit_code,
                        if out.len() > 400 { format!("{}...", &out[..out.floor_char_boundary(400)]) } else { out.to_string() },
                        if err.len() > 200 { format!("{}...", &err[..err.floor_char_boundary(200)]) } else { err.to_string() }
                    ), None)
                }
                Err(e) => (false, format!("[run_in_container] error: {}", e), None),
            }
        }
        "device_discover" => {
            let interface = args.get(0).map(String::as_str).unwrap_or("").trim();
            let interfaces_to_list: Vec<String> = if interface.is_empty() {
                let allowed = &executor.policy.allowed_device_interfaces;
                let base: Vec<String> = if allowed.iter().any(|a| a.trim().eq_ignore_ascii_case("*")) {
                    vec!["local_media".to_string(), "system".to_string(), "synthetic_input".to_string()]
                } else {
                    allowed.clone()
                };
                // Always apply policy filter (respects blocked_device_interfaces)
                base.into_iter()
                    .filter(|iface| executor.policy.can_use_device_interface(iface))
                    .collect()
            } else {
                if !executor.policy.can_use_device_interface(interface) {
                    return (false, format!("[device_discover] interface '{}' not allowed by policy (allowed_device_interfaces / blocked_device_interfaces)", interface), None);
                }
                vec![interface.to_string()]
            };
            if !interface.is_empty()
                && !matches!(interface, "local_media" | "system" | "synthetic_input")
            {
                return (
                    false,
                    format!(
                        "[device_discover] interface '{interface}' is not supported (available: local_media, system, synthetic_input). Configure allowed_device_interfaces in tools_policy.yaml — see GET /api/docs."
                    ),
                    None,
                );
            }
            let mut devices: Vec<serde_json::Value> = Vec::new();
            let mut unsupported: Vec<String> = Vec::new();
            for iface in &interfaces_to_list {
                match iface.as_str() {
                    "local_media" => {
                        devices.push(serde_json::json!({ "interface": "local_media", "id": "camera", "name": "Camera" }));
                        devices.push(serde_json::json!({ "interface": "local_media", "id": "microphone", "name": "Microphone" }));
                        devices.push(serde_json::json!({ "interface": "local_media", "id": "speaker", "name": "Speaker" }));
                    }
                    "system" => {
                        devices.push(serde_json::json!({ "interface": "system", "id": "printer", "name": "System printers" }));
                    }
                    "synthetic_input" => {
                        devices.push(serde_json::json!({ "interface": "synthetic_input", "id": "keyboard", "name": "Keyboard (shortcuts, type)" }));
                        devices.push(serde_json::json!({ "interface": "synthetic_input", "id": "mouse", "name": "Mouse (move, click, scroll, drag)" }));
                    }
                    _ => unsupported.push(iface.clone()),
                }
            }
            let body = if unsupported.is_empty() {
                serde_json::json!({ "devices": devices })
            } else {
                serde_json::json!({ "devices": devices, "unsupported_interfaces": unsupported })
            };
            (true, format!("[device_discover] {} device(s): {}", devices.len(), body.to_string()), None)
        }
        "device_invoke" => {
            let interface = args.get(0).map(String::as_str).unwrap_or("");
            let device_id = args.get(1).map(String::as_str).unwrap_or("");
            let action = args.get(2).map(String::as_str).unwrap_or("");
            if interface.is_empty() || device_id.is_empty() || action.is_empty() {
                return (false, "[device_invoke] usage: device_invoke <interface> <device_id> <action> [params...]".to_string(), None);
            }
            if !executor.policy.can_use_device_interface(interface) {
                return (false, format!("[device_invoke] interface '{}' not allowed by policy", interface), None);
            }
            // Params:
            // - if a single 4th arg is valid JSON, use it; else {}
            // - if multiple params are provided, pass them as a JSON array of strings
            let params = parse_device_invoke_params(args);
            if interface == "local_media" {
                let bridge = match device_bridge {
                    Some(b) => b,
                    None => return (false, "[device_invoke] device bridge not available (no UI client for local_media)".to_string(), None),
                };
                let (request_id, rx) = bridge
                    .submit_request(interface.to_string(), device_id.to_string(), action.to_string(), params)
                    .await;
                let data_dir = store_path.and_then(|p| p.parent()).unwrap_or_else(|| std::path::Path::new("."));
                tracing::info!(request_id = %request_id, interface = %interface, device_id = %device_id, action = %action, "[device_bridge] submitted request, waiting for UI");
                const DEVICE_TIMEOUT_SECS: u64 = 120;
                match tokio::time::timeout(
                    std::time::Duration::from_secs(DEVICE_TIMEOUT_SECS),
                    rx,
                )
                .await
                {
                    Ok(Ok(result)) => {
                        tracing::info!(request_id = %request_id, success = result.success, "[device_bridge] received result from UI");
                        let msg = if result.success {
                            let data_preview = result.data.as_deref().map(|d| if d.len() > 200 { format!("{}...", &d[..d.floor_char_boundary(200)]) } else { d.to_string() }).unwrap_or_else(|| "ok".to_string());
                            format!("[device_invoke local_media {}] success — {}", action, data_preview)
                        } else {
                            format!("[device_invoke local_media {}] failed or refused", action)
                        };
                        // Save captured image to data_dir/captures and return base64 for UI display
                        let out_image_base64 = if result.success && action == "capture" {
                            if let Some(ref data) = result.data {
                                if !data.is_empty() {
                                    let captures_dir = data_dir.join("captures");
                                    let _ = std::fs::create_dir_all(&captures_dir);
                                    let filename = format!("photo_{}.jpg", chrono::Utc::now().format("%Y-%m-%d_%H-%M-%S"));
                                    let path = captures_dir.join(&filename);
                                    if let Ok(decoded) = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data) {
                                        let _ = std::fs::write(&path, decoded);
                                    }
                                    Some(data.clone())
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        } else {
                            None
                        };
                        (result.success, msg, out_image_base64)
                    }
                    Ok(Err(_)) => {
                        tracing::warn!(request_id = %request_id, "[device_bridge] channel closed without result");
                        (false, "[device_invoke] channel closed without result".to_string(), None)
                    }
                    Err(_) => {
                        tracing::warn!(request_id = %request_id, "[device_bridge] timeout, no POST /api/device/result received");
                        bridge.cancel(&request_id).await;
                        (false, format!("[device_invoke] timeout after {}s (no UI client responded)", DEVICE_TIMEOUT_SECS), None)
                    }
                }
            } else if interface == "synthetic_input" {
                let bridge = match device_bridge {
                    Some(b) => b,
                    None => return (false, "[device_invoke] device bridge not available (no UI client for synthetic_input)".to_string(), None),
                };
                let (request_id, rx) = bridge
                    .submit_request(interface.to_string(), device_id.to_string(), action.to_string(), params)
                    .await;
                const DEVICE_TIMEOUT_SECS: u64 = 120;
                match tokio::time::timeout(
                    std::time::Duration::from_secs(DEVICE_TIMEOUT_SECS),
                    rx,
                )
                .await
                {
                    Ok(Ok(result)) => {
                        let msg = if result.success {
                            let data_preview = result.data.as_deref().map(|d| if d.len() > 200 { format!("{}...", &d[..d.floor_char_boundary(200)]) } else { d.to_string() }).unwrap_or_else(|| "ok".to_string());
                            format!("[device_invoke synthetic_input {}] success — {}", action, data_preview)
                        } else {
                            format!("[device_invoke synthetic_input {}] failed or refused", action)
                        };
                        (result.success, msg, None)
                    }
                    Ok(Err(_)) => (false, "[device_invoke] channel closed without result".to_string(), None),
                    Err(_) => {
                        bridge.cancel(&request_id).await;
                        (false, format!("[device_invoke] timeout after {}s (no UI client responded)", DEVICE_TIMEOUT_SECS), None)
                    }
                }
            } else if interface == "system" {
                (
                    false,
                    "[device_invoke] system interface (printer/print) is not supported — OS-level printing is out of scope for Akasha. See GET /api/docs and tools_policy.yaml (allowed_device_interfaces).".to_string(),
                    None,
                )
            } else {
                (false, format!("[device_invoke] interface '{}' handler not yet implemented", interface), None)
            }
        }
        "generate_image" => {
            let (prompt, size_owned) = parse_generate_image_tool_args(args);
            let prompt = prompt.trim();
            if prompt.is_empty() {
                (false, "[generate_image] usage: generate_image <prompt> [size]".to_string(), None)
            } else {
                let size = size_owned.as_deref();
                let data_dir = store_path.and_then(|p| p.parent()).unwrap_or_else(|| Path::new("."));
                match crate::image_generation::generate_image_impl(data_dir, prompt, size).await {
                    Ok((msg, data_url)) => (true, msg, Some(data_url)),
                    Err(e) => (false, e, None),
                }
            }
        }
        "speech_synthesize" => {
            let text = args.get(0).map(String::as_str).unwrap_or("").trim();
            if text.is_empty() {
                (false, "[speech_synthesize] usage: speech_synthesize <text>".to_string(), None)
            } else {
                let data_dir = store_path.and_then(|p| p.parent()).unwrap_or_else(|| Path::new("."));
                match crate::voice::speech_synthesize_impl(data_dir, text).await {
                    Ok((msg, data_url)) => (true, msg, Some(data_url)),
                    Err(e) => (false, e, None),
                }
            }
        }
        "speech_transcribe" => {
            let audio_input = args.get(0).map(String::as_str).unwrap_or("").trim();
            let data_dir = store_path.and_then(|p| p.parent()).unwrap_or_else(|| Path::new("."));
            match crate::voice::speech_transcribe_impl(data_dir, audio_input).await {
                Ok(text) => (true, format!("[speech_transcribe] {}", text), None),
                Err(e) => (false, e, None),
            }
        }
        _ => {
            if let Some((plugin_id, plugin_payload)) = plugin_invocation {
                match plugin_registry {
                    Some(r) => {
                        let reg = Arc::clone(r);
                        let pid = plugin_id.clone();
                        let data_dir_for_plugin = store_path
                            .and_then(|p| p.parent())
                            .map(|p| p.to_path_buf());
                        let pl = if pid == "homeassistant" {
                            data_dir_for_plugin
                                .as_ref()
                                .map(|d| {
                                    crate::homeassistant_config::enrich_homeassistant_plugin_payload(
                                        d,
                                        &plugin_payload,
                                    )
                                })
                                .unwrap_or(plugin_payload)
                        } else {
                            plugin_payload
                        };
                        // #region agent log
                        debug_log(
                            "H3",
                            "crates/akasha-daemon/src/api.rs:5578",
                            "Spawning blocking plugin call task",
                            serde_json::json!({
                                "plugin_id": pid,
                                "payload_len": pl.len()
                            }),
                        );
                        // #endregion
                        match tokio::task::spawn_blocking(move || reg.call_tool(&pid, &pl)).await {
                            Ok(Ok(out)) => {
                                if out.trim().is_empty() {
                                    return (
                                        false,
                                        format!(
                                            "[plugin:{}] execution returned empty output (check plugin input/action)",
                                            plugin_id
                                        ),
                                        None,
                                    );
                                }
                                let plugin_ok = serde_json::from_str::<serde_json::Value>(&out)
                                    .ok()
                                    .and_then(|v| v.get("ok").and_then(|b| b.as_bool()));
                                let preview = if out.chars().count() > 600 {
                                    format!("{}…", out.chars().take(600).collect::<String>())
                                } else {
                                    out
                                };
                                if plugin_ok == Some(false) {
                                    return (
                                        false,
                                        format!(
                                            "[plugin:{}] execution failed: plugin returned ok=false: {}",
                                            plugin_id, preview
                                        ),
                                        None,
                                    );
                                }
                                (
                                    true,
                                    format!("[plugin:{}] {}", plugin_id, preview),
                                    None,
                                )
                            }
                            Ok(Err(err)) => (
                                false,
                                format!("[plugin:{}] execution failed: {}", plugin_id, err),
                                None,
                            ),
                            Err(join_err) => (
                                false,
                                format!(
                                    "[plugin:{}] execution failed: {}",
                                    plugin_id,
                                    if join_err.is_panic() {
                                        // #region agent log
                                        debug_log(
                                            "H3",
                                            "crates/akasha-daemon/src/api.rs:5629",
                                            "Blocking plugin task panicked",
                                            serde_json::json!({
                                                "plugin_id": plugin_id,
                                                "join_error": join_err.to_string()
                                            }),
                                        );
                                        // #endregion
                                        "internal error (plugin task panicked)".to_string()
                                    } else {
                                        join_err.to_string()
                                    }
                                ),
                                None,
                            ),
                        }
                    }
                    None => (
                        false,
                        format!(
                            "[plugin:{}] execution failed: plugin registry unavailable",
                            plugin_id
                        ),
                        None,
                    ),
                }
            } else {
                let names: Vec<&str> = AVAILABLE_TOOLS.iter().map(|(n, _)| *n).collect();
                (false, format!("[{}] unknown tool. For shell commands, use: TOOL: run_command <cmd> .... Available: {}.", tool_name, names.join(", ")), None)
            }
        }
    };
    result
}
