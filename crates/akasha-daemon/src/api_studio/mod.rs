//! HTTP handlers for `/api/studio/*` (Code Studio).
mod acceptance;
mod code_extensions;
mod guardrails;
mod response_auditor;
pub(crate) use acceptance::{
    format_acceptance_prefix_for_llm, parse_api_acceptance_field, run_mechanical_acceptance_checks,
    strip_embedded_acceptance_json, studio_survey_tool, StudioAcceptancePayload, StudioCriterionKind,
    STUDIO_ACCEPTANCE_JSON_BEGIN, STUDIO_ACCEPTANCE_JSON_END,
};
pub(crate) use code_extensions::{is_agent_code_file_extension, path_has_agent_code_extension};
pub(crate) use guardrails::{
    code_studio_skip_zero_tool_mandatory_retry, looks_like_code_studio_promise_before_any_tools,
    looks_like_code_studio_prose_only_implementation_reply,
};
pub(crate) use response_auditor::{
    studio_llm_audit_code_studio_turn, studio_llm_response_auditor_enabled, StudioLlmAuditParams,
};

use crate::api_http::json_response;
use crate::studio::{is_strictly_under_studio_root, resolve_studio_project_dir, studio_projects_base};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::json;
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
/// Plafond pour un `npm install` déclenché automatiquement après un build studio en échec.
const AUTO_VERIFY_NPM_INSTALL_CAP_SEC: u64 = 480;
/// Default range for Vite/webpack dev servers started by `POST .../preview/start`.
const PREVIEW_PORT_MIN: u16 = 5180;
const PREVIEW_PORT_MAX: u16 = 5279;
/// Ring buffer for dev-server stdout/stderr (preview process).
const MAX_PREVIEW_LOG_BYTES: usize = 256 * 1024;
const PREVIEW_PROXY_TOKEN_TTL_SEC: i64 = 180;

struct StudioPreviewProcess {
    child: tokio::process::Child,
    log: Arc<Mutex<String>>,
}

#[derive(Debug, Clone)]
struct PreviewProxyTicket {
    project_id: String,
    port: u16,
    expires_unix: i64,
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

fn preview_proxy_registry() -> &'static Mutex<HashMap<String, PreviewProxyTicket>> {
    static REG: OnceLock<Mutex<HashMap<String, PreviewProxyTicket>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

async fn issue_preview_proxy_token(project_id: &str, port: u16) -> String {
    let token = format!("ppx_{}", uuid::Uuid::new_v4().simple());
    let now = chrono::Utc::now().timestamp();
    let expires_unix = now + PREVIEW_PROXY_TOKEN_TTL_SEC;
    let mut reg = preview_proxy_registry().lock().await;
    reg.retain(|_, t| t.expires_unix > now);
    reg.insert(
        token.clone(),
        PreviewProxyTicket {
            project_id: project_id.to_string(),
            port,
            expires_unix,
        },
    );
    token
}

async fn validate_preview_proxy_token(project_id: &str, token: &str) -> Result<u16, String> {
    let now = chrono::Utc::now().timestamp();
    let mut reg = preview_proxy_registry().lock().await;
    reg.retain(|_, t| t.expires_unix > now);
    let Some(ticket) = reg.get(token) else {
        return Err("invalid_token".to_string());
    };
    if ticket.project_id != project_id {
        return Err("project_scope_mismatch".to_string());
    }
    if ticket.expires_unix <= now {
        return Err("token_expired".to_string());
    }
    Ok(ticket.port)
}

fn command_in_container_image(argv: &[String]) -> &'static str {
    if argv.first().is_some_and(|c| c == "cargo" || c == "cargo.exe") {
        "rust:1"
    } else {
        "node:20"
    }
}

async fn run_build_in_container(
    project_root: &Path,
    argv: &[String],
    timeout_sec: u64,
) -> Result<(Option<i32>, String, String), String> {
    if argv.is_empty() {
        return Err("argv_required".to_string());
    }
    let root_s = project_root
        .to_str()
        .ok_or_else(|| "invalid_project_path".to_string())?;
    let image = command_in_container_image(argv);
    let mut docker_argv = vec![
        "run".to_string(),
        "--rm".to_string(),
        "-v".to_string(),
        format!("{root_s}:/workspace"),
        "-w".to_string(),
        "/workspace".to_string(),
        image.to_string(),
    ];
    docker_argv.extend_from_slice(argv);
    let mut cmd = Command::new("docker");
    cmd.args(&docker_argv);
    cmd.kill_on_drop(true);
    let run = async move {
        let mut child = cmd
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
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
        let (out, err) = tokio::try_join!(read_stdout, read_stderr).map_err(|e| e.to_string())?;
        let status = child.wait().await.map_err(|e| e.to_string())?;
        Ok::<_, String>((status.code(), truncate_output(&out), truncate_output(&err)))
    };
    match timeout(Duration::from_secs(timeout_sec), run).await {
        Ok(v) => v,
        Err(_) => Err(format!("timeout after {}s", timeout_sec)),
    }
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
    /// Résumé court de l’évolution / session (réinjecté dans chaque message Code Studio).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    evolution_summary: Option<String>,
    /// Notes de politique outils / périmètre (réinjectées ; complètent tools_policy côté humain).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    policy_notes: Option<String>,
}

const MAX_TECH_STACK_CHARS: usize = 4000;
/// Upper bound on characters injected from `CODE_STUDIO_PLAN.md` into each Code Studio message.
const MAX_CODE_STUDIO_PLAN_INJECT_CHARS: usize = 4000;
const MAX_EVOLUTION_SUMMARY_CHARS: usize = 6000;
const MAX_POLICY_NOTES_CHARS: usize = 4000;
const MAX_DESIGN_HINT_CHARS: usize = 4000;
const MAX_DESIGN_DOC_CHARS: usize = 12000;

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
        "[Contexte projet — CODE_STUDIO_PLAN.md (gabarit à sections fixes ; à respecter tant que l’utilisateur ne demande pas explicitement autre chose ; les agents doivent le mettre à jour **par section**, pas en réécriture totale systématique) :\n{body}\n]\n\n"
    ))
}

/// Build the tech-stack prefix from an already-loaded `StudioMeta`.
fn tech_stack_prefix_from_meta(meta: &StudioMeta) -> Option<String> {
    let t = sanitize_for_prompt(meta.tech_stack.as_deref()?, MAX_TECH_STACK_CHARS);
    if t.is_empty() {
        return None;
    }
    Some(format!(
        "[Stack projet — respecter pour fichiers, dépendances et build (sauf demande utilisateur contraire) :\n{t}\n]\n\n"
    ))
}

/// Build the evolution-summary prefix from an already-loaded `StudioMeta`.
fn evolution_summary_prefix_from_meta(meta: &StudioMeta) -> Option<String> {
    let t = sanitize_for_prompt(meta.evolution_summary.as_deref()?, MAX_EVOLUTION_SUMMARY_CHARS);
    if t.is_empty() {
        return None;
    }
    Some(format!(
        "[Résumé évolution / session (à respecter ; mettre à jour si besoin via l’UI ou une tâche dédiée) :\n{t}\n]\n\n"
    ))
}

/// Build the policy-notes prefix from an already-loaded `StudioMeta`.
fn policy_notes_prefix_from_meta(meta: &StudioMeta) -> Option<String> {
    let t = sanitize_for_prompt(meta.policy_notes.as_deref()?, MAX_POLICY_NOTES_CHARS);
    if t.is_empty() {
        return None;
    }
    Some(format!(
        "[Politique / consignes projet (outils et périmètre) :\n{t}\n]\n\n"
    ))
}

/// Load `.akasha-studio.json` once and return `(evolution_summary, policy_notes, tech_stack)` prefixes.
/// Avoids redundant disk I/O when the caller needs all three in the same request.
pub fn studio_meta_prefixes(project_root: &Path) -> (Option<String>, Option<String>, Option<String>) {
    let Some(meta) = load_studio_meta(project_root) else {
        return (None, None, None);
    };
    (
        evolution_summary_prefix_from_meta(&meta),
        policy_notes_prefix_from_meta(&meta),
        tech_stack_prefix_from_meta(&meta),
    )
}

/// Prefix prepended to the user message when `tech_stack` is set (read by LLM + studio agents).
pub fn studio_tech_stack_message_prefix(project_root: &Path) -> Option<String> {
    let meta = load_studio_meta(project_root)?;
    tech_stack_prefix_from_meta(&meta)
}

/// Résumé d’évolution / mémoire courte (fichier `.akasha-studio.json`).
pub fn studio_evolution_summary_prefix(project_root: &Path) -> Option<String> {
    let meta = load_studio_meta(project_root)?;
    evolution_summary_prefix_from_meta(&meta)
}

/// Notes de politique projet (périmètre outils, dossiers sensibles).
pub fn studio_policy_notes_prefix(project_root: &Path) -> Option<String> {
    let meta = load_studio_meta(project_root)?;
    policy_notes_prefix_from_meta(&meta)
}

/// Préfixe utilisateur / UI : `plan`, `implement`, `build`, `free` (aucun préfixe).
pub fn studio_code_mode_message_prefix(mode: &str) -> Option<String> {
    match mode.trim().to_lowercase().as_str() {
        "plan" => Some(
            "[Mode Code Studio — PLANIFICATION : priorité analyse et mise à jour de CODE_STUDIO_PLAN.md ; pas d’implémentation ni commandes mutatrices sauf demande explicite.]\n\n"
                .to_string(),
        ),
        "implement" => Some(
            "[Mode Code Studio — IMPLÉMENTATION : produire ou modifier le code dans le périmètre du plan et de la stack.]\n\n"
                .to_string(),
        ),
        "build" => Some(
            "[Mode Code Studio — BUILD / QUALITÉ : privilégier build, tests et corrections.]\n\n"
                .to_string(),
        ),
        "free" | "" => None,
        _ => None,
    }
}

/// Consigne ponctuelle (un message) depuis l’UI.
pub fn studio_one_shot_policy_hint_prefix(hint: &str) -> Option<String> {
    let t = sanitize_for_prompt(hint, 2000);
    if t.is_empty() {
        return None;
    }
    Some(format!(
        "[Consigne additionnelle pour cette requête uniquement :\n{t}\n]\n\n"
    ))
}

/// Design hint ponctuel (résumé tokens/règles) envoyé par l'UI.
pub fn studio_design_hint_prefix(hint: &str) -> Option<String> {
    let t = sanitize_for_prompt(hint, MAX_DESIGN_HINT_CHARS);
    if t.is_empty() {
        return None;
    }
    Some(format!("[Contexte design (résumé) :\n{t}\n]\n\n"))
}

/// Contrat DESIGN.md complet envoyé par l'UI (borné pour éviter un prompt trop volumineux).
pub fn studio_design_doc_prefix(doc: &str) -> Option<String> {
    let t = sanitize_for_prompt(doc, MAX_DESIGN_DOC_CHARS);
    if t.is_empty() {
        return None;
    }
    Some(format!(
        "[Contrat design — DESIGN.md (respecter tokens + prose, sauf demande explicite utilisateur) :\n{t}\n]\n\n"
    ))
}

fn lint_design_doc(raw: &str) -> serde_json::Value {
    let text = raw.trim();
    let mut findings: Vec<serde_json::Value> = Vec::new();
    if text.is_empty() {
        findings.push(json!({"severity":"warning","path":"root","message":"DESIGN.md vide"}));
    }
    let has_front_matter = text.starts_with("---") && text[3..].contains("\n---");
    if !has_front_matter {
        findings.push(json!({"severity":"error","path":"frontmatter","message":"front matter YAML manquant"}));
    }
    if !text.contains("name:") {
        findings.push(json!({"severity":"warning","path":"name","message":"token `name` absent"}));
    }
    if !text.contains("colors:") {
        findings.push(json!({"severity":"warning","path":"colors","message":"section tokens `colors` absente"}));
    }
    if !text.contains("typography:") {
        findings.push(json!({"severity":"warning","path":"typography","message":"section tokens `typography` absente"}));
    }
    if !text.contains("## ") {
        findings.push(json!({"severity":"info","path":"body","message":"aucune section markdown `##` détectée"}));
    }
    let errors = findings
        .iter()
        .filter(|f| f.get("severity").and_then(|s| s.as_str()) == Some("error"))
        .count();
    let warnings = findings
        .iter()
        .filter(|f| f.get("severity").and_then(|s| s.as_str()) == Some("warning"))
        .count();
    let info = findings
        .iter()
        .filter(|f| f.get("severity").and_then(|s| s.as_str()) == Some("info"))
        .count();
    json!({
        "findings": findings,
        "summary": { "errors": errors, "warnings": warnings, "info": info }
    })
}

fn extract_design_summary(v: &serde_json::Value) -> (usize, usize) {
    let errors = v
        .get("summary")
        .and_then(|s| s.get("errors"))
        .and_then(|x| x.as_u64())
        .unwrap_or(0) as usize;
    let warnings = v
        .get("summary")
        .and_then(|s| s.get("warnings"))
        .and_then(|x| x.as_u64())
        .unwrap_or(0) as usize;
    (errors, warnings)
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
        // Use file_type() (does not follow symlinks) to skip symlinked entries entirely.
        // Following symlinks could traverse outside the studio sandbox root.
        let ft = match e.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if ft.is_symlink() {
            continue;
        }
        let mut sub = rel.to_path_buf();
        sub.push(&name);
        let sub_s = sub.to_string_lossy().replace('\\', "/");
        if ft.is_dir() {
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

fn allowed_studio_command(cmd: &str) -> bool {
    matches!(
        cmd,
        "npm"
            | "npm.cmd"
            | "pnpm"
            | "pnpm.cmd"
            | "yarn"
            | "yarn.cmd"
            | "cargo"
            | "cargo.exe"
            | "git"
            | "git.exe"
    )
}

/// `npm exec -- vite build` uniquement (secours vérification post-tâche Code Studio sans `tsc`).
fn npm_exec_argv_allowed_for_studio_verify(argv: &[String]) -> bool {
    argv.len() == 5
        && argv[0] == "npm"
        && argv[1] == "exec"
        && argv[2] == "--"
        && argv[3] == "vite"
        && argv[4] == "build"
}

fn allowed_studio_subcommand(cmd: &str, argv: &[String]) -> bool {
    let subcommand = argv.get(1).map(String::as_str);
    match cmd {
        "npm" | "npm.cmd" | "pnpm" | "pnpm.cmd" => {
            matches!(subcommand, Some("run") | Some("install") | Some("ci"))
                || (matches!(subcommand, Some("exec")) && npm_exec_argv_allowed_for_studio_verify(argv))
        }
        "yarn" | "yarn.cmd" => {
            matches!(
                subcommand,
                Some("run")
                    | Some("install")
                    | Some("build")
                    | Some("preview")
                    | Some("dev")
                    | Some("start")
                    | Some("test")
            )
        }
        "cargo" | "cargo.exe" => {
            matches!(
                subcommand,
                Some("build")
                    | Some("check")
                    | Some("run")
                    | Some("test")
                    | Some("fmt")
                    | Some("clippy")
            )
        }
        "git" | "git.exe" => {
            matches!(subcommand, Some("rev-parse") | Some("status") | Some("diff"))
        }
        _ => false,
    }
}

fn argv_looks_safe(argv: &[String]) -> bool {
    if argv.is_empty() {
        return false;
    }

    let cmd = argv[0].as_str();
    let cmd_path = Path::new(cmd);
    if cmd_path.is_absolute()
        || cmd.contains('/')
        || cmd.contains('\\')
        || !allowed_studio_command(cmd)
        || !allowed_studio_subcommand(cmd, argv)
    {
        return false;
    }

    for a in argv.iter().skip(1) {
        let arg_path = Path::new(a);
        if a.is_empty()
            || a.contains("..")
            || a.contains(';')
            || a.contains('|')
            || a.contains('&')
            || a.contains('`')
            || a.contains('\n')
            || a.contains('\r')
            || arg_path.is_absolute()
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

/// Motifs d’exclusion ajoutés automatiquement quand `tsc` compile des tests comme du code d’app (describe/it/expect).
const TSC_EXCLUDE_TEST_PATTERNS: &[&str] = &[
    "**/*.test.ts",
    "**/*.test.tsx",
    "**/*.test.js",
    "**/*.test.jsx",
    "**/*.spec.ts",
    "**/*.spec.tsx",
    "**/*.spec.js",
    "**/*.spec.jsx",
    "src/tests",
    "src/test",
    "src/__tests__",
];

fn format_verify_failure(code: Option<i32>, out: &str, err: &str) -> String {
    let code_str = match code {
        Some(n) => format!("code de sortie {n}"),
        None => "terminaison sans code numérique (signal ou erreur système)".to_string(),
    };
    let out_s: String = out.chars().take(4000).collect();
    let err_s: String = if err.trim().is_empty() {
        "(vide — avec npm run build, les erreurs TypeScript sont souvent sur stdout ci-dessus.)".to_string()
    } else {
        err.chars().take(4000).collect()
    };
    let has_syntax_marker = out.contains("TS1128")
        || err.contains("TS1128")
        || out.contains("TS1005")
        || err.contains("TS1005")
        || out.contains("TS1434")
        || err.contains("TS1434");
    let syntax_hint = if has_syntax_marker {
        "\n\nNote : TS1128 / TS1005 / TS1434 indiquent souvent du texte invalide dans le fichier source (markdown, phrase hors code, accolade en trop) aux lignes indiquées."
    } else {
        ""
    };
    format!(
        "Vérification post-tâche échouée ({code_str}).\n--- stdout ---\n{out_s}\n--- stderr ---\n{err_s}{syntax_hint}"
    )
}

/// Détecte l’échec typique : fichiers `*.test.*` / `src/tests` passés dans `tsc` du build sans types Vitest/Jest.
/// Erreurs du type « paquet non installé » / résolution de module.
fn looks_like_missing_npm_module(stdout: &str, stderr: &str) -> bool {
    let combined = format!("{stdout}\n{stderr}").to_ascii_lowercase();
    combined.contains("cannot find module")
        || combined.contains("cannot resolve")
        || combined.contains("module not found")
        || combined.contains("err_module_not_found")
        || combined.contains("ts2307")
        || combined.contains("failed to resolve import")
        || (combined.contains("npm err!") && (combined.contains("enoent") || combined.contains("not found")))
}

/// Retire les commentaires de ligne `//` (lignes où, après espaces, le reste commence par `//`).
/// Objectif : pouvoir fusionner des `tsconfig*.json` souvent en JSONC léger, sans dépendance jsonc.
fn strip_tsconfig_line_comments(raw: &str) -> String {
    let mut kept: Vec<&str> = Vec::new();
    for line in raw.lines() {
        let t = line.trim_start();
        if t.starts_with("//") {
            continue;
        }
        let t2 = t.trim();
        if t2.starts_with("/*") && t2.ends_with("*/") && !t2.contains('\n') {
            continue;
        }
        kept.push(line);
    }
    kept.join("\n")
}

/// Parse JSON de tsconfig : strict puis sans commentaires de ligne.
fn parse_tsconfig_json_for_merge(raw: &str) -> Option<serde_json::Value> {
    serde_json::from_str(raw)
        .ok()
        .or_else(|| serde_json::from_str(&strip_tsconfig_line_comments(raw)).ok())
        .or_else(|| {
            // Virgules finales simples avant `}` ou `]` (cas fréquent dans tsconfig Vite).
            let relaxed = strip_tsconfig_line_comments(raw);
            let re = Regex::new(r",(\s*[\]}])").ok()?;
            let mut collapsed = relaxed;
            for _ in 0..8 {
                let next = re.replace_all(&collapsed, "$1").to_string();
                if next == collapsed {
                    break;
                }
                collapsed = next;
            }
            serde_json::from_str(&collapsed).ok()
        })
}

fn looks_like_ts_tests_compiled_in_app_build(stdout: &str, stderr: &str) -> bool {
    let combined = format!("{stdout}\n{stderr}");
    let test_path = combined.contains(".test.")
        || combined.contains(".spec.")
        || combined.contains("src/tests/")
        || combined.contains("src\\tests\\")
        || combined.contains("/tests/")
        || combined.contains("\\tests\\");
    if !test_path {
        return false;
    }
    let jest_hint = combined.contains("@types/jest")
        || combined.contains("@types/mocha")
        || combined.contains("vitest/globals")
        || combined.contains("Try `npm i --save-dev @types/jest`");
    let ts_runner = (combined.contains("TS2582") && combined.contains("describe"))
        || (combined.contains("TS2582") && combined.contains("'it'"))
        || (combined.contains("TS2582") && combined.contains("`it`"))
        || (combined.contains("TS2304") && combined.contains("expect"));
    let ts_noise_in_tests =
        combined.contains("TS6196") || combined.contains("TS6133");
    test_path && (ts_runner || jest_hint || ts_noise_in_tests)
}

/// Erreurs `tsc` typiques quand le code généré est partiellement incohérent (exports, props, imports) alors que Vite peut encore produire un bundle.
fn looks_like_tsc_codegen_errors_for_vite_fallback(stdout: &str, stderr: &str) -> bool {
    let c = format!("{stdout}\n{stderr}");
    if !c.contains("error TS") {
        return false;
    }
    c.contains("TS2305")
        || c.contains("TS2613")
        || c.contains("TS2339")
        || c.contains("TS2459")
        || c.contains("TS2322")
        || c.contains("TS2345")
        || c.contains("TS2741")
        || c.contains("TS6133")
}

/// Ne pas modifier les `tsconfig.json` « solution » (références uniquement) : ils n’acceptent pas `exclude` utile pour les sous-projets.
fn tsconfig_eligible_for_exclude_patch(v: &serde_json::Value) -> bool {
    let Some(o) = v.as_object() else {
        return false;
    };
    if o.contains_key("compilerOptions") {
        return true;
    }
    if let Some(serde_json::Value::Array(a)) = o.get("include") {
        return !a.is_empty();
    }
    false
}

/// Fusionne `exclude` pour retirer les tests du périmètre `tsc`. Retourne `true` si le fichier a été modifié.
fn try_merge_exclude_into_tsconfig(path: &Path) -> Result<bool, String> {
    if !path.is_file() {
        return Ok(false);
    }
    let raw = fs::read_to_string(path).map_err(|e| format!("lecture {}: {e}", path.display()))?;
    let mut v: serde_json::Value = match parse_tsconfig_json_for_merge(&raw) {
        Some(v) => v,
        None => return Ok(false),
    };
    if !tsconfig_eligible_for_exclude_patch(&v) {
        return Ok(false);
    }
    let Some(obj) = v.as_object_mut() else {
        return Ok(false);
    };
    let mut list: Vec<String> = match obj.get("exclude") {
        Some(serde_json::Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect(),
        None => Vec::new(),
        Some(_) => return Ok(false),
    };
    let mut changed = false;
    for p in TSC_EXCLUDE_TEST_PATTERNS {
        if !list.iter().any(|e| e == p) {
            list.push((*p).to_string());
            changed = true;
        }
    }
    if !changed {
        return Ok(false);
    }
    obj.insert("exclude".to_string(), json!(list));
    let pretty = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
    fs::write(path, format!("{pretty}\n"))
        .map_err(|e| format!("écriture {}: {e}", path.display()))?;
    Ok(true)
}

/// Tente de corriger les tsconfig pour exclure les tests, puis indique ce qui a été modifié (pour les messages d’erreur).
fn try_autofix_tsconfig_exclude_tests_for_build(project_root: &Path) -> Result<Option<String>, String> {
    let mut patched = Vec::new();
    for name in ["tsconfig.app.json", "tsconfig.json", "tsconfig.node.json"] {
        let p = project_root.join(name);
        if try_merge_exclude_into_tsconfig(&p)? {
            patched.push(name.to_string());
        }
    }
    if patched.is_empty() {
        Ok(None)
    } else {
        Ok(Some(format!(
            "fusion des exclusions de tests dans {}",
            patched.join(", ")
        )))
    }
}

const AKASHA_TEST_SHIM_REL: &str = "src/akasha-studio-test-globals.d.ts";
const AKASHA_TEST_SHIM_MARKER: &str = "akasha-studio-auto: test globals shim";

/// Déclarations minimales pour que `tsc` du build d’application accepte les fichiers `*.test.*` sans @types/jest.
fn try_write_test_globals_shim(project_root: &Path) -> Result<bool, String> {
    let p = project_root.join(AKASHA_TEST_SHIM_REL);
    if p.is_file() {
        let existing = fs::read_to_string(&p).map_err(|e| e.to_string())?;
        if existing.contains(AKASHA_TEST_SHIM_MARKER) {
            return Ok(false);
        }
        return Ok(false);
    }
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let body = format!(
        "// {AKASHA_TEST_SHIM_MARKER}\n\
         // Ajouté automatiquement par Akasha pour la vérification post-tâche (build) Code Studio.\n\
         declare function describe(...args: unknown[]): void;\n\
         declare function it(...args: unknown[]): void;\n\
         declare function expect(...args: unknown[]): unknown;\n"
    );
    fs::write(&p, body).map_err(|e| e.to_string())?;
    Ok(true)
}

async fn try_npm_install_autofix(project_root: &Path, timeout_sec: u64) {
    let argv = vec!["npm".into(), "install".into()];
    if !argv_looks_safe(&argv) {
        return;
    }
    let cap = timeout_sec
        .min(AUTO_VERIFY_NPM_INSTALL_CAP_SEC)
        .max(90);
    let _ = studio_run_command_capture(project_root, &argv, cap).await;
}

/// Sections canoniques du plan — si l’une apparaît plus d’une fois, c’est en général un second gabarit collé en bas.
const CODE_STUDIO_PLAN_SECTION_HEADINGS: &[&str] = &[
    "## Description",
    "## Scope",
    "## Stack",
    "## Structure du projet",
    "## Commandes",
    "## Fichiers hors scope",
    "## Demandes d'évolutions utilisateur par phase",
    "## Recommandations",
    "## Todos",
    "## Informations complémentaires",
];

fn studio_reject_polluted_plan_content(content: &str) -> Option<String> {
    for heading in CODE_STUDIO_PLAN_SECTION_HEADINGS {
        if content.matches(heading).count() > 1 {
            return Some(format!(
                "rejected: section « {heading} » dupliquée dans CODE_STUDIO_PLAN.md — modifier la section existante (search_replace sur ce bloc), ne pas recoller un second gabarit ni réécrire tout le fichier en bas."
            ));
        }
    }
    let titre_lines = content
        .lines()
        .filter(|l| l.trim().starts_with("# Titre"))
        .count();
    if titre_lines > 1 {
        return Some(
            "rejected: plusieurs lignes « # Titre » dans CODE_STUDIO_PLAN.md — conserver une seule ligne de titre en tête du fichier."
                .to_string(),
        );
    }
    None
}

fn is_ts_js_like_ext(ext: &str) -> bool {
    matches!(
        ext,
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs"
    )
}

/// Lignes type « **4. foo.ts** - … » ou inventaire markdown dans du code (hors commentaires de ligne simples).
fn code_file_has_markdown_instruction_lines(ext: &str, content: &str) -> bool {
    if !is_ts_js_like_ext(ext) {
        return false;
    }
    let bold_line = match Regex::new(r"^\s*\*\*.+\*\*") {
        Ok(re) => re,
        Err(_) => return false,
    };
    let numbered_file_hint = match Regex::new(r"^\s*\d+\.\s+\S+\.(ts|tsx|js|jsx)\b") {
        Ok(re) => re,
        Err(_) => return false,
    };
    for line in content.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if t.starts_with("//") {
            continue;
        }
        if t.starts_with("/*") {
            continue;
        }
        // Ne pas confondre « * ligne de bloc » avec du gras markdown « **…** ».
        if t.starts_with('*') && !t.starts_with("**") {
            continue;
        }
        if bold_line.is_match(t)
            && (t.contains(" - ")
                || t.contains(" — ")
                || t.contains('—')
                || t.contains(".ts")
                || t.contains(".tsx")
                || t.contains(".js")
                || t.contains(".jsx"))
        {
            return true;
        }
        if numbered_file_hint.is_match(t)
            && (t.contains(" - ") || t.contains(" — ") || t.contains('—'))
        {
            return true;
        }
    }
    false
}

/// Reject obvious LLM markdown / tool-protocol leakage in code files under Code Studio.
pub fn studio_reject_polluted_code_content(disk_path: &Path, content: &str) -> Option<String> {
    if disk_path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case("CODE_STUDIO_PLAN.md"))
    {
        return studio_reject_polluted_plan_content(content);
    }

    let ext = disk_path.extension()?.to_string_lossy().to_lowercase();
    let code_ext = is_agent_code_file_extension(ext.as_str());
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
            || s.starts_with("TOOL: write_code")
            || (s.starts_with("TOOL: ")
                && (s.contains("write_file") || s.contains("write_code") || s.contains("apply_patch")))
    }) {
        return Some(
            "rejected: le fichier contient des lignes de protocole d’outil — le contenu doit être uniquement du code source."
                .to_string(),
        );
    }
    if code_file_has_markdown_instruction_lines(ext.as_str(), content) {
        return Some(
            "rejected: le fichier code contient des lignes de type markdown / consigne utilisateur (**…**, « N. fichier.ts — … ») — ce texte doit aller uniquement dans la réponse chat, pas dans le source."
                .to_string(),
        );
    }
    None
}

/// Timeout (secondes) pour `command_ok` dans les critères d'acceptation — aligné sur `verify_timeout_sec` / build studio.
pub(crate) fn studio_project_verify_timeout_sec(project_root: &Path) -> u64 {
    load_studio_meta(project_root)
        .and_then(|m| m.verify_timeout_sec)
        .unwrap_or(DEFAULT_BUILD_TIMEOUT_SEC)
        .min(3600)
        .max(1)
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
    let is_npm_run_build =
        argv.len() == 3 && argv[0] == "npm" && argv[1] == "run" && argv[2] == "build";
    match studio_run_command_capture(project_root, &argv, timeout_sec).await {
        Ok((Some(0), _, _)) => Ok(()),
        Ok((mut code, mut out, mut err)) => {
            let first_err = format_verify_failure(code, &out, &err);
            if !is_npm_run_build {
                return Err(first_err);
            }

            let mut fix_notes: Vec<String> = Vec::new();

            if looks_like_missing_npm_module(&out, &err) {
                fix_notes.push("npm install (dépendances manquantes détectées)".to_string());
                try_npm_install_autofix(project_root, timeout_sec).await;
                match studio_run_command_capture(project_root, &argv, timeout_sec).await {
                    Ok((Some(0), _, _)) => return Ok(()),
                    Ok((c, o, e)) => {
                        code = c;
                        out = o;
                        err = e;
                    }
                    Err(e2) => {
                        return Err(format!(
                            "{first_err}\n\n[Tentatives automatiques : {}]\nVérification post-tâche après npm install: {e2}",
                            fix_notes.join(" ; ")
                        ));
                    }
                }
            }

            if looks_like_ts_tests_compiled_in_app_build(&out, &err) {
                match try_autofix_tsconfig_exclude_tests_for_build(project_root) {
                    Ok(Some(note)) => {
                        fix_notes.push(note.clone());
                        match studio_run_command_capture(project_root, &argv, timeout_sec).await {
                            Ok((Some(0), _, _)) => return Ok(()),
                            Ok((c, o, e)) => {
                                code = c;
                                out = o;
                                err = e;
                            }
                            Err(e2) => {
                                return Err(format!(
                                    "{first_err}\n\n[Tentatives automatiques : {}]\n{}",
                                    fix_notes.join(" ; "),
                                    e2
                                ));
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(fix_err) => fix_notes.push(format!("tsconfig exclude : {fix_err}")),
                }

                if looks_like_ts_tests_compiled_in_app_build(&out, &err) {
                    match try_write_test_globals_shim(project_root) {
                        Ok(true) => {
                            fix_notes.push(format!(
                                "fichier {AKASHA_TEST_SHIM_REL} (globals describe/it/expect)"
                            ));
                            match studio_run_command_capture(project_root, &argv, timeout_sec).await {
                                Ok((Some(0), _, _)) => return Ok(()),
                                Ok((c2, o2, e2)) => {
                                    return Err(format!(
                                        "{first_err}\n\n[Tentatives automatiques : {}]\n{}",
                                        fix_notes.join(" ; "),
                                        format_verify_failure(c2, &o2, &e2)
                                    ));
                                }
                                Err(e2) => {
                                    return Err(format!(
                                        "{first_err}\n\n[Tentatives automatiques : {}]\nVérification post-tâche: {e2}",
                                        fix_notes.join(" ; ")
                                    ));
                                }
                            }
                        }
                        Ok(false) => {}
                        Err(shim_err) => fix_notes.push(format!("shim tests : {shim_err}")),
                    }
                }
            }

            // Dernier secours : le script `npm run build` enchaîne souvent `tsc` puis Vite ; si le code généré
            // est incohérent pour TypeScript mais Vite peut encore bundler, on tente uniquement `vite build`
            // (local, sans réseau — exige `node_modules/vite`).
            if project_root.join("node_modules/vite/package.json").is_file()
                && looks_like_tsc_codegen_errors_for_vite_fallback(&out, &err)
            {
                let vite_argv = vec![
                    "npm".into(),
                    "exec".into(),
                    "--".into(),
                    "vite".into(),
                    "build".into(),
                ];
                if argv_looks_safe(&vite_argv) {
                    fix_notes.push(
                        "secours: npm exec -- vite build (sans étape tsc du script build)".into(),
                    );
                    match studio_run_command_capture(project_root, &vite_argv, timeout_sec).await {
                        Ok((Some(0), _, _)) => return Ok(()),
                        Ok((cv, ov, ev)) => {
                            fix_notes.push(format!(
                                "vite build secours — {}",
                                format_verify_failure(cv, &ov, &ev)
                                    .chars()
                                    .take(900)
                                    .collect::<String>()
                            ));
                        }
                        Err(ve) => fix_notes.push(format!("vite build secours: {ve}")),
                    }
                }
            }

            let last = format_verify_failure(code, &out, &err);
            if fix_notes.is_empty() {
                Err(last)
            } else {
                Err(format!(
                    "{last}\n\n[Tentatives automatiques : {}]",
                    fix_notes.join(" ; ")
                ))
            }
        }
        Err(e) => Err(format!("Vérification post-tâche: {}", e)),
    }
}

fn write_initial_code_studio_plan(project_root: &Path, name: &str, tech_stack: Option<&str>) -> Result<(), String> {
    let plan_path = project_root.join("CODE_STUDIO_PLAN.md");
    if plan_path.exists() {
        return Ok(());
    }
    let stack_body = tech_stack
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "(À compléter — stack enregistrée dans `.akasha-studio.json` ou déduite du manifeste.)".to_string());
    let body = format!(
        r#"# Titre : {name}

## Description

(Brève description du produit et de l’usage attendu.)

## Scope

- **Inclus** : …
- **Exclus / hors périmètre actuel** : …

## Stack

{stack_body}

## Structure du projet

(Grands dossiers ou modules importants ; rester concis.)

## Commandes

- **Dev** : …
- **Build** : …
- **Tests** : …

## Fichiers hors scope

(Fichiers ou zones que les agents ne modifient pas sans instruction explicite — ex. `node_modules`, artefacts de build.)

## Demandes d'évolutions utilisateur par phase

- (Une ligne datée par demande ou regroupement logique ; lier à la phase ou au lot concerné.)

## Recommandations

(Pièges connus, ordre de lecture du code, conventions à respecter.)

## Todos

- [ ] …

## Informations complémentaires

(Notes diverses, liens internes, détails d’exécution, résultats de commandes utiles.)

---

_Gabarit Code Studio (Akasha) : **conserver ces titres de section** (`## …`). Pour une modification mineure, **ne pas** réécrire tout le fichier : éditer uniquement les sections concernées et ajouter des **lignes datées** dans *Informations complémentaires* ou *Demandes d'évolutions utilisateur par phase* pour l’historique des lots._
"#,
        name = name,
        stack_body = stack_body,
    );
    fs::write(&plan_path, body).map_err(|e| e.to_string())
}

/// Schedule Code Studio code-RAG indexing without blocking the UI / tool call.
/// `force=false` still rescans the tree but reuses unchanged file chunks; changed/deleted files converge.
pub fn schedule_studio_code_rag_index(
    data_dir: &Path,
    project_id: &str,
    project_root: &Path,
    force: bool,
) {
    let data_dir = data_dir.to_path_buf();
    let project_id = project_id.to_string();
    let project_root = project_root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let store = crate::code_rag::CodeRagStore::new(&data_dir);
        if let Err(e) = store.ensure_index(&project_id, &project_root, force) {
            tracing::warn!(
                project_id = %project_id,
                project_root = %project_root.display(),
                error = %e,
                "Code Studio code-RAG background indexing failed"
            );
        }
    });
}

async fn git_output(project_root: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    let mut c = Command::new("git");
    c.args(args).current_dir(project_root).kill_on_drop(true);
    c.output().await.map_err(|e| e.to_string())
}

async fn git_current_branch(project_root: &Path) -> Result<String, String> {
    let o = git_output(project_root, &["symbolic-ref", "--short", "HEAD"]).await?;
    if !o.status.success() {
        return Err(String::from_utf8_lossy(&o.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

async fn git_has_pending_changes(project_root: &Path) -> Result<bool, String> {
    let o = git_output(project_root, &["status", "--porcelain"]).await?;
    if !o.status.success() {
        return Err(String::from_utf8_lossy(&o.stderr).trim().to_string());
    }
    Ok(!String::from_utf8_lossy(&o.stdout).trim().is_empty())
}

/// Before merging an evolution branch, persist pending local edits on that branch.
/// Returns `Ok(true)` when an auto-commit was created.
async fn ensure_evolution_branch_committed_before_merge(
    project_root: &Path,
    evolution_branch: &str,
) -> Result<bool, String> {
    let current = git_current_branch(project_root).await?;
    let has_pending = git_has_pending_changes(project_root).await?;
    if !has_pending {
        return Ok(false);
    }
    if current != evolution_branch {
        return Err(format!(
            "pending_local_changes_on_branch:{current}; expected:{evolution_branch}"
        ));
    }
    let add = git_output(project_root, &["add", "-A"]).await?;
    if !add.status.success() {
        return Err(format!(
            "git_add_failed: {}",
            String::from_utf8_lossy(&add.stderr).trim()
        ));
    }
    let commit = git_output(
        project_root,
        &[
            "commit",
            "-m",
            "Akasha Code Studio: save pending evolution changes",
        ],
    )
    .await?;
    if !commit.status.success() {
        let err = String::from_utf8_lossy(&commit.stderr).trim().to_string();
        let out = String::from_utf8_lossy(&commit.stdout).trim().to_string();
        // If nothing actually changed after `add -A`, treat as non-fatal.
        if err.contains("nothing to commit") || out.contains("nothing to commit") {
            return Ok(false);
        }
        return Err(format!("git_commit_failed: {err}"));
    }
    Ok(true)
}

async fn is_git_repo(project_root: &Path) -> bool {
    match git_output(project_root, &["rev-parse", "--is-inside-work-tree"]).await {
        Ok(o) => o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true",
        Err(_) => false,
    }
}

/// Current branch, whether the worktree is clean, and bounded porcelain lines for UI.
async fn git_branch_clean_worktree_lines(
    project_root: &Path,
) -> (Option<String>, Option<bool>, Vec<serde_json::Value>) {
    if !is_git_repo(project_root).await {
        return (None, None, Vec::new());
    }
    let branch = match git_output(project_root, &["rev-parse", "--abbrev-ref", "HEAD"]).await {
        Ok(o) if o.status.success() => Some(String::from_utf8_lossy(&o.stdout).trim().to_string()),
        _ => None,
    };
    let mut lines_out: Vec<serde_json::Value> = Vec::new();
    let clean = match git_output(project_root, &["status", "--porcelain"]).await {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout).to_string();
            let empty = text.trim().is_empty();
            for (i, raw) in text.lines().enumerate() {
                if i >= 200 {
                    break;
                }
                let line = raw.trim_end();
                if line.is_empty() {
                    continue;
                }
                let status: String = line.chars().take(2).collect();
                let path = if line.len() > 3 {
                    line[3..].trim_start()
                } else {
                    ""
                };
                if path.is_empty() {
                    continue;
                }
                lines_out.push(serde_json::json!({
                    "status": status,
                    "path": path.chars().take(4096).collect::<String>()
                }));
            }
            Some(empty)
        }
        _ => None,
    };
    (branch, clean, lines_out)
}

/// Ensure at least one primary branch exists and is checked out.
/// Priority: existing `main`, existing `master`, otherwise create `main`.
async fn ensure_main_or_master_branch(project_root: &Path) -> Result<(), String> {
    if !is_git_repo(project_root).await {
        return Ok(());
    }
    let head = git_output(project_root, &["symbolic-ref", "--short", "HEAD"]).await;
    if let Ok(o) = head {
        if o.status.success() {
            let cur = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if cur == "main" || cur == "master" {
                return Ok(());
            }
        }
    }
    let has_main = git_output(project_root, &["show-ref", "--verify", "--quiet", "refs/heads/main"])
        .await
        .map(|o| o.status.success())
        .unwrap_or(false);
    if has_main {
        let _ = git_output(project_root, &["checkout", "main"]).await?;
        return Ok(());
    }
    let has_master = git_output(project_root, &["show-ref", "--verify", "--quiet", "refs/heads/master"])
        .await
        .map(|o| o.status.success())
        .unwrap_or(false);
    if has_master {
        let _ = git_output(project_root, &["checkout", "master"]).await?;
        return Ok(());
    }
    for args in [["checkout", "-B", "main"], ["switch", "-c", "main"], ["checkout", "-b", "main"]] {
        match git_output(project_root, &args).await {
            Ok(o) if o.status.success() => return Ok(()),
            _ => {}
        }
    }
    Err("failed to ensure main/master branch".to_string())
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



mod handlers;

pub use handlers::handle_studio_route;



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
    use super::{ensure_main_or_master_branch, studio_command_from_argv};
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
    fn studio_reject_polluted_detects_markdown_instruction_in_ts() {
        use super::studio_reject_polluted_code_content;
        use std::path::Path;
        let p = Path::new("src/gameLogic.ts");
        let bad = "function f() {}\n\n**4. gameLogic.ts** - Ajouter l'export de minimax\n";
        assert!(studio_reject_polluted_code_content(p, bad).is_some());
        let bad2 = "function f() {}\n\n4. gameLogic.ts — ajouter export\n";
        assert!(studio_reject_polluted_code_content(p, bad2).is_some());
        let ok = "function f() {}\n// **note** fichier.ts — pour humain\n";
        assert!(studio_reject_polluted_code_content(p, ok).is_none());
    }

    #[test]
    fn studio_reject_polluted_plan_rejects_duplicate_section() {
        use super::studio_reject_polluted_code_content;
        use std::path::Path;
        let p = Path::new("CODE_STUDIO_PLAN.md");
        let dup = "## Description\nA\n## Scope\nB\n## Description\nC\n";
        let msg = studio_reject_polluted_code_content(p, dup).expect("expected rejection");
        assert!(msg.contains("## Description"));
        let ok = "# Titre : jeu\n\n## Description\nUne seule fois.\n## Scope\nx\n";
        assert!(studio_reject_polluted_code_content(p, ok).is_none());
    }

    #[test]
    fn studio_reject_polluted_plan_rejects_duplicate_titre_line() {
        use super::studio_reject_polluted_code_content;
        use std::path::Path;
        let p = Path::new("CODE_STUDIO_PLAN.md");
        let dup = "# Titre : a\n\n## Description\nx\n\n# Titre : b\n";
        assert!(studio_reject_polluted_code_content(p, dup).is_some());
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

    #[test]
    fn looks_like_ts_tests_in_app_build_detects_tsc_test_errors() {
        use super::looks_like_ts_tests_compiled_in_app_build;
        let sample = r"src/tests/AntiAIMode.test.tsx(8,1): error TS2582: Cannot find name 'describe'.
Try `npm i --save-dev @types/jest`";
        assert!(looks_like_ts_tests_compiled_in_app_build(sample, ""));
        assert!(!looks_like_ts_tests_compiled_in_app_build(
            "src/App.tsx(1,1): error TS2322: Type 'number' is not assignable to type 'string'.",
            ""
        ));
    }

    #[test]
    fn strip_tsconfig_line_comments_drops_slash_slash_lines() {
        use super::strip_tsconfig_line_comments;
        let raw = "{\n  // hi\n  \"x\": 1\n}";
        let s = strip_tsconfig_line_comments(raw);
        assert!(serde_json::from_str::<serde_json::Value>(&s).is_ok());
    }

    #[test]
    fn parse_tsconfig_json_for_merge_accepts_trailing_comma() {
        use super::parse_tsconfig_json_for_merge;
        let raw = r#"{"compilerOptions": { "strict": true, },}"#;
        assert!(parse_tsconfig_json_for_merge(raw).is_some());
    }

    #[test]
    fn looks_like_tsc_codegen_errors_for_vite_fallback_detects_export_mismatch() {
        use super::looks_like_tsc_codegen_errors_for_vite_fallback;
        let s = "src/ai.ts(6,15): error TS2305: Module '\"./gameLogic\"' has no exported member 'Player'.";
        assert!(looks_like_tsc_codegen_errors_for_vite_fallback(s, ""));
        assert!(!looks_like_tsc_codegen_errors_for_vite_fallback("no type errors here", ""));
    }

    #[test]
    fn npm_exec_vite_build_argv_is_whitelisted() {
        use super::{argv_looks_safe, npm_exec_argv_allowed_for_studio_verify};
        let v = vec![
            "npm".into(),
            "exec".into(),
            "--".into(),
            "vite".into(),
            "build".into(),
        ];
        assert!(npm_exec_argv_allowed_for_studio_verify(&v));
        assert!(argv_looks_safe(&v));
    }

    #[test]
    fn merge_tsconfig_exclude_adds_test_globs() {
        use super::try_merge_exclude_into_tsconfig;
        let dir = std::env::temp_dir().join(format!(
            "akasha_tsconfig_exclude_test_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tsconfig.app.json");
        std::fs::write(
            &path,
            r#"{"compilerOptions":{"strict":true},"include":["src"]}"#,
        )
        .unwrap();
        assert!(try_merge_exclude_into_tsconfig(&path).unwrap());
        let raw = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let ex = v["exclude"].as_array().unwrap();
        assert!(ex.iter().any(|x| x == "**/*.test.tsx"));
        assert!(!try_merge_exclude_into_tsconfig(&path).unwrap());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn ensure_main_or_master_branch_creates_main_when_missing() {
        use std::process::Command;
        use std::time::{SystemTime, UNIX_EPOCH};

        let git_available = Command::new("git")
            .arg("--version")
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !git_available {
            return;
        }

        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("akasha_studio_main_guard_{stamp}_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let init = Command::new("git").arg("init").current_dir(&dir).status().unwrap();
        assert!(init.success());

        // Move default branch to a non-primary name so neither main nor master exist.
        let rename = Command::new("git")
            .args(["branch", "-m", "feature/tmp"])
            .current_dir(&dir)
            .status()
            .unwrap();
        assert!(rename.success());

        ensure_main_or_master_branch(&dir).await.expect("must create or checkout primary branch");

        let head = Command::new("git")
            .args(["symbolic-ref", "--short", "HEAD"])
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(head.status.success());
        let branch = String::from_utf8_lossy(&head.stdout).trim().to_string();
        assert_eq!(branch, "main");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn ensure_evolution_branch_committed_before_merge_auto_commits_pending_changes() {
        use std::process::Command;
        use std::time::{SystemTime, UNIX_EPOCH};

        let git_available = Command::new("git")
            .arg("--version")
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !git_available {
            return;
        }

        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "akasha_studio_evo_commit_{stamp}_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let init = Command::new("git").arg("init").current_dir(&dir).status().unwrap();
        assert!(init.success());
        let _ = Command::new("git")
            .args(["config", "user.name", "Akasha Test"])
            .current_dir(&dir)
            .status();
        let _ = Command::new("git")
            .args(["config", "user.email", "akasha-test@example.com"])
            .current_dir(&dir)
            .status();

        std::fs::write(dir.join("README.md"), "hello\n").unwrap();
        assert!(Command::new("git")
            .args(["add", "README.md"])
            .current_dir(&dir)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&dir)
            .status()
            .unwrap()
            .success());

        assert!(Command::new("git")
            .args(["checkout", "-b", "studio/e2e"])
            .current_dir(&dir)
            .status()
            .unwrap()
            .success());
        std::fs::write(dir.join("README.md"), "hello\nchanges\n").unwrap();

        let committed = super::ensure_evolution_branch_committed_before_merge(&dir, "studio/e2e")
            .await
            .expect("auto-commit should succeed");
        assert!(committed);

        let status = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(status.status.success());
        assert!(String::from_utf8_lossy(&status.stdout).trim().is_empty());

        let log = Command::new("git")
            .args(["log", "-1", "--pretty=%s"])
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(log.status.success());
        let msg = String::from_utf8_lossy(&log.stdout);
        assert!(msg.contains("Akasha Code Studio: save pending evolution changes"));

        std::fs::remove_dir_all(&dir).ok();
    }
}
