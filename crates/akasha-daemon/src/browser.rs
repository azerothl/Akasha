//! Browser automation session (spec 39). One Playwright-backed session per task.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Single flight for Playwright browser download (multiple tasks may hit init failure together).
static PLAYWRIGHT_INSTALL_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// Registry of browser sessions by task_id. When a task completes, call close_task to free the session.
pub type BrowserSessionRegistry = Arc<RwLock<HashMap<Uuid, BrowserSession>>>;

pub struct BrowserSession {
    _child: Child,
    stdin: Option<tokio::process::ChildStdin>,
    stdout: BufReader<tokio::process::ChildStdout>,
    /// Last lines of the runner's stderr (filled by a background task) for diagnostics when stdout EOFs.
    stderr_tail: Arc<tokio::sync::Mutex<String>>,
    /// Wall-clock instant when this session was created, used to enforce `browser_session_timeout_secs`.
    pub started_at: std::time::Instant,
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
            // Child exited or closed stdout; stderr often has the real reason (import error, Playwright, etc.).
            let _ = self._child.wait().await;
            for _ in 0..4 {
                tokio::task::yield_now().await;
            }
            let tail = self.stderr_tail.lock().await.clone();
            let detail = tail.trim();
            if detail.is_empty() {
                return Err(
                    "Runner closed stdout before sending a response (stderr empty). Check that Node.js is installed, scripts/playwright-runner has run npm install, and npx playwright install chromium succeeded."
                        .to_string(),
                );
            }
            return Err(format!("Runner closed stdout (Node stderr):\n{}", detail));
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

/// Parent directory of `run.mjs` (e.g. `scripts/playwright-runner`).
pub fn playwright_runner_dir(runner_path: &Path) -> Option<PathBuf> {
    runner_path.parent().map(Path::to_path_buf)
}

/// Actionable message when `browser_allowed_domains` denies a host.
pub fn format_domain_denied(host: &str, allowed: &[String], blocked: &[String]) -> String {
    let allow_hint = if allowed.iter().any(|d| d.trim() == "*") {
        "browser_allowed_domains contains '*' but host may be in browser_blocked_domains".to_string()
    } else if allowed.is_empty() {
        "browser_allowed_domains is empty (deny by default)".to_string()
    } else {
        format!("browser_allowed_domains: {}", allowed.join(", "))
    };
    let block_hint = if blocked.is_empty() {
        String::new()
    } else {
        format!("; browser_blocked_domains: {}", blocked.join(", "))
    };
    format!(
        "Domain not allowed: {host}. Add '{host}' (or a parent domain) to browser_allowed_domains in tools_policy.yaml ({allow_hint}{block_hint}). Use TOOL: web_fetch when the page is static and allowed_web_domains covers the host."
    )
}

/// Enrich Playwright runner errors with timeout and install guidance.
pub fn format_runner_error(
    raw: &str,
    action_timeout_secs: u64,
    session_timeout_secs: u64,
) -> String {
    let lower = raw.to_lowercase();
    if lower.contains("timeout") {
        return format!(
            "{raw} (action timeout browser_action_timeout_secs={action_timeout_secs}s in tools_policy.yaml; session cap browser_session_timeout_secs={session_timeout_secs}s — use browser wait <ms> or increase timeouts, then retry navigate/snapshot)"
        );
    }
    if init_failure_suggests_missing_browser(raw) {
        return format!(
            "{raw} — Run TOOL: install_playwright after user consent, or ensure AKASHA_PLAYWRIGHT_AUTO_INSTALL is not 0 and Node.js/npm are on PATH."
        );
    }
    raw.to_string()
}

fn init_failure_suggests_missing_browser(msg: &str) -> bool {
    let m = msg.to_lowercase();
    m.contains("executable doesn't exist")
        || m.contains("executable does not exist")
        || m.contains("browsertype.launch")
        || m.contains("playwright install")
        || m.contains("could not find browser")
        || m.contains("browser is not installed")
        || m.contains("cannot find module")
        || m.contains("cannot find package")
        || m.contains("err_module_not_found")
        || m.contains("node:internal/modules")
}

/// On Windows, `npm` / `npx` are `.cmd` shims; `Command::new("npm")` often fails to spawn (NotFound).
fn npm_program() -> &'static str {
    if cfg!(windows) {
        "npm.cmd"
    } else {
        "npm"
    }
}

fn npx_program() -> &'static str {
    if cfg!(windows) {
        "npx.cmd"
    } else {
        "npx"
    }
}

/// Run `npm install` then `npx playwright install chromium` in the runner directory.
pub async fn ensure_playwright_chromium(runner_dir: &Path) -> Result<(), String> {
    if !runner_dir.is_dir() {
        return Err(format!(
            "Playwright runner directory not found: {}",
            runner_dir.display()
        ));
    }

    tracing::info!(
        path = %runner_dir.display(),
        "Installing Playwright runner dependencies (npm install)…"
    );
    let npm_out = Command::new(npm_program())
        .current_dir(runner_dir)
        .args(["install", "--no-audit", "--no-fund"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|e| {
            format!(
                "npm install failed (is Node.js/npm installed?): {}",
                e
            )
        })?;

    if !npm_out.status.success() {
        let stderr = String::from_utf8_lossy(&npm_out.stderr);
        let stdout = String::from_utf8_lossy(&npm_out.stdout);
        return Err(format!(
            "npm install failed (exit {:?}): {}\n{}",
            npm_out.status.code(),
            stderr.trim(),
            stdout.trim()
        ));
    }

    let playwright_pkg = runner_dir.join("node_modules").join("playwright").join("package.json");
    if !playwright_pkg.is_file() {
        return Err(format!(
            "npm install reported success but `playwright` is missing at {}. Run manually in a shell: cd \"{}\" && npm install",
            playwright_pkg.display(),
            runner_dir.display()
        ));
    }

    tracing::info!("Downloading Playwright Chromium (this may take several minutes)…");
    let px_out = Command::new(npx_program())
        .current_dir(runner_dir)
        .args(["playwright", "install", "chromium"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("npx playwright install failed: {}", e))?;

    if !px_out.status.success() {
        let stderr = String::from_utf8_lossy(&px_out.stderr);
        let stdout = String::from_utf8_lossy(&px_out.stdout);
        return Err(format!(
            "playwright install chromium failed (exit {:?}): {}\n{}",
            px_out.status.code(),
            stderr.trim(),
            stdout.trim()
        ));
    }

    tracing::info!("Playwright Chromium install finished.");
    Ok(())
}

/// Resolve path to `playwright-runner/run.mjs`.
///
/// Resolution order (first existing file wins):
/// 1. `AKASHA_PLAYWRIGHT_RUNNER` — path to `run.mjs` or to a directory containing it
/// 2. `AKASHA_DATA_DIR/playwright-runner/run.mjs` — shipped / first-run copy (writable)
/// 3. `~/akasha/playwright-runner/run.mjs` — default user data layout (same as daemon store parent)
/// 4. `%LOCALAPPDATA%/Akasha/playwright-runner/run.mjs` — optional Windows layout
/// 5. Next to the executable: `playwright-runner/run.mjs`, `resources/playwright-runner/run.mjs` (portable / installer)
/// 6. Dev tree: `cwd/scripts/...`, `exe/../../scripts/...` (repo / `target/debug` builds)
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
        // Neither the exact path nor <dir>/run.mjs exists; return None to avoid
        // spawning a runner with a non-existent script.
        return None;
    }

    if let Ok(d) = std::env::var("AKASHA_DATA_DIR") {
        let run = PathBuf::from(d)
            .join("playwright-runner")
            .join("run.mjs");
        if run.is_file() {
            return Some(run);
        }
    }

    if let Some(h) = dirs::home_dir() {
        let run = h.join("akasha").join("playwright-runner").join("run.mjs");
        if run.is_file() {
            return Some(run);
        }
    }

    if let Some(ld) = dirs::data_local_dir() {
        let run = ld.join("Akasha").join("playwright-runner").join("run.mjs");
        if run.is_file() {
            return Some(run);
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            for rel in [
                ["playwright-runner", "run.mjs"].as_slice(),
                ["resources", "playwright-runner", "run.mjs"].as_slice(),
            ] {
                let mut p = parent.to_path_buf();
                for c in rel {
                    p.push(c);
                }
                if p.is_file() {
                    return Some(p);
                }
            }

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

    let cwd = std::env::current_dir().ok()?;
    let from_cwd = cwd.join("scripts").join("playwright-runner").join("run.mjs");
    if from_cwd.is_file() {
        return Some(from_cwd);
    }
    let from_parent = cwd.join("..").join("scripts").join("playwright-runner").join("run.mjs");
    if from_parent.is_file() {
        return Some(from_parent.canonicalize().unwrap_or(from_parent));
    }

    None
}

async fn create_browser_session_once(
    runner_path: &Path,
    headless: bool,
    action_timeout_secs: u64,
) -> Result<BrowserSession, String> {
    let runner_dir = playwright_runner_dir(runner_path).ok_or_else(|| {
        format!(
            "Playwright runner path has no parent directory: {}",
            runner_path.display()
        )
    })?;
    let script_name = runner_path.file_name().ok_or_else(|| {
        format!(
            "Playwright runner path has no file name: {}",
            runner_path.display()
        )
    })?;
    let mut child = Command::new("node")
        .current_dir(&runner_dir)
        .arg(script_name)
        .arg(if headless { "--headless" } else { "--headed" })
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to start Playwright runner (is Node installed?): {}", e))?;
    let stdin = child.stdin.take().ok_or("Failed to take stdin")?;
    let stdout = child.stdout.take().ok_or("Failed to take stdout")?;
    let stderr = child.stderr.take().ok_or("Failed to take stderr")?;

    let stderr_tail = Arc::new(tokio::sync::Mutex::new(String::new()));
    let tail_for_task = stderr_tail.clone();
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    let mut g = tail_for_task.lock().await;
                    g.push_str(&line);
                    const MAX: usize = 16384;
                    if g.len() > MAX {
                        let trim = g.len() - MAX;
                        g.drain(..trim);
                    }
                }
                Err(_) => break,
            }
        }
    });

    let mut session = BrowserSession {
        _child: child,
        stdin: Some(stdin),
        stdout: BufReader::new(stdout),
        stderr_tail,
        started_at: std::time::Instant::now(),
    };

    let init_result = match session
        .send_command(&serde_json::json!({
            "cmd": "init",
            "params": { "headless": headless, "action_timeout_secs": action_timeout_secs }
        }))
        .await
    {
        Ok(v) => v,
        Err(e) => return Err(e),
    };

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

/// Start a new browser session (spawns Node runner, sends init).
/// If Chromium is missing, runs `npm install` + `playwright install chromium` once (unless `AKASHA_PLAYWRIGHT_AUTO_INSTALL=0`) and retries.
pub async fn create_browser_session(
    runner_path: &Path,
    headless: bool,
    action_timeout_secs: u64,
) -> Result<BrowserSession, String> {
    let runner_dir = playwright_runner_dir(runner_path).ok_or_else(|| {
        format!(
            "Playwright runner path has no parent directory: {}",
            runner_path.display()
        )
    })?;
    let auto_install_off = std::env::var("AKASHA_PLAYWRIGHT_AUTO_INSTALL")
        .ok()
        .as_deref()
        == Some("0");
    let playwright_pkg = runner_dir
        .join("node_modules")
        .join("playwright")
        .join("package.json");
    if !auto_install_off && !playwright_pkg.is_file() {
        let lock = PLAYWRIGHT_INSTALL_LOCK.get_or_init(|| Mutex::new(()));
        let _guard = lock.lock().await;
        if !playwright_pkg.is_file() {
            tracing::info!(
                path = %runner_dir.display(),
                "Playwright npm package missing; installing dependencies and Chromium…"
            );
            ensure_playwright_chromium(&runner_dir).await?;
        }
    }

    match create_browser_session_once(runner_path, headless, action_timeout_secs).await {
        Ok(s) => Ok(s),
        Err(e) => {
            if auto_install_off || !init_failure_suggests_missing_browser(&e) {
                return Err(e);
            }
            let lock = PLAYWRIGHT_INSTALL_LOCK.get_or_init(|| Mutex::new(()));
            let _guard = lock.lock().await;
            tracing::warn!(
                error = %e,
                "Playwright browser init failed; attempting automatic install in {}",
                runner_dir.display()
            );
            ensure_playwright_chromium(&runner_dir).await?;
            create_browser_session_once(runner_path, headless, action_timeout_secs).await
                .map_err(|e2| {
                    format!(
                        "After auto-install, browser init still failed: {}",
                        e2
                    )
                })
        }
    }
}

/// Close and remove the session for the given task.
pub async fn close_task(registry: &BrowserSessionRegistry, task_id: Uuid) {
    let mut g = registry.write().await;
    if let Some(mut session) = g.remove(&task_id) {
        let _ = session.close().await;
    }
}
