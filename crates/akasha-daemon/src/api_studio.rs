//! HTTP handlers for `/api/studio/*` (Code Studio).
use crate::studio::{is_strictly_under_studio_root, resolve_studio_project_dir, studio_projects_base};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use tokio::io::{AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{Mutex, Semaphore};
use tokio::time::{timeout, Duration};

const MAX_FILE_LIST: usize = 500;
const MAX_RAW_BYTES: usize = 2 * 1024 * 1024;
const MAX_DEPTH: usize = 8;
const MAX_BUILD_OUTPUT_BYTES: usize = 512 * 1024;
const DEFAULT_BUILD_TIMEOUT_SEC: u64 = 600;
const NPM_INSTALL_TIMEOUT_SEC: u64 = 900;
/// Default range for Vite/webpack dev servers started by `POST .../preview/start`.
const PREVIEW_PORT_MIN: u16 = 5180;
const PREVIEW_PORT_MAX: u16 = 5279;
/// Ring buffer for dev-server stdout/stderr (preview process).
const MAX_PREVIEW_LOG_BYTES: usize = 256 * 1024;

struct StudioPreviewProcess {
    child: tokio::process::Child,
    log: Arc<Mutex<String>>,
}

/// Append to `log`, keeping only the last `MAX_PREVIEW_LOG_BYTES` UTF-8 bytes (best-effort).
async fn studio_append_preview_log(log: &Mutex<String>, chunk: &str) {
    let mut g = log.lock().await;
    g.push_str(chunk);
    if g.len() > MAX_PREVIEW_LOG_BYTES {
        let cut = g.len() - MAX_PREVIEW_LOG_BYTES;
        // Find the first valid UTF-8 char boundary at or after `cut` to avoid splitting a codepoint.
        let drain_to = g
            .char_indices()
            .map(|(idx, _)| idx)
            .find(|&idx| idx >= cut)
            .unwrap_or(g.len());
        if drain_to > 0 {
            g.drain(..drain_to);
        }
    }
}

async fn studio_pump_preview_stream<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    log: Arc<Mutex<String>>,
    stream_label: &'static str,
) {
    let mut buf = vec![0u8; 4096];
    loop {
        let n = match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        let chunk = String::from_utf8_lossy(&buf[..n]);
        let line = format!("[{stream_label}] {chunk}");
        studio_append_preview_log(log.as_ref(), &line).await;
    }
}

fn studio_preview_registry() -> &'static Mutex<HashMap<String, StudioPreviewProcess>> {
    static REG: OnceLock<Mutex<HashMap<String, StudioPreviewProcess>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Pick a TCP port on 127.0.0.1 (best-effort; released before the dev server binds).
fn pick_preview_port(preferred: Option<u16>) -> Option<u16> {
    if let Some(p) = preferred {
        if p >= 1024 && std::net::TcpListener::bind(("127.0.0.1", p)).is_ok() {
            return Some(p);
        }
    }
    for p in PREVIEW_PORT_MIN..=PREVIEW_PORT_MAX {
        if std::net::TcpListener::bind(("127.0.0.1", p)).is_ok() {
            return Some(p);
        }
    }
    None
}

/// On Windows, `npm` / `npx` / `pnpm` / `yarn` / `corepack` are often `*.cmd` shims —
/// `Command::new("npm")` often returns `ErrorKind::NotFound` ("program not found").
/// Delegate to `cmd.exe /c` so PATH matches a shell.
fn studio_command_from_argv(argv: &[String]) -> Command {
    #[cfg(windows)]
    {
        if argv.first().is_some_and(|s| {
            matches!(
                s.as_str(),
                "npm" | "npx" | "pnpm" | "yarn" | "corepack"
            )
        }) {
            let mut c = Command::new("cmd.exe");
            c.arg("/c");
            for a in argv {
                c.arg(a);
            }
            return c;
        }
    }
    let mut c = Command::new(&argv[0]);
    if argv.len() > 1 {
        c.args(&argv[1..]);
    }
    c
}

/// Run a bounded command under `root`, capturing stdout/stderr (same safety rules as build).
async fn studio_run_command_capture(
    root: &Path,
    argv: &[String],
    timeout_sec: u64,
) -> Result<(Option<i32>, String, String), String> {
    if argv.is_empty() {
        return Err("empty argv".into());
    }
    if !argv_looks_safe(argv) {
        return Err("invalid or unsafe argv".into());
    }
    let mut cmd = studio_command_from_argv(argv);
    cmd.current_dir(root).kill_on_drop(true);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.as_std_mut().creation_flags(CREATE_NO_WINDOW);
    }
    let run = async move {
        let mut child = cmd
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let read_stdout = async move {
            let mut out = String::new();
            if let Some(s) = stdout {
                let mut r = BufReader::new(s);
                let mut buf = vec![0u8; 8192];
                loop {
                    let n = r.read(&mut buf).await?;
                    if n == 0 {
                        break;
                    }
                    if out.len() < MAX_BUILD_OUTPUT_BYTES {
                        let take = n.min(MAX_BUILD_OUTPUT_BYTES.saturating_sub(out.len()));
                        out.push_str(&String::from_utf8_lossy(&buf[..take]));
                    }
                }
            }
            Ok::<_, std::io::Error>(out)
        };

        let read_stderr = async move {
            let mut err = String::new();
            if let Some(s) = stderr {
                let mut r = BufReader::new(s);
                let mut buf = vec![0u8; 8192];
                loop {
                    let n = r.read(&mut buf).await?;
                    if n == 0 {
                        break;
                    }
                    if err.len() < MAX_BUILD_OUTPUT_BYTES {
                        let take = n.min(MAX_BUILD_OUTPUT_BYTES.saturating_sub(err.len()));
                        err.push_str(&String::from_utf8_lossy(&buf[..take]));
                    }
                }
            }
            Ok::<_, std::io::Error>(err)
        };

        let (out, err) = tokio::try_join!(read_stdout, read_stderr)?;
        let status = child.wait().await?;
        Ok::<_, std::io::Error>((status, out, err))
    };
    let result = timeout(Duration::from_secs(timeout_sec), run).await;
    match result {
        Ok(Ok((status, stdout, stderr))) => Ok((status.code(), truncate_output(&stdout), truncate_output(&stderr))),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err(format!("timeout after {}s", timeout_sec)),
    }
}

fn studio_ops_semaphore() -> &'static Semaphore {
    static SEM: OnceLock<Semaphore> = OnceLock::new();
    SEM.get_or_init(|| {
        let n = std::env::var("AKASHA_STUDIO_MAX_PARALLEL_OPS")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|&n| n > 0 && n < 64)
            .unwrap_or(4);
        Semaphore::new(n)
    })
}

fn json_response(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        body.len(),
        body
    )
}

#[derive(Serialize)]
struct ProjectMetaOut {
    id: String,
    /// Display name from `.akasha-studio.json` (falls back to a short id label if missing).
    name: String,
    path: String,
}

#[derive(Serialize)]
struct ProjectsListOut {
    projects: Vec<ProjectMetaOut>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StudioEvolution {
    id: String,
    branch: String,
    status: String,
    created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    root_task_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StudioMeta {
    id: String,
    name: String,
    created_at: String,
    #[serde(default)]
    evolutions: Vec<StudioEvolution>,
    /// Free-form stack description (languages, frameworks, package manager). Injected into Code Studio messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tech_stack: Option<String>,
    /// When true, skip automatic post-task verify (`npm run build` / `cargo check`).
    #[serde(default)]
    verify_skip: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    verify_argv: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    verify_timeout_sec: Option<u64>,
}

const MAX_TECH_STACK_CHARS: usize = 4000;
/// Upper bound on characters injected from `CODE_STUDIO_PLAN.md` into each Code Studio message.
const MAX_CODE_STUDIO_PLAN_INJECT_CHARS: usize = 4000;

/// Keep only prompt-safe characters:
/// - drop NUL and non-printable control chars (except LF/CR/TAB)
/// - trim surrounding whitespace
/// - bound final size in characters
fn sanitize_for_prompt(raw: &str, max_chars: usize) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|&ch| ch == '\n' || ch == '\r' || ch == '\t' || !ch.is_control())
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    trimmed.chars().take(max_chars).collect::<String>()
}

/// Embeds `CODE_STUDIO_PLAN.md` so multi-turn and evolution tasks keep product scope (game type, goals, history).
/// Truncates with an explicit hint to use `read_file` for the full file.
pub fn studio_code_plan_message_prefix(project_root: &Path) -> Option<String> {
    let plan_path = project_root.join("CODE_STUDIO_PLAN.md");
    let raw = fs::read_to_string(&plan_path).ok()?;
    let cleaned = sanitize_for_prompt(&raw, MAX_CODE_STUDIO_PLAN_INJECT_CHARS);
    if cleaned.is_empty() {
        return None;
    }
    let was_truncated = raw.trim().chars().count() > cleaned.chars().count();
    let body: String = if was_truncated {
        format!(
            "{cleaned}…\n[… CODE_STUDIO_PLAN.md réduit (sécurité/limite contexte) — utiliser read_file workspace:/CODE_STUDIO_PLAN.md pour le contenu complet.]\n"
        )
    } else {
        cleaned
    };
    Some(format!(
        "[Contexte projet — CODE_STUDIO_PLAN.md (référence produit, objectif, historique ; à respecter tant que l’utilisateur ne demande pas explicitement autre chose) :\n{body}\n]\n\n"
    ))
}

/// Prefix prepended to the user message when `tech_stack` is set (read by LLM + studio agents).
pub fn studio_tech_stack_message_prefix(project_root: &Path) -> Option<String> {
    let meta = load_studio_meta(project_root)?;
    let t = sanitize_for_prompt(meta.tech_stack.as_deref()?, MAX_TECH_STACK_CHARS);
    if t.is_empty() {
        return None;
    }
    Some(format!(
        "[Stack projet — respecter pour fichiers, dépendances et build (sauf demande utilisateur contraire) :\n{t}\n]\n\n"
    ))
}

fn load_studio_meta(project_root: &Path) -> Option<StudioMeta> {
    let p = project_root.join(".akasha-studio.json");
    let s = fs::read_to_string(p).ok()?;
    serde_json::from_str(&s).ok()
}

fn save_studio_meta(project_root: &Path, meta: &StudioMeta) -> Result<(), String> {
    let p = project_root.join(".akasha-studio.json");
    let j = serde_json::to_string_pretty(meta).map_err(|e| e.to_string())?;
    fs::write(&p, j).map_err(|e| e.to_string())
}

fn list_project_dirs(base: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(base) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.push(p);
            }
        }
    }
    out.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    out
}

fn collect_files_recursive(root: &Path, rel: &Path, depth: usize, out: &mut Vec<String>) {
    if out.len() >= MAX_FILE_LIST || depth > MAX_DEPTH {
        return;
    }
    let Ok(rd) = fs::read_dir(root.join(rel)) else {
        return;
    };
    for e in rd.flatten() {
        if out.len() >= MAX_FILE_LIST {
            break;
        }
        let name = e.file_name().to_string_lossy().to_string();
        if name == ".git" || name == "node_modules" {
            continue;
        }
        let mut sub = rel.to_path_buf();
        sub.push(&name);
        let sub_s = sub.to_string_lossy().replace('\\', "/");
        if e.path().is_dir() {
            collect_files_recursive(root, &sub, depth + 1, out);
        } else if name != ".akasha-studio.json" {
            out.push(sub_s);
        }
    }
}

fn query_param<'a>(query_str: &'a str, key: &str) -> Option<std::borrow::Cow<'a, str>> {
    for pair in query_str.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == key {
                return Some(urlencoding::decode(v).unwrap_or(std::borrow::Cow::from(v)));
            }
        }
    }
    None
}

/// Project id segment after `/api/studio/projects/`.
fn strip_studio_projects_prefix(path_only: &str) -> Option<&str> {
    path_only.strip_prefix("/api/studio/projects/")
}

fn argv_looks_safe(argv: &[String]) -> bool {
    if argv.is_empty() {
        return false;
    }
    for a in argv {
        if a.contains("..")
            || a.contains(';')
            || a.contains('|')
            || a.contains('&')
            || a.contains('`')
            || a.contains('\n')
            || a.contains('\r')
        {
            return false;
        }
    }
    true
}

fn truncate_output(s: &str) -> String {
    if s.len() <= MAX_BUILD_OUTPUT_BYTES {
        s.to_string()
    } else {
        // Find a valid UTF-8 char boundary at or before MAX_BUILD_OUTPUT_BYTES.
        let cutoff = s
            .char_indices()
            .map(|(idx, _)| idx)
            .take_while(|&idx| idx <= MAX_BUILD_OUTPUT_BYTES)
            .last()
            .unwrap_or(0);
        format!(
            "{}\n… [truncated, {} bytes total]",
            &s[..cutoff],
            s.len()
        )
    }
}

/// Reject obvious LLM markdown / tool-protocol leakage in code files under Code Studio.
pub fn studio_reject_polluted_code_content(disk_path: &Path, content: &str) -> Option<String> {
    let ext = disk_path.extension()?.to_string_lossy().to_lowercase();
    let code_ext = matches!(
        ext.as_str(),
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "rs" | "py" | "go" | "java" | "kt" | "swift" | "vue" | "svelte"
    );
    if !code_ext {
        return None;
    }
    let t = content.trim_start();
    if t.starts_with("```") {
        return Some(
            "rejected: fichier code commence par une barre markdown ``` — écrire uniquement le source, sans blocs markdown."
                .to_string(),
        );
    }
    if content.lines().any(|l| {
        let s = l.trim();
        s.starts_with("TOOL: write_file")
            || (s.starts_with("TOOL: ") && (s.contains("write_file") || s.contains("apply_patch")))
    }) {
        return Some(
            "rejected: le fichier contient des lignes de protocole d’outil — le contenu doit être uniquement du code source."
                .to_string(),
        );
    }
    None
}

/// After an agent task on a studio disk, run build/check when possible. Err = verify failed (task should fail).
pub async fn studio_verify_after_agent_task(project_root: &Path) -> Result<(), String> {
    let meta = load_studio_meta(project_root);
    if meta.as_ref().map(|m| m.verify_skip).unwrap_or(false) {
        return Ok(());
    }
    let timeout_sec = meta
        .as_ref()
        .and_then(|m| m.verify_timeout_sec)
        .unwrap_or(DEFAULT_BUILD_TIMEOUT_SEC)
        .min(3600);
    let argv: Vec<String> = if let Some(v) = meta.as_ref().and_then(|m| m.verify_argv.as_ref()) {
        if v.is_empty() {
            return Ok(());
        }
        v.clone()
    } else if project_root.join("package.json").is_file() {
        vec!["npm".into(), "run".into(), "build".into()]
    } else if project_root.join("Cargo.toml").is_file() {
        vec!["cargo".into(), "check".into()]
    } else {
        return Ok(());
    };
    if !argv_looks_safe(&argv) {
        return Err("verify_argv in .akasha-studio.json is invalid or unsafe".into());
    }
    match studio_run_command_capture(project_root, &argv, timeout_sec).await {
        Ok((Some(0), _, _)) => Ok(()),
        Ok((code, out, err)) => Err(format!(
            "Vérification post-tâche échouée (code {:?}).\n--- stdout ---\n{}\n--- stderr ---\n{}",
            code,
            out.chars().take(4000).collect::<String>(),
            err.chars().take(4000).collect::<String>()
        )),
        Err(e) => Err(format!("Vérification post-tâche: {}", e)),
    }
}

fn write_initial_code_studio_plan(project_root: &Path, name: &str, tech_stack: Option<&str>) -> Result<(), String> {
    let plan_path = project_root.join("CODE_STUDIO_PLAN.md");
    if plan_path.exists() {
        return Ok(());
    }
    let stack_block = tech_stack
        .filter(|s| !s.trim().is_empty())
        .map(|s| format!("## Stack\n\n{s}\n\n"))
        .unwrap_or_default();
    let body = format!(
        r#"# Projet Code Studio — {name}

## Métadonnées

- Créé : automatiquement à la création du projet dans Akasha Code Studio
- Dépôt : ce dossier (Git local)

{stack_block}## Objectif

(Décrire ici le but du projet.)

## Étapes prévues

1. …

## Historique des changements

- (Les agents doivent ajouter une ligne datée à chaque lot de modifications importantes.)

---
*Ce fichier est la référence de suivi du projet. Les agents Code Studio doivent le mettre à jour après des modifications suite à une demande d’évolution.*
"#,
        name = name,
        stack_block = stack_block,
    );
    fs::write(&plan_path, body).map_err(|e| e.to_string())
}

async fn git_output(project_root: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    let mut c = Command::new("git");
    c.args(args).current_dir(project_root).kill_on_drop(true);
    c.output().await.map_err(|e| e.to_string())
}

async fn is_git_repo(project_root: &Path) -> bool {
    match git_output(project_root, &["rev-parse", "--is-inside-work-tree"]).await {
        Ok(o) => o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true",
        Err(_) => false,
    }
}

/// Current branch and whether the worktree is clean (`git status --porcelain` empty).
async fn git_branch_and_clean_status(project_root: &Path) -> (Option<String>, Option<bool>) {
    if !is_git_repo(project_root).await {
        return (None, None);
    }
    let branch = match git_output(project_root, &["rev-parse", "--abbrev-ref", "HEAD"]).await {
        Ok(o) if o.status.success() => Some(String::from_utf8_lossy(&o.stdout).trim().to_string()),
        _ => None,
    };
    let clean = match git_output(project_root, &["status", "--porcelain"]).await {
        Ok(o) if o.status.success() => Some(String::from_utf8_lossy(&o.stdout).trim().is_empty()),
        _ => None,
    };
    (branch, clean)
}

async fn git_checkout_mainish(project_root: &Path) -> Result<(), String> {
    for b in ["main", "master"] {
        let o = git_output(project_root, &["checkout", b]).await?;
        if o.status.success() {
            return Ok(());
        }
    }
    Err("could not checkout main or master".to_string())
}

/// Public: resolve evolution branch from `.akasha-studio.json` (used by `POST /api/message`).
pub fn evolution_branch_for_id(data_dir: &Path, project_id: &str, evolution_id: &str) -> Option<String> {
    let root = resolve_studio_project_dir(data_dir, project_id).ok()?;
    let meta = load_studio_meta(&root)?;
    meta.evolutions
        .iter()
        .find(|e| e.id == evolution_id && e.status == "open")
        .map(|e| e.branch.clone())
}

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
        };
        let _ = save_studio_meta(&dir, &meta);
        let _ = write_initial_code_studio_plan(&dir, &meta.name, meta.tech_stack.as_deref());
        if !dir.join(".git").exists() {
            let mut g = Command::new("git");
            g.arg("init").current_dir(&dir).kill_on_drop(true);
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                const CREATE_NO_WINDOW: u32 = 0x08000000;
                g.as_std_mut().creation_flags(CREATE_NO_WINDOW);
            }
            if let Err(e) = g.status().await {
                tracing::warn!(error = %e, path = %dir.display(), "git init failed for new studio project");
            }
        }
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
                let meta = load_studio_meta(&root).unwrap_or(StudioMeta {
                    id: id.to_string(),
                    name: "Untitled".to_string(),
                    created_at: String::new(),
                    evolutions: Vec::new(),
                    tech_stack: None,
                    verify_skip: false,
                    verify_argv: None,
                    verify_timeout_sec: None,
                });
                let mut body = serde_json::to_value(&meta).unwrap_or_else(|_| serde_json::json!({}));
                if let Some(obj) = body.as_object_mut() {
                    let (branch, clean) = git_branch_and_clean_status(&root).await;
                    obj.insert("git_branch".into(), serde_json::json!(branch));
                    obj.insert("git_worktree_clean".into(), serde_json::json!(clean));
                }
                return Some(json_response("200 OK", &body.to_string()));
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
                if !has_name && !has_stack && !has_verify_skip && !has_verify_argv && !has_verify_timeout {
                    return Some(json_response(
                        "400 Bad Request",
                        r#"{"error":"provide at least one of: name, tech_stack, verify_skip, verify_argv, verify_timeout_sec"}"#,
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
        let mut non_empty = false;
        if let Ok(rd) = fs::read_dir(&root) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_string();
                if n != ".akasha-studio.json" {
                    non_empty = true;
                    break;
                }
            }
        }
        if non_empty {
            return Some(json_response(
                "409 Conflict",
                r#"{"error":"project_dir_not_empty"}"#,
            ));
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
            Ok(s) if s.success() => return Some(json_response("200 OK", r#"{"ok":true,"message":"cloned"}"#)),
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
                let url = format!("http://127.0.0.1:{port}");
                let mut body = serde_json::json!({
                    "ok": true,
                    "url": url,
                    "port": port,
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
        let mut cmd = studio_command_from_argv(&argv);
        cmd.current_dir(&root).kill_on_drop(true);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            cmd.as_std_mut().creation_flags(CREATE_NO_WINDOW);
        }
        let run = async move {
            let mut child = cmd.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn()?;
            let stdout = child.stdout.take();
            let stderr = child.stderr.take();

            let read_stdout = async move {
                let mut out = String::new();
                if let Some(s) = stdout {
                    let mut r = BufReader::new(s);
                    let mut buf = vec![0u8; 8192];
                    loop {
                        let n = r.read(&mut buf).await?;
                        if n == 0 {
                            break;
                        }
                        if out.len() < MAX_BUILD_OUTPUT_BYTES {
                            let take = n.min(MAX_BUILD_OUTPUT_BYTES.saturating_sub(out.len()));
                            out.push_str(&String::from_utf8_lossy(&buf[..take]));
                        }
                    }
                }
                Ok::<_, std::io::Error>(out)
            };

            let read_stderr = async move {
                let mut err = String::new();
                if let Some(s) = stderr {
                    let mut r = BufReader::new(s);
                    let mut buf = vec![0u8; 8192];
                    loop {
                        let n = r.read(&mut buf).await?;
                        if n == 0 {
                            break;
                        }
                        if err.len() < MAX_BUILD_OUTPUT_BYTES {
                            let take = n.min(MAX_BUILD_OUTPUT_BYTES.saturating_sub(err.len()));
                            err.push_str(&String::from_utf8_lossy(&buf[..take]));
                        }
                    }
                }
                Ok::<_, std::io::Error>(err)
            };

            let (out, err) = tokio::try_join!(read_stdout, read_stderr)?;
            let status = child.wait().await?;
            Ok::<_, std::io::Error>((status, out, err))
        };
        let result = timeout(Duration::from_secs(timeout_sec), run).await;
        let (http_status, body) = match result {
            Ok(Ok((status, stdout, stderr))) => {
                let body = serde_json::json!({
                    "exit_code": status.code(),
                    "stdout": truncate_output(&stdout),
                    "stderr": truncate_output(&stderr),
                })
                .to_string();
                let http_status = if status.success() { "200 OK" } else { "500 Internal Server Error" };
                (http_status, body)
            }
            Ok(Err(e)) => (
                "500 Internal Server Error",
                serde_json::json!({ "error": e.to_string(), "exit_code": -1 }).to_string(),
            ),
            Err(_) => (
                "504 Gateway Timeout",
                serde_json::json!({ "error": "timeout", "timeout_sec": timeout_sec }).to_string(),
            ),
        };
        return Some(json_response(http_status, &body));
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
            if let Err(e) = git_checkout_mainish(&root).await {
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
                if git_checkout_mainish(&root).await.is_err() {
                    return Some(json_response(
                        "500 Internal Server Error",
                        r#"{"error":"checkout_main_failed"}"#,
                    ));
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
                let _ = git_checkout_mainish(&root).await;
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

#[cfg(windows)]
fn strip_verbatim(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if s.starts_with(r"\\?\") {
        PathBuf::from(s.replace(r"\\?\", ""))
    } else {
        p.to_path_buf()
    }
}

#[cfg(not(windows))]
fn strip_verbatim(p: &Path) -> PathBuf {
    p.to_path_buf()
}

fn guess_mime_and_text(rel: &str, bytes: &[u8]) -> (&'static str, bool) {
    let lower = rel.to_lowercase();
    let text_ext = lower.ends_with(".md")
        || lower.ends_with(".txt")
        || lower.ends_with(".json")
        || lower.ends_with(".yaml")
        || lower.ends_with(".yml")
        || lower.ends_with(".html")
        || lower.ends_with(".htm")
        || lower.ends_with(".css")
        || lower.ends_with(".js")
        || lower.ends_with(".ts")
        || lower.ends_with(".tsx")
        || lower.ends_with(".jsx")
        || lower.ends_with(".rs")
        || lower.ends_with(".toml")
        || lower.ends_with(".svg");
    let mime = if lower.ends_with(".html") || lower.ends_with(".htm") {
        "text/html"
    } else if lower.ends_with(".css") {
        "text/css"
    } else if lower.ends_with(".js") {
        "text/javascript"
    } else if lower.ends_with(".json") {
        "application/json"
    } else if lower.ends_with(".svg") {
        "image/svg+xml"
    } else if text_ext {
        "text/plain"
    } else {
        "application/octet-stream"
    };
    let is_text = text_ext || std::str::from_utf8(bytes).is_ok();
    (mime, is_text)
}

#[cfg(test)]
mod tests {
    use super::studio_command_from_argv;
    use std::ffi::OsStr;

    #[test]
    fn studio_command_npm_argv_uses_cmd_on_windows() {
        let argv = vec!["npm".to_string(), "run".to_string(), "build".to_string()];
        let cmd = studio_command_from_argv(&argv);
        let program: &OsStr = cmd.as_std().get_program();
        #[cfg(windows)]
        {
            assert_eq!(program, "cmd.exe");
        }
        #[cfg(not(windows))]
        {
            assert_eq!(program, "npm");
        }
    }

    #[test]
    fn studio_reject_polluted_detects_fence() {
        use super::studio_reject_polluted_code_content;
        use std::path::Path;
        let p = Path::new("src/App.tsx");
        assert!(studio_reject_polluted_code_content(p, "```tsx\nconst x = 1;\n").is_some());
        assert!(studio_reject_polluted_code_content(p, "const x = 1;\n").is_none());
    }

    #[test]
    fn studio_code_plan_message_prefix_includes_plan_text() {
        use super::studio_code_plan_message_prefix;
        let dir = std::env::temp_dir().join(format!(
            "akasha_studio_plan_test_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("CODE_STUDIO_PLAN.md"),
            "# Puissance 4\n\nObjectif : grille 7×6, IA locale.",
        )
        .unwrap();
        let p = studio_code_plan_message_prefix(&dir).unwrap();
        assert!(p.contains("Puissance 4"));
        assert!(p.contains("7×6"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sanitize_for_prompt_removes_control_chars() {
        use super::sanitize_for_prompt;
        let out = sanitize_for_prompt("abc\0def\n\tghi\u{0007}", 100);
        assert_eq!(out, "abcdef\n\tghi");
    }
}
