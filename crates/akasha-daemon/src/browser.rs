//! Browser automation session (spec 39). One Playwright-backed session per task.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::RwLock;
use uuid::Uuid;

/// Registry of browser sessions by task_id. When a task completes, call close_task to free the session.
pub type BrowserSessionRegistry = Arc<RwLock<HashMap<Uuid, BrowserSession>>>;

pub struct BrowserSession {
    _child: Child,
    stdin: Option<tokio::process::ChildStdin>,
    stdout: BufReader<tokio::process::ChildStdout>,
}

impl BrowserSession {
    /// Send a JSON command and read one JSON response line.
    pub async fn send_command(&mut self, cmd: &serde_json::Value) -> Result<serde_json::Value, String> {
        let line = serde_json::to_string(cmd).map_err(|e| e.to_string())?;
        if let Some(ref mut stdin) = self.stdin {
            stdin.write_all(line.as_bytes()).await.map_err(|e| e.to_string())?;
            stdin.write_all(b"\n").await.map_err(|e| e.to_string())?;
            stdin.flush().await.map_err(|e| e.to_string())?;
        }
        let mut buf = String::new();
        let n = self.stdout.read_line(&mut buf).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("Runner closed stdout".to_string());
        }
        let trimmed = buf.trim();
        serde_json::from_str(trimmed).map_err(|e| format!("Invalid JSON from runner: {}", e))
    }

    /// Send close and drop the process.
    pub async fn close(&mut self) -> Result<(), String> {
        let _ = self.send_command(&serde_json::json!({ "cmd": "close" })).await;
        self.stdin.take();
        Ok(())
    }
}

/// Resolve path to playwright-runner run.mjs. Tries AKASHA_PLAYWRIGHT_RUNNER, then scripts/playwright-runner/run.mjs from cwd or exe parent.
pub fn find_playwright_runner_path() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("AKASHA_PLAYWRIGHT_RUNNER") {
        let path = std::path::PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
        let run = path.join("run.mjs");
        if run.is_file() {
            return Some(run);
        }
        return Some(path.join("run.mjs"));
    }
    let cwd = std::env::current_dir().ok()?;
    let from_cwd = cwd.join("scripts").join("playwright-runner").join("run.mjs");
    if from_cwd.is_file() {
        return Some(from_cwd);
    }
    let from_parent = cwd.join("..").join("scripts").join("playwright-runner").join("run.mjs");
    if from_parent.is_file() {
        return Some(from_parent.canonicalize().unwrap_or(from_parent));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let from_target = parent
                .join("..")
                .join("..")
                .join("scripts")
                .join("playwright-runner")
                .join("run.mjs");
            if from_target.is_file() {
                return Some(from_target);
            }
        }
    }
    None
}

/// Start a new browser session (spawns Node runner, sends init).
pub async fn create_browser_session(
    runner_path: &Path,
    headless: bool,
    action_timeout_secs: u64,
) -> Result<BrowserSession, String> {
    let mut child = Command::new("node")
        .arg(runner_path.as_os_str())
        .arg(if headless { "--headless" } else { "--headed" })
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("Failed to start Playwright runner (is Node installed?): {}", e))?;

    let stdin = child.stdin.take().ok_or("Failed to take stdin")?;
    let stdout = child
        .stdout
        .take()
        .ok_or("Failed to take stdout")?;

    let mut session = BrowserSession {
        _child: child,
        stdin: Some(stdin),
        stdout: BufReader::new(stdout),
    };

    let init_result = session
        .send_command(&serde_json::json!({
            "cmd": "init",
            "params": { "headless": headless, "action_timeout_secs": action_timeout_secs }
        }))
        .await?;

    let ok = init_result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    if !ok {
        let err = init_result
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("Init failed");
        return Err(err.to_string());
    }

    Ok(session)
}

/// Close and remove the session for the given task.
pub async fn close_task(registry: &BrowserSessionRegistry, task_id: Uuid) {
    let mut g = registry.write().await;
    if let Some(mut session) = g.remove(&task_id) {
        let _ = session.close().await;
    }
}
