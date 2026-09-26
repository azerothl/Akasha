//! Ops / system HTTP routes: migrate, chat title, metrics, agents, doctor, vault, restart,
//! user-rag, complete, metrics summary, diagnostic advice.

use crate::api::{packaged_spec_check_ok, RestartTx};
use crate::api_http::json_response;
use crate::protocol_adapter::unknown_external_message_count;
use akasha_llm::CompletionRequest;
use akasha_store::{TaskStatus, TaskStore};
use akasha_vault::Vault;
use std::path::Path;

pub struct RouteCtx<'a> {
    pub data_dir: &'a Path,
    pub store_path: &'a Path,
    pub spec_dir: &'a Path,
    pub llm_router: &'a std::sync::Arc<akasha_llm::LLMRouter>,
    pub rag_pack: &'a std::sync::Arc<akasha_rag::RagPack>,
    pub ollama_base_url: Option<&'a str>,
    pub restart_tx: &'a RestartTx,
    pub user_rag_store: &'a crate::user_rag::SharedUserRagStore,
}

pub async fn try_handle(
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    ctx: &RouteCtx<'_>,
) -> Option<String> {
    let data_dir = ctx.data_dir;
    let store_path = ctx.store_path;
    let spec_dir = ctx.spec_dir;
    let llm_router = ctx.llm_router;
    let rag_pack = ctx.rag_pack;
    let ollama_base_url = ctx.ollama_base_url;
    let restart_tx = ctx.restart_tx;
    let user_rag_store = ctx.user_rag_store;

// Second brain controls: settings, overview and clear.

if method == "POST" && path == "/api/migrate/openclaw/preview" {
    let source_dir = body
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
        .and_then(|v| v.get("source_dir").and_then(|x| x.as_str()).map(|s| s.to_string()))
        .unwrap_or_default();
    if source_dir.trim().is_empty() {
        return Some(json_response("400 Bad Request", r#"{"error":"missing_source_dir"}"#));
    }
    match crate::openclaw_migration::preview(&source_dir) {
        Ok(p) => {
            return Some(json_response("200 OK", &serde_json::to_string(&p).unwrap_or_else(|_| "{}".into())));
        }
        Err(e) => {
            return Some(json_response(
                "400 Bad Request",
                &serde_json::json!({ "error": e }).to_string(),
            ));
        }
    }
}

if method == "POST" && path == "/api/migrate/openclaw/apply" {
    let parsed = body
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
    let source_dir = parsed
        .as_ref()
        .and_then(|v| v.get("source_dir").and_then(|x| x.as_str()))
        .unwrap_or("")
        .to_string();
    let dry_run = parsed
        .as_ref()
        .and_then(|v| v.get("dry_run").and_then(|x| x.as_bool()))
        .unwrap_or(false);
    let import_memory = parsed
        .as_ref()
        .and_then(|v| v.get("import_memory").and_then(|x| x.as_bool()))
        .unwrap_or(false);
    if source_dir.trim().is_empty() {
        return Some(json_response("400 Bad Request", r#"{"error":"missing_source_dir"}"#));
    }
    match crate::openclaw_migration::apply(&source_dir, data_dir, dry_run, import_memory) {
        Ok(r) => {
            return Some(json_response("200 OK", &serde_json::to_string(&r).unwrap_or_else(|_| "{}".into())));
        }
        Err(e) => {
            return Some(json_response(
                "400 Bad Request",
                &serde_json::json!({ "error": e }).to_string(),
            ));
        }
    }
}

// GET /api/memory/recall-metrics — counters from memory orchestrator (semantic recall hits/empty).

// POST /api/chat/suggest-thread-title — short title from first user message (UI chat threads)
if method == "POST" && path == "/api/chat/suggest-thread-title" {
    let body_json = body
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
    let message = body_json
        .as_ref()
        .and_then(|v| v.get("message").and_then(|v| v.as_str()))
        .unwrap_or("")
        .trim();
    if message.is_empty() {
        return Some(json_response("400 Bad Request", r#"{"error":"missing message"}"#));
    }
    fn fallback_title(msg: &str) -> String {
        const MAX: usize = 48;
        let t = msg.trim();
        let n = t.chars().count();
        if n <= MAX {
            t.to_string()
        } else {
            format!("{}…", t.chars().take(MAX).collect::<String>())
        }
    }
    let fallback = fallback_title(message);
    let prompt = format!(
        "Reply with ONLY a short conversation title (max 60 characters, no quotation marks, same language as the user message).\n\nUser message:\n{}",
        message
    );
    let req = CompletionRequest {
        prompt,
        max_tokens: Some(80),
        temperature: Some(0.3),
        preferred_task_type: Some("utility".to_string()),
        system_prompt: Some(
            "Output only the title text. No quotes. No leading 'Title:'.".to_string(),
        ),
        image_data_urls: None,
        top_p: None,
        top_k: None,
        frequency_penalty: None,
        presence_penalty: None,
        repeat_penalty: None,
        num_ctx: None,
        num_gpu: None,
        thinking_level: None,
    };
    let title_timeout = std::time::Duration::from_secs(15);
    let title = match tokio::time::timeout(title_timeout, llm_router.complete(&req)).await {
        Ok(Ok(resp)) => {
            let mut t = resp.text.trim().to_string();
            if let Some(i) = t.find('\n') {
                t.truncate(i);
            }
            t = t
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .trim()
                .to_string();
            if t.starts_with("Title:") || t.starts_with("Titre:") {
                t = t
                    .trim_start_matches("Title:")
                    .trim_start_matches("Titre:")
                    .trim()
                    .to_string();
            }
            if t.chars().count() > 60 {
                t = t.chars().take(60).collect();
            }
            if t.is_empty() {
                fallback
            } else {
                t
            }
        }
        _ => fallback,
    };
    let body = serde_json::json!({ "title": title });
    return Some(json_response("200 OK", &body.to_string()));
}


// GET /api/status — same as / but explicit for slash commands

// GET /api/timeline — unified timeline of recent events (Phase 5 AI OS). Query: ?limit=50&task_id=uuid (optional).

// GET /api/metrics — task counts and simple metrics (Phase 5 AI OS).
if method == "GET" && path == "/api/metrics" {
    let (pending, running, completed, failed, paused, interrupted) =
        match TaskStore::open(store_path) {
            Ok(store) => {
                let tasks = store.get_all().unwrap_or_default();
                let mut pending = 0;
                let mut running = 0;
                let mut completed = 0;
                let mut failed = 0;
                let mut paused = 0;
                let mut interrupted = 0;
                for t in &tasks {
                    match t.status {
                        TaskStatus::Pending
                        | TaskStatus::Queued
                        | TaskStatus::WaitingUserInput => pending += 1,
                        TaskStatus::Running => running += 1,
                        TaskStatus::Completed => completed += 1,
                        TaskStatus::Failed | TaskStatus::Cancelled => failed += 1,
                        TaskStatus::Paused => paused += 1,
                        TaskStatus::Interrupted => interrupted += 1,
                    }
                }
                (pending, running, completed, failed, paused, interrupted)
            }
            Err(_) => (0, 0, 0, 0, 0, 0),
        };
    let body_json = serde_json::json!({
        "tasks": { "pending": pending, "running": running, "completed": completed, "failed": failed, "paused": paused, "interrupted": interrupted },
        "stability": llm_router.metrics().stability_summary(),
        "protocol": { "unknown_message_count": unknown_external_message_count() }
    });
    return Some(json_response("200 OK", &body_json.to_string()));
}

// GET /api/agents — list known agent roles (Phase 6 AI OS cockpit).
if method == "GET" && path == "/api/agents" {
    const AGENT_ROLES: &[&str] = &[
        "conversation",
        "search",
        "code",
        "financial",
        "documentalist",
        "project_manager",
        "technical_writer",
        "research",
        "security_audit",
        "creative",
        "analyst",
        "architect",
        "frontend",
        "backend",
        "database",
        "integration",
        "qa",
        "system",
        "image_generation",
    ];
    let list: Vec<serde_json::Value> = AGENT_ROLES
        .iter()
        .map(|name| serde_json::json!({ "id": name, "name": name }))
        .collect();
    let body_json = serde_json::json!({ "agents": list });
    return Some(json_response("200 OK", &body_json.to_string()));
}

// GET /api/plugins — voir plus bas (Phase 5 PluginRegistry) : tableau JSON pour CLI/Tauri/TUI.
// Les skills chargeables pour agents sont sur GET /api/skills (pas de doublon ici).

// GET /api/doctor — health checks from daemon (for slash /doctor)
if method == "GET" && path == "/api/doctor" {
    if let Some(cached) = crate::http_get_cache::cache_get_doctor() {
        return Some(format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            cached.len(),
            cached
        ));
    }
    let mut checks: Vec<serde_json::Value> = Vec::new();
    checks.push(
        serde_json::json!({ "id": "daemon", "ok": true, "description": "Daemon running" }),
    );
    checks.push(
        serde_json::json!({ "id": "os", "ok": true, "description": std::env::consts::OS }),
    );

    let ollama_ok = if let Some(u) = ollama_base_url {
        let test_url = format!("{}/api/tags", u.trim_end_matches('/'));
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        client
            .get(&test_url)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    } else {
        false
    };
    checks.push(serde_json::json!({
        "id": "ollama",
        "ok": ollama_ok,
        "description": if ollama_ok { "Ollama reachable" } else { "Ollama unreachable" }
    }));

    if crate::connectors_config::homeassistant_enabled_in_file(data_dir) {
        let ha_url = crate::connectors_config::ha_base_url(data_dir);
        let ha_ok = if let Some(ref u) = ha_url {
            let test_url = format!("{}/api/", u.trim_end_matches('/'));
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(3))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new());
            client
                .get(&test_url)
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false)
        } else {
            false
        };
        let token_ok = akasha_vault::open_vault(data_dir)
            .ok()
            .and_then(|v| v.get("ha_access_token").ok())
            .map(|t| !t.trim().is_empty())
            .unwrap_or(false);
        checks.push(serde_json::json!({
            "id": "homeassistant",
            "ok": ha_ok,
            "description": if ha_ok {
                format!("Home Assistant reachable{}",
                    if token_ok { ", token configured" } else { ", token missing" })
            } else if ha_url.is_some() {
                "Home Assistant unreachable at configured URL".to_string()
            } else {
                "Home Assistant enabled but HA_BASE_URL not set (try akasha discover homeassistant)".to_string()
            }
        }));
    }

    let vault_ok = akasha_vault::open_vault(data_dir).is_ok();
    checks.push(serde_json::json!({
        "id": "vault",
        "ok": vault_ok,
        "description": if vault_ok { "Vault open" } else { "Vault error" }
    }));

    let spec_ok = packaged_spec_check_ok(spec_dir);
    checks.push(serde_json::json!({
        "id": "spec_dir",
        "ok": spec_ok,
        "description": if spec_dir.exists() {
            "Spec directory present"
        } else if std::env::var_os("AKASHA_SPEC_DIR").is_some() {
            "Spec directory missing"
        } else {
            "Spec directory not bundled (OK for installed binaries)"
        }
    }));

    #[cfg(feature = "embedded")]
    {
        let snap = llm_router.embedded_status();
        let ok = snap.ready_for_chat && snap.action.is_none();
        let mut check = serde_json::json!({
            "id": "embedded_llm",
            "ok": ok,
            "description": format!(
                "{} (backend {}, device {}, tier {})",
                snap.hint,
                snap.backend.as_deref().unwrap_or("?"),
                snap.device.as_deref().unwrap_or("?"),
                snap.hardware_tier.as_deref().unwrap_or("?")
            ),
            "gguf_present": snap.gguf_present,
            "llama_cpp_compiled": snap.llama_cpp_compiled,
            "ready_for_chat": snap.ready_for_chat,
            "calibration_done": snap.calibration_done,
            "hardware_tier": snap.hardware_tier,
            "active_engine_policy": snap.active_engine_policy
        });
        if let Some(action) = snap.action {
            check["action"] = serde_json::json!(action);
        }
        checks.push(check);
    }
    #[cfg(not(feature = "embedded"))]
    {
        checks.push(serde_json::json!({
            "id": "embedded_llm",
            "ok": false,
            "description": "Embedded model not available (compile with embedded feature)"
        }));
    }

    // Playwright managed browser (optional): runner path, npm package, node/npm on PATH
    let runner_path = crate::browser::find_playwright_runner_path();
    let runner_path_str = runner_path.as_ref().map(|p| p.display().to_string());
    let runner_ok = runner_path.is_some();
    let playwright_pkg_path = runner_path.as_ref().and_then(|p| {
        crate::browser::playwright_runner_dir(p)
            .map(|d| d.join("node_modules").join("playwright"))
    });
    let playwright_pkg_present = playwright_pkg_path
        .as_ref()
        .map(|p| p.is_dir())
        .unwrap_or(false);
    let auto_install_off = std::env::var("AKASHA_PLAYWRIGHT_AUTO_INSTALL")
        .map(|v| v == "0" || v.eq_ignore_ascii_case("false"))
        .unwrap_or(false);
    let playwright_pkg_ok = if playwright_pkg_present {
        true
    } else if !runner_ok {
        false
    } else {
        !auto_install_off
    };
    let playwright_pkg_desc = if playwright_pkg_present {
        "node_modules/playwright present"
    } else if !runner_ok {
        "Playwright runner not resolved"
    } else if auto_install_off {
        "node_modules/playwright missing (set AKASHA_PLAYWRIGHT_AUTO_INSTALL or run npm install in runner dir)"
    } else {
        "node_modules/playwright not installed yet (will auto-install on first browser use)"
    };

    let (node_on_path, node_version_line) = match tokio::time::timeout(
        std::time::Duration::from_secs(3),
        tokio::process::Command::new("node")
            .arg("--version")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output(),
    )
    .await
    {
        Ok(Ok(o)) if o.status.success() => (
            true,
            String::from_utf8(o.stdout)
                .ok()
                .map(|s| s.trim().to_string()),
        ),
        _ => (false, None),
    };

    let npm_exe = if cfg!(windows) { "npm.cmd" } else { "npm" };
    let npm_on_path = matches!(
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            tokio::process::Command::new(npm_exe)
                .arg("--version")
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .output(),
        )
        .await,
        Ok(Ok(ref o)) if o.status.success()
    );

    checks.push(serde_json::json!({
        "id": "playwright_runner",
        "ok": runner_ok,
        "description": if runner_ok {
            format!("Playwright runner: {}", runner_path_str.as_deref().unwrap_or("?"))
        } else {
            "Playwright runner not found (use release layout, AKASHA_PLAYWRIGHT_RUNNER, or data dir)".to_string()
        }
    }));
    checks.push(serde_json::json!({
        "id": "playwright_node",
        "ok": node_on_path,
        "description": if node_on_path {
            format!(
                "node on PATH ({})",
                node_version_line.as_deref().unwrap_or("?")
            )
        } else {
            "node not found on PATH (install Node.js for managed browser)".to_string()
        }
    }));
    checks.push(serde_json::json!({
        "id": "playwright_npm",
        "ok": npm_on_path,
        "description": if npm_on_path { "npm on PATH" } else { "npm not found on PATH" }
    }));
    checks.push(serde_json::json!({
        "id": "playwright_package",
        "ok": playwright_pkg_ok,
        "description": playwright_pkg_desc
    }));

    // Calendar / CalDAV (external events cache + sidecar sync status)
    let (cal_account_count, cal_accounts_err, cal_ext_count, cal_sync_desc) =
        match akasha_store::ExternalCalendarStore::open(store_path) {
            Ok(store) => {
                let accounts = store.list_accounts().unwrap_or_default();
                let err_count = accounts
                    .iter()
                    .filter(|a| a.last_sync_error.as_ref().is_some_and(|e| !e.is_empty()))
                    .count();
                let now = chrono::Utc::now();
                let ext_count = store
                    .list_events_between(
                        now - chrono::Duration::days(30),
                        now + chrono::Duration::days(90),
                        None,
                    )
                    .map(|v| v.len())
                    .unwrap_or(0);
                let sync_path = data_dir.join("calendar_sync_status.json");
                let sync_json: Option<serde_json::Value> = std::fs::read_to_string(&sync_path)
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok());
                let connected = sync_json
                    .as_ref()
                    .and_then(|j| j.get("connected").and_then(|v| v.as_bool()))
                    .unwrap_or(false);
                let last_err = sync_json
                    .as_ref()
                    .and_then(|j| j.get("last_error").and_then(|v| v.as_str()))
                    .unwrap_or("");
                let last_at = sync_json
                    .as_ref()
                    .and_then(|j| j.get("last_sync_at").and_then(|v| v.as_str()))
                    .unwrap_or("");
                let desc = if accounts.is_empty() {
                    format!(
                        "No CalDAV accounts ({ext_count} external events in cache; ICS import OK)"
                    )
                } else if !last_err.is_empty() {
                    format!(
                        "{} account(s), last sync error: {last_err}",
                        accounts.len()
                    )
                } else if connected {
                    format!(
                        "{} account(s), sidecar connected, last sync {last_at}",
                        accounts.len()
                    )
                } else {
                    format!(
                        "{} account(s), sidecar idle (run caldav-channel sidecar)",
                        accounts.len()
                    )
                };
                (accounts.len(), err_count, ext_count, desc)
            }
            Err(e) => (0, 0, 0, format!("External calendar store: {e}")),
        };
    let cal_sync_ok = cal_accounts_err == 0;
    checks.push(serde_json::json!({
        "id": "calendar_accounts",
        "ok": cal_sync_ok,
        "description": cal_sync_desc
    }));
    checks.push(serde_json::json!({
        "id": "calendar_external_events",
        "ok": true,
        "description": format!("External calendar events (next 90d window): {cal_ext_count}")
    }));
    if cal_account_count > 0 {
        let mut missing_pw = 0usize;
        if let Ok(vault) = akasha_vault::open_vault(data_dir) {
            if let Ok(store) = akasha_store::ExternalCalendarStore::open(store_path) {
                if let Ok(accounts) = store.list_accounts() {
                    for a in accounts {
                        let key = format!("caldav_{}_password", a.id);
                        if vault.get(&key).is_err() {
                            missing_pw += 1;
                        }
                    }
                }
            }
        }
        checks.push(serde_json::json!({
            "id": "calendar_vault_passwords",
            "ok": missing_pw == 0,
            "description": if missing_pw == 0 {
                "CalDAV vault passwords present for all accounts".to_string()
            } else {
                format!("{missing_pw} CalDAV account(s) missing vault key caldav_<id>_password")
            }
        }));
    }

    let playwright_json = serde_json::json!({
        "runner_path": runner_path_str,
        "runner_found": runner_ok,
        "node_modules_playwright": playwright_pkg_present,
        "auto_install_disabled": auto_install_off,
        "node_on_path": node_on_path,
        "node_version": node_version_line,
        "npm_on_path": npm_on_path,
    });

    let rbitnet_models_url = std::env::var("RBITNET_CHAT_BASE_URL")
        .ok()
        .map(|u| {
            let base = u.trim_end_matches('/');
            if base.ends_with("/v1") {
                format!("{base}/models")
            } else {
                format!("{base}/v1/models")
            }
        })
        .or_else(|| {
            std::env::var("RBITNET_BIND").ok().map(|b| {
                let host = b.trim();
                if host.starts_with("http://") || host.starts_with("https://") {
                    format!("{host}/v1/models")
                } else {
                    format!("http://{host}/v1/models")
                }
            })
        })
        .unwrap_or_else(|| "http://127.0.0.1:8080/v1/models".to_string());
    let rbitnet_ok = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
        .get(&rbitnet_models_url)
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false);
    checks.push(serde_json::json!({
        "id": "rbitnet",
        "ok": rbitnet_ok,
        "description": if rbitnet_ok {
            "Rbitnet reachable (local inference). Compare perf vs llama.cpp: see Rbitnet/docs/BENCHMARKS.md"
        } else {
            "Rbitnet unreachable (optional: rbitnet-server on RBITNET_BIND, default 127.0.0.1:8080)"
        },
        "models_url": rbitnet_models_url
    }));

    let all_ok = checks
        .iter()
        .all(|c| c.get("ok").and_then(|v| v.as_bool()).unwrap_or(false));
    let body_json =
        serde_json::json!({ "ok": all_ok, "checks": checks, "playwright": playwright_json })
            .to_string();
    crate::http_get_cache::cache_put_doctor(&body_json);
    return Some(format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body_json.len(),
        body_json
    ));
}

// GET /api/config — list vars from data_dir/akasha.env

// GET /api/vault/keys — list vault key names (no values)
if method == "GET" && path == "/api/vault/keys" {
    let keys = akasha_vault::open_vault(data_dir)
        .ok()
        .and_then(|v| v.list_keys().ok())
        .unwrap_or_default();
    let body_json = serde_json::json!({ "keys": keys }).to_string();
    return Some(format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body_json.len(),
        body_json
    ));
}

// POST /api/vault — set one secret (body: {"key": "KEY_NAME", "value": "secret"})
if method == "POST" && path == "/api/vault" {
    let body_json = body
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
    let key = body_json
        .as_ref()
        .and_then(|j| j.get("key"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let value = body_json
        .as_ref()
        .and_then(|j| j.get("value"))
        .and_then(|v| v.as_str())
        .map(String::from);
    match (key, value) {
        (Some(k), Some(v)) if !k.is_empty() && !v.is_empty() => {
            match akasha_vault::open_vault(data_dir) {
                Ok(vault) => match vault.set(&k, &v) {
                    Ok(()) => {
                        return Some(json_response(
                            "200 OK",
                            &serde_json::json!({ "ok": true, "key": k }).to_string(),
                        ));
                    }
                    Err(e) => {
                        return Some(json_response(
                            "500 Internal Server Error",
                            &serde_json::json!({ "error": e.to_string() }).to_string(),
                        ));
                    }
                },
                Err(e) => {
                    return Some(json_response(
                        "503 Service Unavailable",
                        &serde_json::json!({ "error": e.to_string() }).to_string(),
                    ));
                }
            }
        }
        _ => return Some(json_response("400 Bad Request", r#"{"error":"missing key or value"}"#)),
    }
}

// DELETE /api/vault — remove one key (body: {"key": "KEY_NAME"})
if method == "DELETE" && path == "/api/vault" {
    let body_json = body
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
    let key = body_json
        .as_ref()
        .and_then(|j| j.get("key"))
        .and_then(|v| v.as_str())
        .map(String::from);
    match key {
        Some(k) if !k.is_empty() => match akasha_vault::open_vault(data_dir) {
            Ok(v) => match v.delete(&k) {
                Ok(()) => {
                    return Some(json_response(
                        "200 OK",
                        &serde_json::json!({ "ok": true, "key": k }).to_string(),
                    ));
                }
                Err(akasha_vault::VaultError::NotFound(_)) => {
                    return Some(json_response(
                        "404 Not Found",
                        &serde_json::json!({ "error": "not_found", "key": k }).to_string(),
                    ));
                }
                Err(e) => {
                    return Some(json_response(
                        "500 Internal Server Error",
                        &serde_json::json!({ "error": e.to_string() }).to_string(),
                    ));
                }
            },
            Err(e) => {
                return Some(json_response(
                    "503 Service Unavailable",
                    &serde_json::json!({ "error": e.to_string() }).to_string(),
                ));
            }
        },
        _ => return Some(json_response("400 Bad Request", r#"{"error":"missing or empty key"}"#)),
    }
}

// POST /api/restart — signal daemon to exit (supervisor restarts it)
if method == "POST" && path == "/api/restart" {
    if let Some(ref tx) = restart_tx {
        let _ = tx.send(()).await;
        return Some(json_response("200 OK", r#"{"ok":true,"message":"Redémarrage demandé"}"#));
    }
    return Some(json_response(
        "503 Service Unavailable",
        r#"{"error":"restart_not_available"}"#,
    ));
}

// User RAG: list documents
if method == "GET" && path == "/api/user-rag/documents" {
    let store = user_rag_store.lock().await;
    match store.list_documents() {
        Ok(docs) => {
            let body = serde_json::to_string(&serde_json::json!({ "documents": docs }))
                .unwrap_or_else(|_| "[]".to_string());
            return Some(json_response("200 OK", &body));
        }
        Err(e) => {
            let body = serde_json::json!({ "error": "list_failed", "detail": e.to_string() });
            return Some(json_response("500 Internal Server Error", &body.to_string()));
        }
    }
}

// User RAG: upload document (body: { name, content_base64, mime_type? })
if method == "POST" && path == "/api/user-rag/documents" {
    let body_json = body
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
    let name = body_json
        .as_ref()
        .and_then(|j| j.get("name"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let content_base64 = body_json
        .as_ref()
        .and_then(|j| j.get("content_base64"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let mime_type = body_json
        .as_ref()
        .and_then(|j| j.get("mime_type"))
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| "application/octet-stream".to_string());
    let name = match name.filter(|n| !n.is_empty()) {
        Some(n) => n,
        None => return Some(json_response("400 Bad Request", r#"{"error":"name_required"}"#)),
    };
    let content_base64 = match content_base64.filter(|c| !c.is_empty()) {
        Some(c) => c,
        None => {
            return Some(json_response("400 Bad Request", r#"{"error":"content_base64_required"}"#));
        }
    };
    let store = user_rag_store.lock().await;
    match store.add_document(&content_base64, &name, &mime_type) {
        Ok(id) => {
            let data_dir = store_path.parent().unwrap_or(store_path).to_path_buf();
            crate::api_routes_kinbot::spawn_user_rag_index(data_dir, id.clone());
            let body =
                serde_json::json!({ "id": id, "name": name, "message": "Document ajouté.", "index_status": "pending" });
            return Some(json_response("200 OK", &body.to_string()));
        }
        Err(e) => {
            let body = serde_json::json!({ "error": "add_failed", "detail": e.to_string() });
            return Some(json_response("500 Internal Server Error", &body.to_string()));
        }
    }
}

// User RAG: delete document by id
if method == "DELETE" && path.starts_with("/api/user-rag/documents/") {
    let id = path
        .trim_start_matches("/api/user-rag/documents/")
        .split('?')
        .next()
        .unwrap_or("")
        .trim();
    if id.is_empty() {
        return Some(json_response("400 Bad Request", r#"{"error":"id_required"}"#));
    }
    let store = user_rag_store.lock().await;
    match store.delete_document(id) {
        Ok(true) => {
            return Some(json_response("200 OK", r#"{"ok":true,"message":"Document supprimé."}"#));
        }
        Ok(false) => {
            return Some(json_response("404 Not Found", r#"{"error":"document_not_found"}"#));
        }
        Err(e) => {
            let body = serde_json::json!({ "error": "delete_failed", "detail": e.to_string() });
            return Some(json_response("500 Internal Server Error", &body.to_string()));
        }
    }
}

// Phase 6: LLM completion via router
if method == "POST" && path == "/api/complete" {
    let body = match body
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
    {
        Some(b) => b,
        None => return Some(json_response("400 Bad Request", r#"{"error":"invalid_json"}"#)),
    };
    let prompt = body
        .get("prompt")
        .or(body.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let req = akasha_llm::CompletionRequest {
        prompt,
        max_tokens: body
            .get("max_tokens")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32),
        temperature: body
            .get("temperature")
            .and_then(|v| v.as_f64())
            .map(|f| f as f32),
        preferred_task_type: None,
        system_prompt: None,
        image_data_urls: None,
        top_p: None,
        top_k: None,
        frequency_penalty: None,
        presence_penalty: None,
        repeat_penalty: None,
        num_ctx: None,
        num_gpu: None,
        thinking_level: None,
    };
    match llm_router.complete(&req).await {
        Ok(resp) => {
            let latency_ms = resp
                .total_duration_ns
                .map(|ns| ns / 1_000_000)
                .unwrap_or(0);
            let body = serde_json::json!({
                "text": resp.text,
                "model_used": resp.model_used,
                "usage": resp.usage,
                "cost_usd": resp.cost_usd,
                "latency_ms": latency_ms,
            });
            return Some(json_response("200 OK", &body.to_string()));
        }
        Err(e) => {
            let body = serde_json::json!({ "error": "completion_failed", "detail": e });
            return Some(json_response("502 Bad Gateway", &body.to_string()));
        }
    }
}


// GET /api/metrics/summary — metrics with latency percentiles (P50, P95, P99) per provider/model
if method == "GET" && path == "/api/metrics/summary" {
    let summary = llm_router.metrics().summary();
    let body = serde_json::to_string(&summary).unwrap_or_else(|_| "{}".to_string());
    return Some(json_response("200 OK", &body));
}


// Phase 8: Diagnostic advice (RAG + core model, guardrails)
if (method == "POST" || method == "GET") && path == "/api/diagnostic/advice" {
    let health_json: serde_json::Value = body
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
        .and_then(|v: serde_json::Value| v.get("health").cloned())
        .unwrap_or_else(|| serde_json::json!({ "checks": [] }));
    let health_str = serde_json::to_string_pretty(&health_json).unwrap_or_default();
    let all_ok = health_json
        .get("ok")
        .and_then(|v| v.as_bool())
        .unwrap_or_else(|| {
            health_json
                .get("checks")
                .and_then(|c| c.as_array())
                .map(|a| {
                    a.iter()
                        .all(|c| c.get("ok").and_then(|v| v.as_bool()).unwrap_or(false))
                })
                .unwrap_or(false)
        });
    let summary = if all_ok {
        "→ All checks PASSED; no action required."
    } else {
        "→ Some checks FAILED; suggest fixes only for those."
    };
    let chunks = akasha_rag::retrieve(rag_pack, "diagnostic health runbook error daemon", 5);
    let doc_context: String = chunks
        .iter()
        .map(|c| format!("--- {} ---\n{}", c.path, c.content))
        .fold(String::new(), |a, b| a + "\n" + &b);
    let prompt = format!(
        r#"You are the Akasha diagnostic assistant. The JSON below is the ACTUAL current health state of the system (just ran). Each check has "ok": true (working) or "ok": false (missing/failed).

RULES:
- Base your answer ONLY on this state. Do NOT suggest fixing or starting something that already has "ok": true (e.g. if "daemon_health" is ok: true, the daemon IS running — do not suggest starting it).
- If ALL checks have "ok": true, say clearly that everything is OK and that no action is required; you may add one optional tip (e.g. run "akasha start --foreground" to see logs).
- If some checks have "ok": false, list only those and suggest 1–3 concrete, safe steps from the runbooks.
- Never recommend destructive actions. Do not expose secrets.

Documentation (runbooks/specs):
{}
---
Current health state (actual, JSON):
{}
{}
Reply in the same language as the user (or French if ambiguous). Be concise."#,
        doc_context.trim(),
        health_str,
        summary
    );
    let req = akasha_llm::CompletionRequest {
        prompt,
        max_tokens: Some(512),
        temperature: Some(0.3),
        preferred_task_type: None,
        system_prompt: None,
        image_data_urls: None,
        top_p: None,
        top_k: None,
        frequency_penalty: None,
        presence_penalty: None,
        repeat_penalty: None,
        num_ctx: None,
        num_gpu: None,
        thinking_level: None,
    };
    let advice_timeout = std::time::Duration::from_secs(120);
    match tokio::time::timeout(advice_timeout, llm_router.complete(&req)).await {
        Ok(Ok(resp)) => {
            let body =
                serde_json::json!({ "advice": resp.text, "model_used": resp.model_used });
            return Some(json_response("200 OK", &body.to_string()));
        }
        Ok(Err(e)) => {
            let body = serde_json::json!({ "error": "advice_failed", "detail": e.to_string() });
            return Some(json_response("502 Bad Gateway", &body.to_string()));
        }
        Err(_) => {
            let body = serde_json::json!({
                "error": "advice_timeout",
                "detail": "LLM timed out (120s). Embedded model may still be loading; try /embedded to check."
            });
            return Some(json_response("504 Gateway Timeout", &body.to_string()));
        }
    }
}

    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn ops_paths_smoke() {
        assert!(("/api/migrate/openclaw/preview", "POST").ok());
        assert!(("/api/migrate/openclaw/apply", "POST").ok());
        assert!(("/api/chat/suggest-thread-title", "POST").ok());
        assert!(("/api/metrics", "GET").ok());
        assert!(("/api/agents", "GET").ok());
        assert!(("/api/doctor", "GET").ok());
        assert!(("/api/vault/keys", "GET").ok());
        assert!(("/api/vault", "POST").ok());
        assert!(("/api/vault", "DELETE").ok());
        assert!(("/api/restart", "POST").ok());
        assert!(("/api/user-rag/documents", "GET").ok());
        assert!(("/api/user-rag/documents", "POST").ok());
        assert!(("/api/user-rag/documents/x", "DELETE").ok());
        assert!(("/api/complete", "POST").ok());
        assert!(("/api/metrics/summary", "GET").ok());
        assert!(("/api/diagnostic/advice", "POST").ok());
        assert!(!("/api/tasks", "GET").ok());
    }

    trait PathSmoke {
        fn ok(self) -> bool;
    }
    impl PathSmoke for (&str, &str) {
        fn ok(self) -> bool {
            let (p, _m) = self;
            p.starts_with("/api/migrate/openclaw/")
                || p == "/api/chat/suggest-thread-title"
                || p == "/api/metrics"
                || p == "/api/agents"
                || p == "/api/doctor"
                || p == "/api/vault/keys"
                || p == "/api/vault"
                || p == "/api/restart"
                || p == "/api/user-rag/documents"
                || p.starts_with("/api/user-rag/documents/")
                || p == "/api/complete"
                || p == "/api/metrics/summary"
                || p == "/api/diagnostic/advice"
        }
    }
}
