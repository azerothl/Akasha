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
    looks_like_code_studio_tool_marker_but_unparsed,
};
pub(crate) use response_auditor::{
    studio_llm_audit_code_studio_turn, studio_llm_response_auditor_enabled, StudioLlmAuditParams,
};

use crate::api_http::json_response;
use crate::studio::{is_strictly_under_studio_root, resolve_studio_project_dir, studio_projects_base};
use crate::studio_task_snapshot::EXCLUDED_DIR_NAMES;
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

/// Plan d’exécution pour `POST /api/studio/projects/:id/preview/start` détecté
/// à partir des fichiers manifestes du projet (package.json, pyproject.toml, …).
///
/// Reste volontairement minimaliste : le daemon n’embarque pas un vrai détecteur
/// de stack, juste les conventions les plus courantes pour permettre l’aperçu
/// au-delà de Node.js.
#[derive(Debug, Clone)]
pub struct StudioPreviewPlan {
    /// Libellé lisible affiché dans l’UI / les réponses (« Node.js (npm) », « Python · uv · Streamlit », …).
    pub label: String,
    /// Commande d’installation des dépendances (ex. `["npm","install"]`, `["uv","sync"]`).
    /// `None` = pas d’étape d’installation gérée par le daemon.
    pub install_argv: Option<Vec<String>>,
    /// Si présent et que ce chemin existe (relatif au projet), l’installation est sautée
    /// sauf `force=true` (ex. `node_modules` pour Node, `.venv` pour uv).
    pub install_skip_when_present: Option<PathBuf>,
    /// Commande de lancement du serveur de dev — déjà bornée au port choisi.
    pub run_argv: Vec<String>,
}

/// Cherche `streamlit` ou `fastapi` dans le contenu d’un `pyproject.toml`.
///
/// Heuristique simple (pas de parser TOML) : on regarde si le mot apparaît dans
/// le fichier en minuscules. Suffisant pour les conventions habituelles
/// (`dependencies = ["streamlit", ...]`, `streamlit = "^1.0"`, etc.).
fn pyproject_mentions(content_lower: &str, needle: &str) -> bool {
    content_lower
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .any(|tok| tok == needle)
}

/// Première entrée existante parmi `candidates` (chemins relatifs au projet).
fn first_existing_relative(project_root: &Path, candidates: &[&str]) -> Option<String> {
    for name in candidates {
        if project_root.join(name).is_file() {
            return Some((*name).to_string());
        }
    }
    None
}

/// Détecte la stack technique du projet et renvoie le plan d’aperçu correspondant.
///
/// Priorités :
/// 1. `package.json` → Node.js (npm) — `npm install` + `npm run dev -- --host 127.0.0.1 --port <p>`.
/// 2. `pyproject.toml` mentionnant `streamlit` → Python (uv) Streamlit — `uv sync` + `uv run streamlit run …`.
/// 3. `pyproject.toml` mentionnant `fastapi` → Python (uv) FastAPI — `uv sync` + `uv run uvicorn main:app …`.
/// 4. Sinon : `Err` avec un indice pour configurer la stack manuellement.
pub fn detect_studio_preview_plan(project_root: &Path, port: u16) -> Result<StudioPreviewPlan, String> {
    let port_s = port.to_string();

    // 1) Node.js — comportement historique
    if project_root.join("package.json").is_file() {
        return Ok(StudioPreviewPlan {
            label: "Node.js (npm)".to_string(),
            install_argv: Some(vec!["npm".into(), "install".into()]),
            install_skip_when_present: Some(PathBuf::from("node_modules")),
            run_argv: vec![
                "npm".into(),
                "run".into(),
                "dev".into(),
                "--".into(),
                "--host".into(),
                "127.0.0.1".into(),
                "--port".into(),
                port_s.clone(),
            ],
        });
    }

    // 2) / 3) Python via `pyproject.toml` (uv pour la gestion d’environnement)
    let pyproject = project_root.join("pyproject.toml");
    if pyproject.is_file() {
        let content = fs::read_to_string(&pyproject)
            .map(|s| s.to_lowercase())
            .unwrap_or_default();

        if pyproject_mentions(&content, "streamlit") {
            let entry = first_existing_relative(
                project_root,
                &["app.py", "streamlit_app.py", "main.py", "src/app.py"],
            )
            .unwrap_or_else(|| "app.py".to_string());
            return Ok(StudioPreviewPlan {
                label: "Python · uv · Streamlit".to_string(),
                install_argv: Some(vec!["uv".into(), "sync".into()]),
                install_skip_when_present: Some(PathBuf::from(".venv")),
                run_argv: vec![
                    "uv".into(),
                    "run".into(),
                    "streamlit".into(),
                    "run".into(),
                    entry,
                    "--server.port".into(),
                    port_s.clone(),
                    "--server.address".into(),
                    "127.0.0.1".into(),
                    "--server.headless".into(),
                    "true".into(),
                ],
            });
        }

        if pyproject_mentions(&content, "fastapi") {
            // Convention `main:app` ; sinon `app.main:app` si le module existe.
            let target = if project_root.join("app").join("main.py").is_file() {
                "app.main:app"
            } else {
                "main:app"
            };
            return Ok(StudioPreviewPlan {
                label: "Python · uv · FastAPI".to_string(),
                install_argv: Some(vec!["uv".into(), "sync".into()]),
                install_skip_when_present: Some(PathBuf::from(".venv")),
                run_argv: vec![
                    "uv".into(),
                    "run".into(),
                    "uvicorn".into(),
                    target.to_string(),
                    "--host".into(),
                    "127.0.0.1".into(),
                    "--port".into(),
                    port_s,
                    "--reload".into(),
                ],
            });
        }
    }

    Err(
        "preview_stack_unsupported: ajoutez un manifeste reconnu (package.json pour Node ; \
         pyproject.toml mentionnant streamlit ou fastapi pour Python via uv). \
         L’aperçu HTML statique reste disponible en ouvrant un fichier .html."
            .to_string(),
    )
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
    /// Résumé produit / intention à la création (préfixe agent + graine plan & DESIGN.md).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    project_summary: Option<String>,
    /// Ticket enforcement mode for Code Studio runs: off | soft | strict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ticket_enforcement_mode: Option<String>,
}

const MAX_TECH_STACK_CHARS: usize = 4000;
/// Upper bound on characters injected from `CODE_STUDIO_PLAN.md` into each Code Studio message.
const MAX_CODE_STUDIO_PLAN_INJECT_CHARS: usize = 4000;
const MAX_EVOLUTION_SUMMARY_CHARS: usize = 6000;
const MAX_POLICY_NOTES_CHARS: usize = 4000;
const MAX_PROJECT_SUMMARY_CHARS: usize = 6000;
const MAX_DESIGN_HINT_CHARS: usize = 4000;
const MAX_DESIGN_DOC_CHARS: usize = 12000;
const STUDIO_TICKETS_FILE: &str = ".akasha-studio-tickets.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StudioTicketAcceptanceCriterion {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argv: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StudioTicketEvidence {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StudioTicket {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub requested_by: String,
    pub assigned_agent: String,
    pub review_agent: String,
    /// Tickets du même projet qui doivent être en `done` avant de lancer celui-ci (union avec `depends_on_ticket_id` si présent).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on_ticket_ids: Vec<String>,
    /// Déprécié : premier prérequis seulement ; fusionné dans `depends_on_ticket_ids` au chargement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depends_on_ticket_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub related_task_id: Option<String>,
    #[serde(default)]
    pub acceptance_criteria: Vec<StudioTicketAcceptanceCriterion>,
    #[serde(default)]
    pub evidence: StudioTicketEvidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_notes: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub corrective_steps: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StudioTicketEvent {
    pub id: String,
    pub ticket_id: String,
    pub event_type: String,
    pub at: String,
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct StudioTicketsStore {
    #[serde(default)]
    tickets: Vec<StudioTicket>,
    #[serde(default)]
    events: Vec<StudioTicketEvent>,
}

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
        "[Stack projet — CONTRAINTE FORTE :\n\
- Respecter strictement cette stack pour fichiers, dépendances, commandes et recommandations.\n\
- Interdiction de migrer vers un autre écosystème/langage (ex. Python -> TypeScript) sans demande explicite de l’utilisateur dans ce tour.\n\
- Si une proposition hors stack est envisagée, la garder en option textuelle sans modifier les fichiers ni la section Stack de `CODE_STUDIO_PLAN.md`.\n\
- En cas de doute, conserver la stack existante et demander clarification plutôt que réécrire.\n\
Stack active :\n{t}\n]\n\n"
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

/// Résumé produit saisi à la création ( borne identique à `evolution_summary` ).
fn project_summary_prefix_from_meta(meta: &StudioMeta) -> Option<String> {
    let t = sanitize_for_prompt(meta.project_summary.as_deref()?, MAX_PROJECT_SUMMARY_CHARS);
    if t.is_empty() {
        return None;
    }
    Some(format!(
        "[Résumé produit — intention initiale (à conserver pour le plan et le périmètre ; mettre à jour le fichier si le produit change) :\n{t}\n]\n\n"
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

/// Résumé produit enregistré dans `.akasha-studio.json` à la création (ou via PATCH).
pub fn studio_project_summary_prefix(project_root: &Path) -> Option<String> {
    let meta = load_studio_meta(project_root)?;
    project_summary_prefix_from_meta(&meta)
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

fn studio_tickets_path(project_root: &Path) -> PathBuf {
    project_root.join(STUDIO_TICKETS_FILE)
}

fn load_studio_tickets_store(project_root: &Path) -> StudioTicketsStore {
    let p = studio_tickets_path(project_root);
    let Ok(s) = fs::read_to_string(p) else {
        return StudioTicketsStore::default();
    };
    let mut store: StudioTicketsStore = serde_json::from_str(&s).unwrap_or_default();
    for t in &mut store.tickets {
        normalize_ticket_dependencies(t);
    }
    store
}

/// Union des prérequis (`depends_on_ticket_ids` + ancien champ singleton).
pub fn ticket_dependency_ids(ticket: &StudioTicket) -> Vec<String> {
    use std::collections::BTreeSet;
    let mut set: BTreeSet<String> = BTreeSet::new();
    for x in &ticket.depends_on_ticket_ids {
        let x = x.trim();
        if !x.is_empty() {
            set.insert(x.to_string());
        }
    }
    if let Some(ref o) = ticket.depends_on_ticket_id {
        let o = o.trim();
        if !o.is_empty() {
            set.insert(o.to_string());
        }
    }
    set.into_iter().collect()
}

/// Fusionne les champs legacy / liste et met à jour `depends_on_ticket_id` comme alias du premier id (tri lexicographique).
pub fn normalize_ticket_dependencies(ticket: &mut StudioTicket) {
    let merged = ticket_dependency_ids(ticket);
    ticket.depends_on_ticket_ids = merged.clone();
    ticket.depends_on_ticket_id = merged.first().cloned();
}

/// Suit la chaîne des prérequis depuis `start_id` ; retourne `true` si `needle` est atteignable (cycle si needle est le ticket courant).
pub fn prerequisite_chain_reaches_ticket(project_root: &Path, start_id: &str, needle: &str) -> bool {
    let store = load_studio_tickets_store(project_root);
    let mut id_to_ticket: std::collections::HashMap<String, StudioTicket> =
        std::collections::HashMap::new();
    for t in store.tickets {
        id_to_ticket.insert(t.id.clone(), t);
    }
    let mut stack = vec![start_id.to_string()];
    let mut seen = std::collections::HashSet::<String>::new();
    while let Some(cur) = stack.pop() {
        if cur == needle {
            return true;
        }
        if !seen.insert(cur.clone()) {
            continue;
        }
        let Some(t) = id_to_ticket.get(&cur) else {
            continue;
        };
        for d in ticket_dependency_ids(t) {
            stack.push(d);
        }
    }
    false
}

/// `true` si ajouter des arêtes ticket → chaque id dans `new_dep_ids` créerait un cycle.
pub fn studio_ticket_deps_would_cycle(
    project_root: &Path,
    ticket_id: &str,
    new_dep_ids: &[String],
) -> bool {
    for dep in new_dep_ids {
        let dep = dep.trim();
        if dep.is_empty() || dep == ticket_id {
            return true;
        }
        if prerequisite_chain_reaches_ticket(project_root, dep, ticket_id) {
            return true;
        }
    }
    false
}

fn save_studio_tickets_store(project_root: &Path, store: &StudioTicketsStore) -> Result<(), String> {
    let p = studio_tickets_path(project_root);
    let j = serde_json::to_string_pretty(store).map_err(|e| e.to_string())?;
    fs::write(&p, j).map_err(|e| e.to_string())
}

pub fn studio_ticket_enforcement_mode(project_root: &Path) -> String {
    load_studio_meta(project_root)
        .and_then(|m| m.ticket_enforcement_mode)
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| matches!(s.as_str(), "off" | "soft" | "strict"))
        .unwrap_or_else(|| "off".to_string())
}

pub fn studio_list_tickets(project_root: &Path) -> Vec<StudioTicket> {
    let mut tickets = load_studio_tickets_store(project_root).tickets;
    tickets.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    tickets
}

pub fn studio_get_ticket(project_root: &Path, ticket_id: &str) -> Option<StudioTicket> {
    let id = ticket_id.trim();
    if id.is_empty() {
        return None;
    }
    load_studio_tickets_store(project_root)
        .tickets
        .into_iter()
        .find(|t| t.id == id)
}

/// `true` lorsque tous les prérequis existent et sont `done` (ou aucun prérequis).
pub fn studio_ticket_prerequisite_done(project_root: &Path, ticket: &StudioTicket) -> bool {
    let deps = ticket_dependency_ids(ticket);
    if deps.is_empty() {
        return true;
    }
    for dep_id in deps {
        let Some(dep) = studio_get_ticket(project_root, &dep_id) else {
            return false;
        };
        if dep.status != "done" {
            return false;
        }
    }
    true
}

pub fn studio_list_ticket_events(project_root: &Path, ticket_id: &str) -> Vec<StudioTicketEvent> {
    let id = ticket_id.trim();
    if id.is_empty() {
        return Vec::new();
    }
    let mut events: Vec<StudioTicketEvent> = load_studio_tickets_store(project_root)
        .events
        .into_iter()
        .filter(|e| e.ticket_id == id)
        .collect();
    events.sort_by(|a, b| a.at.cmp(&b.at));
    events
}

pub fn studio_upsert_ticket(project_root: &Path, mut ticket: StudioTicket) -> Result<(), String> {
    normalize_ticket_dependencies(&mut ticket);
    let mut store = load_studio_tickets_store(project_root);
    if let Some(idx) = store.tickets.iter().position(|t| t.id == ticket.id) {
        store.tickets[idx] = ticket;
    } else {
        store.tickets.push(ticket);
    }
    save_studio_tickets_store(project_root, &store)
}

pub fn studio_append_ticket_event(
    project_root: &Path,
    ticket_id: &str,
    event_type: &str,
    actor: &str,
    payload: Option<serde_json::Value>,
) -> Result<(), String> {
    let mut store = load_studio_tickets_store(project_root);
    store.events.push(StudioTicketEvent {
        id: uuid::Uuid::new_v4().to_string(),
        ticket_id: ticket_id.to_string(),
        event_type: event_type.to_string(),
        at: chrono::Utc::now().to_rfc3339(),
        actor: actor.to_string(),
        payload,
    });
    if store.events.len() > 5000 {
        let trim_from = store.events.len().saturating_sub(5000);
        if trim_from > 0 {
            store.events.drain(..trim_from);
        }
    }
    save_studio_tickets_store(project_root, &store)
}

pub fn studio_attach_task_to_ticket(
    project_root: &Path,
    ticket_id: &str,
    task_id: &str,
    actor: &str,
) -> Result<(), String> {
    let Some(mut ticket) = studio_get_ticket(project_root, ticket_id) else {
        return Err("ticket_not_found".to_string());
    };
    if ticket.status == "todo" {
        ticket.status = "in_progress".to_string();
    }
    ticket.related_task_id = Some(task_id.to_string());
    if !ticket.evidence.task_ids.iter().any(|t| t == task_id) {
        ticket.evidence.task_ids.push(task_id.to_string());
    }
    ticket.updated_at = chrono::Utc::now().to_rfc3339();
    studio_upsert_ticket(project_root, ticket.clone())?;
    studio_append_ticket_event(
        project_root,
        ticket_id,
        "ticket_execution_started",
        actor,
        Some(serde_json::json!({
            "task_id": task_id,
            "status": ticket.status,
        })),
    )?;
    Ok(())
}

pub fn studio_mark_ticket_ready_for_review(
    project_root: &Path,
    ticket_id: &str,
    task_id: &str,
    actor: &str,
) -> Result<(), String> {
    let Some(mut ticket) = studio_get_ticket(project_root, ticket_id) else {
        return Err("ticket_not_found".to_string());
    };
    if ticket.status == "in_progress" {
        ticket.status = "review".to_string();
    }
    ticket.related_task_id = Some(task_id.to_string());
    if !ticket.evidence.task_ids.iter().any(|t| t == task_id) {
        ticket.evidence.task_ids.push(task_id.to_string());
    }
    ticket.updated_at = chrono::Utc::now().to_rfc3339();
    studio_upsert_ticket(project_root, ticket.clone())?;
    studio_append_ticket_event(
        project_root,
        ticket_id,
        "ticket_ready_for_review",
        actor,
        Some(serde_json::json!({
            "task_id": task_id,
            "status": ticket.status,
        })),
    )?;
    Ok(())
}

/// Workspace roots permitted for `studio_*` ticket tools: must sit under `<data_dir>/studio-projects/`.
pub fn studio_ticket_tool_workspace_root(
    store_path: Option<&Path>,
    workspace_root: Option<&Path>,
) -> Option<PathBuf> {
    let sp = store_path?;
    let ws = workspace_root?;
    let data_dir = sp.parent()?;
    let base = studio_projects_base(data_dir);
    if ws.starts_with(&base) && ws.is_dir() {
        Some(ws.to_path_buf())
    } else {
        None
    }
}

/// Heuristic: user message looks like a code/feature evolution request (avoid greetings / bootstrap noise).
pub fn studio_user_message_suggests_evolution(message: &str) -> bool {
    let t = message.trim();
    if t.starts_with("[Bootstrap Kanban") {
        return false;
    }
    if t.len() < 16 {
        return false;
    }
    let lower = t.to_ascii_lowercase();
    if lower.len() < 48
        && (lower == "bonjour"
            || lower == "salut"
            || lower == "hello"
            || lower == "hi"
            || lower == "thanks"
            || lower == "merci"
            || lower == "ok"
            || lower == "okay")
    {
        return false;
    }
    const KW: &[&str] = &[
        "implement",
        "implément",
        "implémente",
        "feature",
        "bug",
        "fix",
        "corrige",
        "correct",
        "refactor",
        "refonte",
        " ajoute",
        "add ",
        "change",
        "chang",
        "modif",
        "modify",
        "update ",
        "mise à jour",
        "endpoint",
        "route ",
        "component",
        "composant",
        "build error",
        "erreur de build",
        "typescript",
        "eslint",
        "nouvelle fonctionnalité",
        "new feature",
        "patch",
        "hotfix",
        "évolution",
        "evolution",
        "enhancement",
        "régression",
        "regression",
        "fonctionnalité",
        "migration",
        "schema",
        "database",
        "api ",
    ];
    KW.iter().any(|k| lower.contains(k))
}

/// Auto-create a Kanban ticket from a chat message (linked to the next `/api/message` turn).
pub fn studio_create_evolution_ticket_from_chat(
    project_root: &Path,
    message: &str,
) -> Result<String, String> {
    let meta = load_studio_meta(project_root).ok_or_else(|| "studio_meta_missing".to_string())?;
    let raw_title = message.lines().next().unwrap_or(message).trim();
    let mut title = if raw_title.chars().count() > 200 {
        raw_title.chars().take(200).collect::<String>()
    } else {
        raw_title.to_string()
    };
    if title.is_empty() {
        title = "Évolution (chat)".to_string();
    }
    let new_ticket_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let ticket = StudioTicket {
        id: new_ticket_id.clone(),
        project_id: meta.id.clone(),
        title,
        description: message.trim().to_string(),
        status: "todo".to_string(),
        requested_by: "user_chat".to_string(),
        assigned_agent: "studio_fullstack".to_string(),
        review_agent: "studio_reviewer".to_string(),
        depends_on_ticket_ids: Vec::new(),
        depends_on_ticket_id: None,
        related_task_id: None,
        acceptance_criteria: Vec::new(),
        evidence: StudioTicketEvidence::default(),
        review_outcome: None,
        review_notes: None,
        corrective_steps: Vec::new(),
        created_at: now.clone(),
        updated_at: now,
    };
    studio_upsert_ticket(project_root, ticket.clone())?;
    let _ = studio_append_ticket_event(
        project_root,
        &ticket.id,
        "ticket_created",
        "system",
        Some(json!({
            "source": "chat_evolution_auto",
            "title": ticket.title,
        })),
    );
    Ok(new_ticket_id)
}

/// Agent tool: create ticket (JSON args). `assigned_agent` defaults to `studio_fullstack`; `review_agent` to `studio_reviewer`.
pub fn studio_tool_create_ticket_json(
    project_root: &Path,
    body: &serde_json::Value,
) -> Result<StudioTicket, String> {
    let meta = load_studio_meta(project_root).ok_or_else(|| "studio_meta_missing".to_string())?;
    let title = body
        .get("title")
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "title_required".to_string())?;
    if title.chars().count() > 200 {
        return Err("title_invalid".to_string());
    }
    let description = body
        .get("description")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let assigned_agent = body
        .get("assigned_agent")
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "studio_fullstack".to_string());
    let review_agent = body
        .get("review_agent")
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "studio_reviewer".to_string());
    let requested_by = body
        .get("requested_by")
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "studio_project_manager".to_string());
    let acceptance_criteria = match body.get("acceptance_criteria") {
        None => vec![],
        Some(v) => serde_json::from_value::<Vec<StudioTicketAcceptanceCriterion>>(v.clone())
            .map_err(|_| "acceptance_criteria_invalid".to_string())?,
    };

    let mut depends_on_ticket_ids: Vec<String> = Vec::new();
    if let Some(raw) = body.get("depends_on_ticket_ids") {
        if raw.is_null() {
            // empty
        } else if let Some(arr) = raw.as_array() {
            for v in arr {
                let Some(s) = v.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
                    return Err("depends_on_ticket_ids_must_be_string_array".to_string());
                };
                depends_on_ticket_ids.push(s.to_string());
            }
        } else {
            return Err("depends_on_ticket_ids_must_be_array_or_null".to_string());
        }
    }
    match body.get("depends_on_ticket_id") {
        None | Some(serde_json::Value::Null) => {}
        Some(v) => {
            let Some(s) = v.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
                return Err("depends_on_ticket_id_invalid".to_string());
            };
            if !depends_on_ticket_ids.iter().any(|x| x == s) {
                depends_on_ticket_ids.push(s.to_string());
            }
        }
    }
    depends_on_ticket_ids.sort();
    depends_on_ticket_ids.dedup();

    let new_ticket_id = uuid::Uuid::new_v4().to_string();
    for d in &depends_on_ticket_ids {
        if d == &new_ticket_id {
            return Err("depends_on_ticket_self".to_string());
        }
        if studio_get_ticket(project_root, d).is_none() {
            return Err("depends_on_ticket_not_found".to_string());
        }
    }
    if studio_ticket_deps_would_cycle(project_root, &new_ticket_id, &depends_on_ticket_ids) {
        return Err("depends_on_ticket_cycle".to_string());
    }
    let depends_on_ticket_id = depends_on_ticket_ids.first().cloned();

    let status = body
        .get("status")
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| matches!(s.as_str(), "todo" | "in_progress" | "review" | "done" | "blocked"))
        .unwrap_or_else(|| "todo".to_string());

    let now = chrono::Utc::now().to_rfc3339();
    let ticket = StudioTicket {
        id: new_ticket_id,
        project_id: meta.id.clone(),
        title: title.to_string(),
        description,
        status,
        requested_by,
        assigned_agent,
        review_agent,
        depends_on_ticket_ids,
        depends_on_ticket_id,
        related_task_id: None,
        acceptance_criteria,
        evidence: StudioTicketEvidence::default(),
        review_outcome: None,
        review_notes: None,
        corrective_steps: Vec::new(),
        created_at: now.clone(),
        updated_at: now,
    };
    studio_upsert_ticket(project_root, ticket.clone())?;
    let _ = studio_append_ticket_event(
        project_root,
        &ticket.id,
        "ticket_created",
        "agent",
        Some(json!({
            "assigned_agent": ticket.assigned_agent,
            "review_agent": ticket.review_agent,
            "depends_on_ticket_id": ticket.depends_on_ticket_id,
            "depends_on_ticket_ids": ticket.depends_on_ticket_ids,
            "source": "studio_create_ticket",
        })),
    );
    Ok(ticket)
}

/// Agent tool: PATCH ticket fields from JSON (`ticket_id` or `id` required). Same validation rules as HTTP PATCH.
pub fn studio_tool_apply_ticket_patch_json(
    project_root: &Path,
    body: &serde_json::Value,
) -> Result<StudioTicket, String> {
    let ticket_id = body
        .get("ticket_id")
        .or_else(|| body.get("id"))
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "ticket_id_required".to_string())?;
    let Some(mut ticket) = studio_get_ticket(project_root, ticket_id) else {
        return Err("ticket_not_found".to_string());
    };
    let body_v = body;
    if let Some(v) = body_v.get("title").and_then(|x| x.as_str()) {
        let t = v.trim();
        if t.is_empty() || t.chars().count() > 200 {
            return Err("title_invalid".to_string());
        }
        ticket.title = t.to_string();
    }
    if let Some(v) = body_v.get("description").and_then(|x| x.as_str()) {
        ticket.description = v.trim().to_string();
    }
    if let Some(v) = body_v.get("assigned_agent").and_then(|x| x.as_str()) {
        let a = v.trim();
        if a.is_empty() {
            return Err("assigned_agent_required".to_string());
        }
        ticket.assigned_agent = a.to_string();
    }
    if let Some(v) = body_v.get("review_agent").and_then(|x| x.as_str()) {
        let a = v.trim();
        if a.is_empty() {
            return Err("review_agent_required".to_string());
        }
        ticket.review_agent = a.to_string();
    }
    if body_v.get("depends_on_ticket_ids").is_some() {
        let next_deps: Vec<String> = match body_v.get("depends_on_ticket_ids") {
            Some(serde_json::Value::Null) => Vec::new(),
            Some(arr_v) => {
                let Some(arr) = arr_v.as_array() else {
                    return Err("depends_on_ticket_ids_must_be_array_or_null".to_string());
                };
                let mut out = Vec::new();
                for v in arr {
                    let Some(s) = v.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
                        return Err("depends_on_ticket_ids_must_be_string_array".to_string());
                    };
                    out.push(s.to_string());
                }
                out.sort();
                out.dedup();
                out
            }
            None => Vec::new(),
        };
        for d in &next_deps {
            if d == ticket_id {
                return Err("depends_on_ticket_self".to_string());
            }
            if studio_get_ticket(project_root, d).is_none() {
                return Err("depends_on_ticket_not_found".to_string());
            }
        }
        if studio_ticket_deps_would_cycle(project_root, ticket_id, &next_deps) {
            return Err("depends_on_ticket_cycle".to_string());
        }
        ticket.depends_on_ticket_ids = next_deps;
        ticket.depends_on_ticket_id = ticket.depends_on_ticket_ids.first().cloned();
    } else if body_v.get("depends_on_ticket_id").is_some() {
        let next_dep = match body_v.get("depends_on_ticket_id") {
            Some(serde_json::Value::Null) => None,
            Some(v) => {
                let Some(s) = v.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
                    return Err("depends_on_ticket_id_invalid".to_string());
                };
                let d = s.to_string();
                if d == ticket_id {
                    return Err("depends_on_ticket_self".to_string());
                }
                if studio_get_ticket(project_root, &d).is_none() {
                    return Err("depends_on_ticket_not_found".to_string());
                }
                Some(d)
            }
            None => None,
        };
        let next_deps: Vec<String> = match next_dep {
            None => Vec::new(),
            Some(d) => vec![d],
        };
        if studio_ticket_deps_would_cycle(project_root, ticket_id, &next_deps) {
            return Err("depends_on_ticket_cycle".to_string());
        }
        ticket.depends_on_ticket_ids = next_deps;
        ticket.depends_on_ticket_id = ticket.depends_on_ticket_ids.first().cloned();
    }
    if let Some(v) = body_v.get("acceptance_criteria") {
        match serde_json::from_value::<Vec<StudioTicketAcceptanceCriterion>>(v.clone()) {
            Ok(criteria) => ticket.acceptance_criteria = criteria,
            Err(_) => return Err("acceptance_criteria_invalid".to_string()),
        }
    }
    let mut recovered_execution: Option<(String, Option<String>)> = None;
    if body_v.get("recover_stuck_execution").and_then(|x| x.as_bool()) == Some(true) {
        if ticket.status != "in_progress" && ticket.status != "review" {
            return Err("recover_only_in_progress_or_review".to_string());
        }
        recovered_execution = Some((ticket.status.clone(), ticket.related_task_id.clone()));
        ticket.related_task_id = None;
        ticket.status = "todo".to_string();
    } else if let Some(v) = body_v.get("status").and_then(|x| x.as_str()) {
        let next = v.trim().to_ascii_lowercase();
        let valid = matches!(
            next.as_str(),
            "todo" | "in_progress" | "review" | "done" | "blocked"
        );
        if !valid {
            return Err("invalid_status".to_string());
        }
        let cur = ticket.status.as_str();
        let transition_ok = match (cur, next.as_str()) {
            ("todo", "in_progress") => !ticket.assigned_agent.trim().is_empty(),
            ("in_progress", "review") => true,
            ("review", "done") => false,
            ("review", "in_progress") => !ticket.corrective_steps.is_empty(),
            (_, "blocked") if cur != "done" => true,
            ("blocked", "in_progress") => true,
            (a, b) if a == b => true,
            _ => false,
        };
        if !transition_ok {
            return Err("invalid_status_transition".to_string());
        }
        ticket.status = next;
    }
    ticket.updated_at = chrono::Utc::now().to_rfc3339();
    studio_upsert_ticket(project_root, ticket.clone())?;
    match recovered_execution {
        Some((previous_status, previous_related_task_id)) => {
            let _ = studio_append_ticket_event(
                project_root,
                &ticket.id,
                "ticket_execution_recovered",
                "agent",
                Some(json!({
                    "status": ticket.status,
                    "previous_status": previous_status,
                    "previous_related_task_id": previous_related_task_id,
                })),
            );
        }
        None => {
            let _ = studio_append_ticket_event(
                project_root,
                &ticket.id,
                "ticket_updated",
                "agent",
                Some(json!({ "status": ticket.status })),
            );
        }
    }
    Ok(ticket)
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
        // Use file_type() (does not follow symlinks) to skip symlinked entries entirely.
        // Following symlinks could traverse outside the studio sandbox root.
        let ft = match e.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir()
            && EXCLUDED_DIR_NAMES
                .iter()
                .any(|d| name.eq_ignore_ascii_case(d))
        {
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
            | "uv"
            | "uv.exe"
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
        "uv" | "uv.exe" => {
            // `uv sync` (install deps) and `uv run <tool> ...` (start dev server / streamlit / uvicorn).
            matches!(subcommand, Some("sync") | Some("run"))
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

fn write_initial_code_studio_plan(
    project_root: &Path,
    name: &str,
    tech_stack: Option<&str>,
    project_summary: Option<&str>,
) -> Result<(), String> {
    let plan_path = project_root.join("CODE_STUDIO_PLAN.md");
    if plan_path.exists() {
        return Ok(());
    }
    let stack_body = tech_stack
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "(À compléter — stack enregistrée dans `.akasha-studio.json` ou déduite du manifeste.)".to_string());
    let description_body = project_summary
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "(Brève description du produit et de l’usage attendu.)".to_string());
    let body = format!(
        r#"# Titre : {name}

## Description

{description_body}

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
        description_body = description_body,
        stack_body = stack_body,
    );
    fs::write(&plan_path, body).map_err(|e| e.to_string())
}

fn yaml_double_quoted_scalar(s: &str) -> String {
    let t = s.trim();
    let t = if t.is_empty() { "App" } else { t };
    let mut out = String::new();
    for ch in t.chars().take(120) {
        match ch {
            '"' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            '\n' | '\r' => out.push(' '),
            _ => out.push(ch),
        }
    }
    out
}

/// Gabarit minimal `DESIGN.md` (front matter + sections) pour que l’agent et l’UI partent d’une base valide.
fn write_initial_design_md(
    project_root: &Path,
    name: &str,
    project_summary: Option<&str>,
) -> Result<(), String> {
    let path = project_root.join("DESIGN.md");
    if path.exists() {
        return Ok(());
    }
    let safe_name = yaml_double_quoted_scalar(name);
    let overview = match project_summary.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => format!(
            "{s}\n\n_(Résumé fourni à la création du projet — affiner avec le design et le code.)_"
        ),
        None => "(À compléter — identité visuelle, références, mood, public cible.)".to_string(),
    };
    let c_primary = "#6366F1";
    let c_surface = "#0F172A";
    let c_text = "#F8FAFC";
    let c_muted = "#94A3B8";
    let body = format!(
        r#"---
version: alpha
name: "{safe_name}"
description: "Initial design scaffold — refine with implementation."
colors:
  primary: "{c_primary}"
  surface: "{c_surface}"
  text: "{c_text}"
  muted: "{c_muted}"
typography:
  body-md:
    fontFamily: system-ui
    fontSize: 16px
    fontWeight: "400"
    lineHeight: "1.5"
  display-lg:
    fontFamily: system-ui
    fontSize: 32px
    fontWeight: "600"
    lineHeight: "1.2"
rounded:
  sm: "4px"
  md: "8px"
spacing:
  xs: "4px"
  sm: "8px"
components: {{}}

---

## Brand & Style

{overview}

## Colors

Palette de départ — à aligner sur la marque et le code livré.

## Typography

Hiérarchie de base — ajuster selon le produit.

## Layout & Spacing

Grille et gouttières — à définir (mobile d’abord recommandé).

## Elevation & depth

Ombres et profondeur — à préciser.

## Shapes

Rayons et formes (cartes, boutons).

## Components

_Principaux blocs UI — détailler en `###` au fil de l’implémentation._
"#,
        safe_name = safe_name,
        c_primary = c_primary,
        c_surface = c_surface,
        c_text = c_text,
        c_muted = c_muted,
        overview = overview,
    );
    fs::write(&path, body).map_err(|e| e.to_string())
}

/// Premier commit sur la branche courante si le dépôt n’en a pas encore (évite `HEAD` ambigu / detached sur certains clients).
pub(super) async fn ensure_studio_initial_commit(project_root: &Path) -> Result<(), String> {
    if !is_git_repo(project_root).await {
        return Ok(());
    }
    let head_ok = git_output(project_root, &["rev-parse", "--verify", "HEAD"])
        .await
        .map(|o| o.status.success())
        .unwrap_or(false);
    if head_ok {
        return Ok(());
    }
    let add = git_output(project_root, &["add", "-A"]).await?;
    if !add.status.success() {
        return Err(format!(
            "git add failed: {}",
            String::from_utf8_lossy(&add.stderr).trim()
        ));
    }
    let commit = git_output(
        project_root,
        &[
            "commit",
            "-m",
            "chore: initial Code Studio project",
            "--no-verify",
        ],
    )
    .await?;
    if !commit.status.success() {
        let msg = String::from_utf8_lossy(&commit.stderr).trim().to_string();
        let out = String::from_utf8_lossy(&commit.stdout).trim().to_string();
        if msg.contains("nothing to commit") || out.contains("nothing to commit") {
            return Ok(());
        }
        return Err(format!("git commit failed: {msg}"));
    }
    Ok(())
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

#[derive(Debug, Clone, Serialize)]
pub(super) struct StudioGitBranch {
    pub name: String,
    pub current: bool,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub last_commit_subject: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct StudioGitCompareCommit {
    pub hash: String,
    pub subject: String,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct StudioGitCompare {
    pub base: String,
    pub target: String,
    pub ahead: u32,
    pub behind: u32,
    pub files_changed: u32,
    pub insertions: u32,
    pub deletions: u32,
    pub commits: Vec<StudioGitCompareCommit>,
}

pub(super) async fn git_list_branches(project_root: &Path) -> Result<Vec<StudioGitBranch>, String> {
    let out = git_output(
        project_root,
        &["for-each-ref", "--format=%(refname:short)|%(upstream:short)|%(HEAD)", "refs/heads"],
    )
    .await?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let mut rows = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let parts: Vec<&str> = line.split('|').collect();
        if parts.is_empty() {
            continue;
        }
        let name = parts.first().map(|s| s.trim()).unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        let upstream = parts.get(1).map(|s| s.trim()).filter(|s| !s.is_empty()).map(ToString::to_string);
        let current = parts.get(2).map(|s| s.trim() == "*").unwrap_or(false);
        let mut ahead = 0u32;
        let mut behind = 0u32;
        if let Some(up) = upstream.as_deref() {
            let cmp = git_output(project_root, &["rev-list", "--left-right", "--count", &format!("{name}...{up}")]).await?;
            if cmp.status.success() {
                let txt = String::from_utf8_lossy(&cmp.stdout);
                let mut it = txt.split_whitespace();
                ahead = it.next().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
                behind = it.next().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
            }
        }
        let subject = git_output(project_root, &["log", "-1", "--pretty=%s", name]).await.ok().and_then(|o| {
            if o.status.success() {
                let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if s.is_empty() { None } else { Some(s) }
            } else {
                None
            }
        });
        rows.push(StudioGitBranch {
            name: name.to_string(),
            current,
            upstream,
            ahead,
            behind,
            last_commit_subject: subject,
        });
    }
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(rows)
}

pub(super) async fn git_checkout_branch(project_root: &Path, branch: &str) -> Result<(), String> {
    let out = git_output(project_root, &["checkout", branch]).await?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(())
}

pub(super) async fn git_compare_branches(
    project_root: &Path,
    base: &str,
    target: &str,
) -> Result<StudioGitCompare, String> {
    let ahead_behind = git_output(project_root, &["rev-list", "--left-right", "--count", &format!("{base}...{target}")]).await?;
    if !ahead_behind.status.success() {
        return Err(String::from_utf8_lossy(&ahead_behind.stderr).trim().to_string());
    }
    let ab = String::from_utf8_lossy(&ahead_behind.stdout);
    let mut ab_it = ab.split_whitespace();
    let behind = ab_it.next().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
    let ahead = ab_it.next().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);

    let stat = git_output(project_root, &["diff", "--shortstat", &format!("{base}...{target}")]).await?;
    let mut files_changed = 0u32;
    let mut insertions = 0u32;
    let mut deletions = 0u32;
    if stat.status.success() {
        let txt = String::from_utf8_lossy(&stat.stdout);
        for seg in txt.split(',') {
            let part = seg.trim();
            if part.contains("file changed") || part.contains("files changed") {
                files_changed = part.split_whitespace().next().and_then(|s| s.parse().ok()).unwrap_or(0);
            } else if part.contains("insertion") {
                insertions = part.split_whitespace().next().and_then(|s| s.parse().ok()).unwrap_or(0);
            } else if part.contains("deletion") {
                deletions = part.split_whitespace().next().and_then(|s| s.parse().ok()).unwrap_or(0);
            }
        }
    }

    let commits_out = git_output(project_root, &["log", "--pretty=%H|%s", "--max-count=50", &format!("{base}..{target}")]).await?;
    let mut commits = Vec::new();
    if commits_out.status.success() {
        for line in String::from_utf8_lossy(&commits_out.stdout).lines() {
            let mut parts = line.splitn(2, '|');
            let hash = parts.next().unwrap_or("").trim().to_string();
            let subject = parts.next().unwrap_or("").trim().to_string();
            if !hash.is_empty() {
                commits.push(StudioGitCompareCommit { hash, subject });
            }
        }
    }

    Ok(StudioGitCompare {
        base: base.to_string(),
        target: target.to_string(),
        ahead,
        behind,
        files_changed,
        insertions,
        deletions,
        commits,
    })
}

pub(super) async fn git_conflict_files(project_root: &Path) -> Result<Vec<String>, String> {
    let out = git_output(project_root, &["diff", "--name-only", "--diff-filter=U"]).await?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect())
}

pub(super) async fn git_merge_abort(project_root: &Path) -> Result<(), String> {
    let out = git_output(project_root, &["merge", "--abort"]).await?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(())
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

/// Règles anti-suppression : worktree non propre, ou commits non poussés (branche de suivi connue).
#[derive(Debug, Clone, Serialize)]
pub struct StudioDeletePrecheck {
    pub has_git: bool,
    pub worktree_dirty: bool,
    pub has_upstream: bool,
    /// Nombre de commits locaux en avance sur `@{u}` ; `None` si pas de suivi amont.
    pub commits_ahead_of_upstream: Option<u32>,
    /// `true` → le client doit envoyer `force` (ou l’utilisateur confirmer explicitement).
    pub requires_force: bool,
    /// Dépôt Git avec historique local mais sans `@{u}` (impossible de vérifier le push).
    pub note_no_upstream: bool,
}

/// Analyse Git avant suppression (avertir commit / push).
pub async fn studio_delete_precheck(project_root: &Path) -> StudioDeletePrecheck {
    if !is_git_repo(project_root).await {
        return StudioDeletePrecheck {
            has_git: false,
            worktree_dirty: false,
            has_upstream: false,
            commits_ahead_of_upstream: None,
            requires_force: false,
            note_no_upstream: false,
        };
    }
    let worktree_dirty = git_has_pending_changes(project_root).await.unwrap_or(false);
    let has_upstream = git_output(project_root, &["rev-parse", "@{u}"])
        .await
        .map(|o| o.status.success())
        .unwrap_or(false);
    let commits_ahead_of_upstream = if has_upstream {
        match git_output(project_root, &["rev-list", "--count", "@{u}..HEAD"]).await {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().parse().ok(),
            _ => None,
        }
    } else {
        None
    };
    let ahead = commits_ahead_of_upstream.unwrap_or(0);
    let requires_force = worktree_dirty || (has_upstream && ahead > 0);
    let note_no_upstream = !has_upstream
        && git_rev_count(project_root, &["HEAD"])
            .await
            .unwrap_or(0)
            > 0;
    StudioDeletePrecheck {
        has_git: true,
        worktree_dirty,
        has_upstream,
        commits_ahead_of_upstream,
        requires_force,
        note_no_upstream,
    }
}

async fn git_rev_count(project_root: &Path, rev: &[&str]) -> Option<u32> {
    let mut args: Vec<&str> = vec!["rev-list", "--count"];
    args.extend_from_slice(rev);
    let o = git_output(project_root, &args).await.ok()?;
    if !o.status.success() {
        return None;
    }
    String::from_utf8_lossy(&o.stdout).trim().parse().ok()
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
    use super::{ensure_main_or_master_branch, git_list_branches, studio_command_from_argv};
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

    #[tokio::test]
    async fn git_list_branches_marks_current_branch() {
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
        let dir = std::env::temp_dir().join(format!("akasha_studio_branches_{stamp}_{}", std::process::id()));
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
            .args(["checkout", "-b", "studio/feature-a"])
            .current_dir(&dir)
            .status()
            .unwrap()
            .success());

        let branches = git_list_branches(&dir).await.expect("list branches");
        assert!(branches.iter().any(|b| b.name == "studio/feature-a" && b.current));
        assert!(branches.iter().any(|b| b.name == "master" || b.name == "main"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn evolution_heuristic_skips_bootstrap_and_greetings() {
        use super::studio_user_message_suggests_evolution;
        assert!(!studio_user_message_suggests_evolution(
            "[Bootstrap Kanban — exécution unique] x"
        ));
        assert!(!studio_user_message_suggests_evolution("bonjour"));
        assert!(!studio_user_message_suggests_evolution("merci"));
        assert!(studio_user_message_suggests_evolution(
            "Ajoute une route API /health pour le monitoring du service."
        ));
        assert!(studio_user_message_suggests_evolution(
            "Please fix the TypeScript error in src/App.tsx when building."
        ));
    }
}
