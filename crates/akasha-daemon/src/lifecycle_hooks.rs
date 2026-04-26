//! Optional shell hooks for automation (Hermes-style gateway hooks — phase 1: schedule fire only).
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
            "executed_phases": ["on_schedule_fire"],
            "note": "Gateway HTTP pre/post hooks are reserved; only on_schedule_fire is executed today."
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
        "executed_phases": ["on_schedule_fire"],
        "note": "on_http_request_* arrays are parsed for forward compatibility; they are not invoked on HTTP traffic yet."
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
