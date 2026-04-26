//! Optional shell hooks for automation (Hermes-style gateway hooks — phase 1: schedule fire only).
//!
//! Config: `data_dir/lifecycle_hooks.json`
//! ```json
//! { "on_schedule_fire": [ ["powershell", "-NoProfile", "-Command", "Write-Host schedule"] ] }
//! ```
//! Each entry is a full argv array. Environment: `AKASHA_DATA_DIR`, `AKASHA_SCHEDULE_ID`, `AKASHA_TASK_ID`.

use std::path::Path;
use std::time::Duration;
use uuid::Uuid;

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
