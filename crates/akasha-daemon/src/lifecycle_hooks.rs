//! Optional shell hooks for automation (gateway-style hooks — phase 1: schedule fire only).
//!
//! Config: `data_dir/lifecycle_hooks.json`
//! ```json
//! { "on_schedule_fire": [ ["powershell", "-NoProfile", "-Command", "Write-Host schedule"] ] }
//! ```
//! Each entry is a full argv array. Environment: `AKASHA_DATA_DIR`, `AKASHA_SCHEDULE_ID`, `AKASHA_TASK_ID`.

use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;
use uuid::Uuid;

fn hook_array_len(v: &Value, key: &str) -> usize {
    v.get(key)
        .and_then(|x| x.as_array())
        .map(|a| a.len())
        .unwrap_or(0)
}

/// Summary of `lifecycle_hooks.json` for `GET /api/lifecycle/hooks` (operator visibility).
pub fn lifecycle_hooks_summary(data_dir: &Path) -> Value {
    let path = data_dir.join("lifecycle_hooks.json");
    if !path.is_file() {
        return json!({
            "present": false,
            "path": path.display().to_string(),
            "executed_phases": ["on_schedule_fire", "on_http_request_pre", "on_http_request_post"],
            "note": "HTTP gateway: on_http_request_pre awaited at request entry; on_http_request_post spawned when the handler returns (see gateway-shell-hooks.md)."
        });
    }
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            return json!({
                "present": false,
                "path": path.display().to_string(),
                "read_error": e.to_string()
            });
        }
    };
    let v: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            return json!({
                "present": true,
                "path": path.display().to_string(),
                "invalid_json": true,
                "detail": e.to_string()
            });
        }
    };
    json!({
        "present": true,
        "path": path.display().to_string(),
        "on_schedule_fire": hook_array_len(&v, "on_schedule_fire"),
        "on_http_request_pre": hook_array_len(&v, "on_http_request_pre"),
        "on_http_request_post": hook_array_len(&v, "on_http_request_post"),
        "executed_phases": ["on_schedule_fire", "on_http_request_pre", "on_http_request_post"],
        "note": "on_http_request_pre runs at API entry; on_http_request_post runs on Drop after response is built. Timeouts: AKASHA_GATEWAY_HOOK_TIMEOUT_SECS (default 3). Sandbox flag: AKASHA_GATEWAY_HOOK_SANDBOX."
    })
}

pub fn fire_on_schedule_fire_async(data_dir: &Path, schedule_id: Uuid, task_id: Uuid) {
    let path = data_dir.join("lifecycle_hooks.json");
    let data_dir = data_dir.to_path_buf();
    let sid = schedule_id.to_string();
    let tid = task_id.to_string();
    tokio::spawn(async move {
        let raw = match tokio::fs::read_to_string(&path).await {
            Ok(raw) => raw,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
            Err(_) => {
                tracing::warn!(path = %path.display(), "lifecycle_hooks: read failed");
                return;
            }
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
            tracing::warn!(path = %path.display(), "lifecycle_hooks: invalid JSON");
            return;
        };
        let Some(arr) = v.get("on_schedule_fire").and_then(|x| x.as_array()) else {
            return;
        };
        if arr.is_empty() {
            return;
        }
        let hooks: Vec<Vec<String>> = arr
            .iter()
            .filter_map(|entry| {
                entry.as_array().map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect::<Vec<_>>()
                })
            })
            .filter(|v| !v.is_empty())
            .collect();
        if hooks.is_empty() {
            return;
        }
        for argv in hooks {
            let prog = argv[0].clone();
            let rest: Vec<String> = argv[1..].to_vec();
            let dd = data_dir.clone();
            let sid = sid.clone();
            let tid = tid.clone();
            match tokio::time::timeout(
                Duration::from_secs(30),
                tokio::process::Command::new(&prog)
                    .args(&rest)
                    .env("AKASHA_DATA_DIR", dd.as_os_str())
                    .env("AKASHA_SCHEDULE_ID", &sid)
                    .env("AKASHA_TASK_ID", &tid)
                    .output(),
            )
            .await
            {
                Err(_) => {
                    tracing::warn!(prog = %prog, "lifecycle_hooks: hook timed out after 30s");
                }
                Ok(Err(e)) => {
                    tracing::warn!(prog = %prog, error = %e, "lifecycle_hooks: hook spawn failed");
                }
                Ok(Ok(out)) if !out.status.success() => {
                    tracing::warn!(
                        prog = %prog,
                        exit_code = ?out.status.code(),
                        stderr = %String::from_utf8_lossy(&out.stderr).chars().take(512).collect::<String>(),
                        "lifecycle_hooks: hook exited with non-zero status"
                    );
                }
                Ok(Ok(_)) => {}
            }
        }
    });
}

fn gateway_hook_timeout() -> Duration {
    std::env::var("AKASHA_GATEWAY_HOOK_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(Duration::from_secs)
        .filter(|d| *d > Duration::ZERO)
        .unwrap_or_else(|| Duration::from_secs(3))
}

fn gateway_sandbox_flag() -> String {
    std::env::var("AKASHA_GATEWAY_HOOK_SANDBOX").unwrap_or_else(|_| "none".into())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HookSandboxMode {
    None,
    Strict,
}

fn gateway_sandbox_mode() -> HookSandboxMode {
    match gateway_sandbox_flag().to_ascii_lowercase().as_str() {
        "strict" => HookSandboxMode::Strict,
        _ => HookSandboxMode::None,
    }
}

fn is_hook_command_allowed(program: &str, mode: HookSandboxMode) -> bool {
    if mode == HookSandboxMode::None {
        return true;
    }
    let p = std::path::Path::new(program);
    let name = p
        .file_name()
        .and_then(|x| x.to_str())
        .unwrap_or(program)
        .to_ascii_lowercase();
    matches!(
        name.as_str(),
        "python" | "python3" | "node" | "pwsh" | "powershell" | "bash" | "sh"
    )
}

async fn load_http_hook_argv(data_dir: &Path, key: &str) -> Vec<Vec<String>> {
    let path = data_dir.join("lifecycle_hooks.json");
    let raw = match tokio::fs::read_to_string(&path).await {
        Ok(s) => s,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(_) => return Vec::new(),
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Vec::new();
    };
    let Some(arr) = v.get(key).and_then(|x| x.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|entry| {
            entry.as_array().map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect::<Vec<_>>()
            })
        })
        .filter(|v| !v.is_empty())
        .collect()
}

async fn run_http_hook_array(
    data_dir: &Path,
    hooks: &[Vec<String>],
    method: &str,
    path: &str,
) {
    if hooks.is_empty() {
        return;
    }
    let timeout_d = gateway_hook_timeout();
    let sandbox = gateway_sandbox_flag();
    let sandbox_mode = gateway_sandbox_mode();
    for argv in hooks {
        let prog = argv[0].clone();
        if !is_hook_command_allowed(&prog, sandbox_mode) {
            tracing::warn!(prog = %prog, sandbox = %sandbox, "gateway_http_hooks: command denied by strict sandbox");
            continue;
        }
        let rest: Vec<String> = argv[1..].to_vec();
        let dd = data_dir.to_path_buf();
        let method = method.to_string();
        let path = path.to_string();
        match tokio::time::timeout(
            timeout_d,
            tokio::process::Command::new(&prog)
                .args(&rest)
                .env("AKASHA_DATA_DIR", dd.as_os_str())
                .env("AKASHA_HTTP_METHOD", &method)
                .env("AKASHA_HTTP_PATH", &path)
                .env("AKASHA_GATEWAY_HOOK_SANDBOX", &sandbox)
                .output(),
        )
        .await
        {
            Err(_) => tracing::warn!(prog = %prog, timeout = ?timeout_d, "gateway_http_hooks: timed out"),
            Ok(Err(e)) => tracing::warn!(prog = %prog, error = %e, "gateway_http_hooks: spawn failed"),
            Ok(Ok(out)) if !out.status.success() => tracing::warn!(
                prog = %prog,
                exit_code = ?out.status.code(),
                stderr = %String::from_utf8_lossy(&out.stderr).chars().take(512).collect::<String>(),
                "gateway_http_hooks: non-zero exit"
            ),
            Ok(Ok(_)) => {}
        }
    }
}

/// Run `on_http_request_pre` hooks from `lifecycle_hooks.json` (blocking up to timeout per hook).
pub async fn run_http_request_pre_hooks(data_dir: &Path, method: &str, path: &str) {
    let hooks = load_http_hook_argv(data_dir, "on_http_request_pre").await;
    run_http_hook_array(data_dir, &hooks, method, path).await;
}

async fn run_http_request_post_hooks(data_dir: &Path, method: &str, path: &str) {
    let hooks = load_http_hook_argv(data_dir, "on_http_request_post").await;
    run_http_hook_array(data_dir, &hooks, method, path).await;
}

/// When dropped at the end of `handle_api`, spawns `on_http_request_post` hooks (best-effort).
pub struct HttpPostLifecycleHooks {
    pub data_dir: std::path::PathBuf,
    pub method: String,
    pub path: String,
}

impl Drop for HttpPostLifecycleHooks {
    fn drop(&mut self) {
        let dd = self.data_dir.clone();
        let m = self.method.clone();
        let p = self.path.clone();
        if let Ok(h) = tokio::runtime::Handle::try_current() {
            h.spawn(async move {
                run_http_request_post_hooks(&dd, &m, &p).await;
            });
        }
    }
}
