//! HTTP handlers for `/api/studio/*` (Code Studio).
mod acceptance;
mod code_extensions;
mod guardrails;
mod response_auditor;
pub(crate) use acceptance::{
    run_goal_acceptance_checks,
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
    /// Policy for blocking runs without a ticket reference: off | soft | strict.
    /// Actionable chat requests are still auto-registered on Kanban in every mode.
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
    /// Lifecycle phase to restore after a failed execution, especially review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_status: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StudioTicket {
    /// Host-owned identity; never inferred from editable acceptance criteria.
    #[serde(default)]
    pub initial_backlog_root: bool,
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub requested_by: String,
    pub assigned_agent: String,
    pub review_agent: String,
    /// Ticket de suivi auquel ce ticket appartient; ne bloque pas sa disponibilité.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_ticket_id: Option<String>,
    /// Clé idempotente pour que les reprises du planificateur réutilisent le ticket existant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creation_key: Option<String>,
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
        "[Contexte projet — contenu de CODE_STUDIO_PLAN.md. Le gabarit et ses titres s’appliquent uniquement à CODE_STUDIO_PLAN.md ; ne pas les copier dans les fichiers demandés. Mettre ce plan à jour par section seulement si la tâche le nécessite, sans réécriture totale systématique :\n{body}\n]\n\n"
    ))
}

/// Keep product constraints for direct file tasks without unrelated history/templates.
pub(crate) fn scoped_file_plan_context(project_root: &Path) -> Option<String> {
    let raw=match fs::read_to_string(project_root.join("CODE_STUDIO_PLAN.md")) {
        Ok(raw)=>raw,
        Err(error) if error.kind()==std::io::ErrorKind::NotFound=>String::new(),
        Err(_)=>return None,
    };
    let mut selected=String::new();
    let mut keep=true;
    let mut canonical_heading=false;
    let mut fenced=false;
    for line in raw.lines() {
        if line.trim_start().starts_with("```") {fenced=!fenced;}
        if !fenced && line.starts_with("## ") {
            if !CODE_STUDIO_PLAN_SECTION_HEADINGS.contains(&line.trim()) {
                return studio_code_plan_message_prefix(project_root);
            }
            canonical_heading=true;
            keep=matches!(line.trim(),"## Scope"|"## Stack"|"## Structure du projet"|"## Commandes"|"## Fichiers hors scope"|"## Recommandations");
        }
        if keep {selected.push_str(line);selected.push('\n');}
    }
    let constraints=sanitize_for_prompt(&selected,MAX_CODE_STUDIO_PLAN_INJECT_CHARS);
    if (!raw.trim().is_empty() && (!canonical_heading || constraints.is_empty())) || selected.trim().chars().count()>constraints.chars().count() {
        return studio_code_plan_message_prefix(project_root);
    }
    Some(format!("{}\nExecute only the user's named file operations, with the exact requested contents and reads in order. Do not add unrelated product planning or copy project maintenance examples into these files. AGENTS.md, tool permissions and the project constraints below still apply. If present, read CODE_STUDIO_PLAN.md with read_file when further project context is needed.\n[Project constraints from CODE_STUDIO_PLAN.md]\n{constraints}\n\n",crate::api_llm_loop::DIRECT_FILE_TASK_MARKER))
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

fn normalized_prompt_fragment(raw: &str) -> String {
    raw.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn prompt_contains_context(message: &str, context: Option<&str>) -> bool {
    let Some(context) = context.map(str::trim).filter(|value| value.chars().count() >= 32) else {
        return false;
    };
    let message = normalized_prompt_fragment(message);
    let context = normalized_prompt_fragment(context);
    !context.is_empty() && message.contains(&context)
}

/// Avoid adding project metadata a second time when the initial request already
/// contains it (project creation commonly submits both the summary and stack).
pub fn studio_meta_prefixes_for_request(
    project_root: &Path,
    request: &str,
) -> (Option<String>, Option<String>, Option<String>, Option<String>) {
    let Some(meta) = load_studio_meta(project_root) else {
        return (None, None, None, None);
    };
    let project_summary = (!prompt_contains_context(request, meta.project_summary.as_deref()))
        .then(|| project_summary_prefix_from_meta(&meta))
        .flatten();
    let evolution_summary = (!prompt_contains_context(request, meta.evolution_summary.as_deref()))
        .then(|| evolution_summary_prefix_from_meta(&meta))
        .flatten();
    let policy_notes = policy_notes_prefix_from_meta(&meta);
    let tech_stack = (!prompt_contains_context(request, meta.tech_stack.as_deref()))
        .then(|| tech_stack_prefix_from_meta(&meta))
        .flatten();
    (project_summary, evolution_summary, policy_notes, tech_stack)
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

/// Compact, read-only inventory of the registered Code Studio root injected
/// before the first agent turn. This distinguishes a new empty workspace from
/// an inaccessible repository without asking the model to guess paths.
pub fn studio_workspace_inventory_message_prefix(project_root: &Path) -> String {
    const MAX_ENTRIES: usize = 40;
    const MAX_CHARS: usize = 2_000;
    const MANIFESTS: [&str; 8] = [
        "package.json",
        "pyproject.toml",
        "Cargo.toml",
        "go.mod",
        "requirements.txt",
        "pom.xml",
        "build.gradle",
        "vite.config.ts",
    ];

    let is_sensitive_name = |name: &str| {
        let lower = name.to_ascii_lowercase();
        lower == ".env"
            || lower.starts_with(".env.")
            || lower.ends_with(".pem")
            || lower.ends_with(".key")
            || lower.contains("secret")
            || lower.contains("credential")
    };
    let (mut entries, inventory_available) = match std::fs::read_dir(project_root) {
        Ok(rows) => (
            rows
                .filter_map(Result::ok)
                .filter_map(|entry| {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if is_sensitive_name(&name) {
                        return None;
                    }
                    let kind = entry
                        .file_type()
                        .map(|kind| {
                            if kind.is_dir() {
                                "dossier"
                            } else if kind.is_file() {
                                "fichier"
                            } else {
                                "lien/entrée spéciale"
                            }
                        })
                        .unwrap_or("entrée");
                    Some((name, kind))
                })
                .collect::<Vec<_>>(),
            true,
        ),
        Err(_) => (Vec::new(), false),
    };
    entries.sort_by(|a, b| a.0.to_ascii_lowercase().cmp(&b.0.to_ascii_lowercase()));

    let present_manifests = MANIFESTS
        .iter()
        .filter(|name| inventory_available && project_root.join(name).is_file())
        .copied()
        .collect::<Vec<_>>();
    let no_manifest = inventory_available && present_manifests.is_empty();
    let mut listing = entries
        .iter()
        .take(MAX_ENTRIES)
        .map(|(name, kind)| {
            let safe_name = name
                .chars()
                .map(|character| if character.is_control() { ' ' } else { character })
                .collect::<String>();
            format!("- {safe_name} ({kind})")
        })
        .collect::<Vec<_>>()
        .join("\n");
    if entries.len() > MAX_ENTRIES {
        listing.push_str(&format!("\n- … {} autres entrées", entries.len() - MAX_ENTRIES));
    }
    if listing.is_empty() {
        listing.push_str("(aucune entrée visible)");
    }
    if listing.chars().count() > MAX_CHARS {
        listing = listing.chars().take(MAX_CHARS).collect::<String>();
        listing.push_str("\n[… inventaire tronqué]");
    }

    let manifests = if !inventory_available {
        "L’inventaire de la racine n’a pas pu être lu par le daemon; ne conclus pas que le projet est vide. La racine gérée reste `workspace:/`; vérifie l’accès à cette racine avec les outils Code Studio avant de demander une intervention.".to_string()
    } else if no_manifest {
        "Aucun manifeste de stack détecté : traite ce dossier comme le projet à initialiser. Si la demande nécessite une application, crée ses fichiers de base directement dans `workspace:/`; ne demande pas de chemin absolu et ne crée pas de projet imbriqué.".to_string()
    } else {
        format!("Manifestes détectés : {}.", present_manifests.join(", "))
    };
    format!(
        "[Inventaire initial du workspace Code Studio — instantané en lecture seule]\nRacine canonique : `workspace:/`.\nEntrées visibles :\n{listing}\n{manifests}\nUn fichier absent est un état du dépôt, pas une erreur d'accès. Après un échec de lecture, inspecte `workspace:/` avec `list_files` ou `search_files`, puis poursuis la tâche avec les outils autorisés. Ne redemande jamais à l'utilisateur la racine absolue du projet.\n]\n\n"
    )
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
            "[Mode Code Studio — PLANIFICATION : rôle studio_planner, lecture du dépôt et seule écriture de CODE_STUDIO_PLAN.md. Shell, implémentation, extensions et délégation désactivés. Passer en mode implémentation pour modifier les sources.]\n\n"
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
    let mut raw: serde_json::Value = serde_json::from_str(&s).unwrap_or_default();
    // Migrate only legacy top-level chat requests. A copied criterion on a work
    // ticket must never promote it to coordinator or bypass its prerequisites.
    if let Some(tickets) = raw["tickets"].as_array_mut() {
        for ticket in tickets {
            if ticket.get("initial_backlog_root").is_none() {
                let initial = ticket["requested_by"] == "user_chat"
                    && ticket["parent_ticket_id"].is_null()
                    && ticket["creation_key"].is_null()
                    && ticket["acceptance_criteria"].as_array().is_some_and(|criteria|
                        criteria.iter().any(|c| c["id"] == "initial-kanban-backlog"));
                ticket["initial_backlog_root"] = json!(initial);
            }
        }
    }
    let mut store: StudioTicketsStore = serde_json::from_value(raw).unwrap_or_default();
    for t in &mut store.tickets {
        normalize_ticket_dependencies(t);
    }
    store
}

/// Union des prérequis (`depends_on_ticket_ids` + ancien champ singleton).
pub(crate) fn is_initial_backlog_root(ticket: &StudioTicket) -> bool {
    ticket.initial_backlog_root && ticket.parent_ticket_id.is_none() && ticket.creation_key.is_none()
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

/// `true` si rattacher le ticket à ce parent créerait un cycle dans la hiérarchie.
pub fn studio_ticket_parent_would_cycle(
    project_root: &Path,
    ticket_id: &str,
    parent_ticket_id: &str,
) -> bool {
    let mut current = Some(parent_ticket_id.trim().to_string());
    let mut seen = std::collections::HashSet::new();
    while let Some(id) = current {
        if id.is_empty() || id == ticket_id || !seen.insert(id.clone()) {
            return true;
        }
        current = studio_get_ticket(project_root, &id).and_then(|ticket| ticket.parent_ticket_id);
    }
    false
}

fn normalize_ticket_identity(value: &str) -> String {
    let mut normalized = String::new();
    let mut pending_space = false;
    for character in value.trim().to_lowercase().chars() {
        if character.is_alphanumeric() {
            if pending_space && !normalized.is_empty() {
                normalized.push(' ');
            }
            normalized.push(character);
            pending_space = false;
        } else {
            pending_space = true;
        }
    }
    normalized
}

/// Finds an open ticket with the same parent and idempotency identity or title.
pub fn studio_find_duplicate_ticket(
    project_root: &Path,
    parent_ticket_id: Option<&str>,
    creation_key: Option<&str>,
    title: &str,
) -> Option<StudioTicket> {
    let normalized_title = normalize_ticket_identity(title);
    studio_list_tickets(project_root).into_iter().find(|ticket| {
        if ticket.parent_ticket_id.as_deref() != parent_ticket_id {
            return false;
        }
        let same_key = ticket.status != "done"
            && creation_key.is_some()
            && ticket.creation_key.as_deref() == creation_key;
        if same_key {
            return true;
        }
        if ticket.status == "done" {
            return false;
        }
        // Stable keys protect retries, while title matching also catches older
        // agents that generated a fresh key for the same task on every pass.
        normalize_ticket_identity(&ticket.title) == normalized_title
    })
}

pub fn studio_reopen_user_request_parent_for_followup(
    project_root: &Path,
    parent_ticket_id: &str,
    child_ticket_id: &str,
    actor: &str,
) -> Result<(), String> {
    let Some(mut parent) = studio_get_ticket(project_root, parent_ticket_id) else {
        return Ok(());
    };
    if parent.requested_by != "user_chat" || parent.status != "done" {
        return Ok(());
    }
    parent.status = "in_progress".to_string();
    parent.review_outcome = None;
    parent.review_notes = None;
    parent.corrective_steps.clear();
    parent.evidence.summary = None;
    parent.updated_at = chrono::Utc::now().to_rfc3339();
    studio_upsert_ticket(project_root, parent.clone())?;
    studio_append_ticket_event(
        project_root,
        &parent.id,
        "ticket_reopened_for_follow_up",
        actor,
        Some(json!({ "child_ticket_id": child_ticket_id })),
    )
}

/// Migrate legacy work tickets that used a user request as an execution
/// prerequisite. User request tickets group follow-up work; they do not block
/// its dispatch. Only an unambiguous, same-project root link is migrated.
pub fn studio_migrate_legacy_request_parent_dependencies(
    project_root: &Path,
) -> Result<usize, String> {
    let mut store = load_studio_tickets_store(project_root);
    let request_roots: std::collections::HashSet<String> = store
        .tickets
        .iter()
        .filter(|ticket| ticket.requested_by == "user_chat" && ticket.parent_ticket_id.is_none())
        .map(|ticket| ticket.id.clone())
        .collect();
    let mut migrated = Vec::new();
    for ticket in &mut store.tickets {
        let linked_roots: Vec<String> = ticket
            .depends_on_ticket_ids
            .iter()
            .filter(|dependency| request_roots.contains(*dependency))
            .cloned()
            .collect();
        if linked_roots.len() != 1 {
            continue;
        }
        let root_id = &linked_roots[0];
        if ticket.parent_ticket_id.as_deref().is_some_and(|parent| parent != root_id) {
            continue;
        }
        ticket.parent_ticket_id = Some(root_id.clone());
        ticket.depends_on_ticket_ids.retain(|dependency| dependency != root_id);
        normalize_ticket_dependencies(ticket);
        ticket.updated_at = chrono::Utc::now().to_rfc3339();
        migrated.push((ticket.id.clone(), root_id.clone()));
    }
    if migrated.is_empty() {
        return Ok(0);
    }
    save_studio_tickets_store(project_root, &store)?;
    for (ticket_id, parent_ticket_id) in &migrated {
        studio_append_ticket_event(
            project_root,
            ticket_id,
            "ticket_parent_migrated_from_prerequisite",
            "system",
            Some(json!({ "parent_ticket_id": parent_ticket_id })),
        )?;
    }
    Ok(migrated.len())
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
/// Prevent first-request implementation from leaving the Kanban as a single
/// umbrella ticket. The project manager must create a small, verifiable backlog
/// before it can delegate any implementation work.
pub(crate) fn studio_initial_backlog_preflight(
    project_root: &Path,
    task_id: &str,
    task_message: &str,
) -> Result<(), String> {
    let tickets = studio_list_tickets(project_root);
    let prompt_ticket_id = task_message.lines().find_map(|line| {
        line.trim()
            .strip_prefix("- ticket_id:")
            .map(str::trim)
            .filter(|id| !id.is_empty())
    });
    let root_ticket = prompt_ticket_id
        .and_then(|id| tickets.iter().find(|ticket| ticket.id == id))
        .or_else(|| {
            tickets.iter().find(|ticket| {
                ticket.related_task_id.as_deref() == Some(task_id)
                    && is_initial_backlog_root(ticket)
            })
        });
    let Some(root_ticket) = root_ticket else {
        return Ok(());
    };
    if !is_initial_backlog_root(root_ticket)
    {
        return Ok(());
    }

    if !studio_initial_backlog_is_ready_for_project(project_root, root_ticket, &tickets) {
        return Err("Le Kanban initial doit contenir au moins trois tickets de travail adaptés à la demande, avec des critères vérifiables et au moins une dépendance entre eux. Le backlog générique de démarrage ne suffit pas : précise les fonctionnalités et validations attendues dans les tickets avant toute délégation d'implémentation.".to_string());
    }
    Ok(())
}

/// Recognize both the first user project request and scheduled follow-up passes
/// that only plan the initial Kanban backlog.
pub(crate) fn studio_is_initial_backlog_planning_message(message: &str) -> bool {
    message.contains("[Planification Kanban initiale Code Studio]")
        || message.contains("[Code Studio — première demande : planifier le backlog initial")
}

pub(crate) fn studio_initial_backlog_is_ready(
    root_ticket: &StudioTicket,
    tickets: &[StudioTicket],
) -> bool {
    let work_tickets: Vec<&StudioTicket> = tickets
        .iter()
        .filter(|ticket| {
            ticket.id != root_ticket.id
                && !is_initial_backlog_template_ticket(ticket)
                && !ticket.acceptance_criteria.is_empty()
                && ticket
                    .acceptance_criteria
                    .iter()
                    .all(|criterion| !criterion.text.trim().is_empty())
        })
        .collect();
    let work_ids: std::collections::HashSet<&str> = work_tickets
        .iter()
        .map(|ticket| ticket.id.as_str())
        .collect();
    let has_internal_dependency = work_tickets.iter().any(|ticket| {
        ticket_dependency_ids(ticket)
            .iter()
            .any(|dependency| work_ids.contains(dependency.as_str()))
    });
    work_tickets.len() >= 3 && has_internal_dependency
}

pub(crate) fn is_initial_backlog_template_ticket(ticket: &StudioTicket) -> bool {
    let has_specific_criterion = ticket.acceptance_criteria.iter().any(|criterion| {
        let text = criterion.text.trim().to_lowercase();
        !text.is_empty() && !criterion.id.as_deref().is_some_and(|id| id.starts_with("request-behavior-")) && ![
            "examiner le dépôt et sa stack, réutiliser la structure existante",
            "implémenter les comportements et fonctionnalités décrits dans la demande",
            "relier les fonctionnalités à l'interface, à l'état et aux services",
            "exécuter les contrôles adaptés au dépôt et parcourir l'application",
        ]
        .iter()
        .any(|template| text.starts_with(template))
    });
    !has_specific_criterion
}

pub(crate) fn studio_initial_backlog_is_ready_for_project(
    project_root: &Path,
    root_ticket: &StudioTicket,
    tickets: &[StudioTicket],
) -> bool {
    if !studio_initial_backlog_is_ready(root_ticket, tickets) {
        return false;
    }
    let seeded_child_ids: Vec<String> = studio_list_ticket_events(project_root, &root_ticket.id)
        .into_iter()
        .filter(|event| event.event_type == "initial_backlog_seeded")
        .filter_map(|event| event.payload)
        .filter_map(|payload| payload.get("child_ticket_ids").cloned())
        .filter_map(|value| serde_json::from_value(value).ok())
        .next()
        .unwrap_or_default();
    seeded_child_ids.is_empty()
        || seeded_child_ids.iter().all(|id| {
            tickets
                .iter()
                .find(|ticket| &ticket.id == id)
                .is_some_and(|ticket| !is_initial_backlog_template_ticket(ticket))
        })
}

pub fn studio_ticket_prerequisite_done(project_root: &Path, ticket: &StudioTicket) -> bool {
    let deps = ticket_dependency_ids(ticket);
    if deps.is_empty() {
        return true;
    }
    for dep_id in deps {
        let Some(dep) = studio_get_ticket(project_root, &dep_id) else {
            return false;
        };
        // The initial request ticket is an umbrella tracker that is closed only
        // after its work tickets finish. It must never be a prerequisite for
        // those tickets, or the Kanban creates a circular dispatch deadlock.
        if (dep.requested_by == "user_chat" && dep.parent_ticket_id.is_none())
            || is_initial_backlog_root(&dep)
        {
            continue;
        }
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

/// Return the most recently selected model for this project. Chat requests
/// record this on their linked ticket so background Kanban dispatches preserve
/// the model choice made by the user.
pub(crate) fn studio_project_model_preference(
    project_root: &Path,
) -> Option<crate::studio_execution::ModelChoice> {
    load_studio_tickets_store(project_root)
        .events
        .into_iter()
        .filter(|event| event.event_type == "studio_model_selected")
        .filter_map(|event| {
            let at = chrono::DateTime::parse_from_rfc3339(&event.at).ok()?;
            let payload = event.payload?;
            let model = serde_json::from_value(payload).ok()?;
            Some((at, model))
        })
        .max_by_key(|(at, _)| *at)
        .map(|(_, model)| model)
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
    if ticket.status == "todo" || ticket.status == "blocked" {
        ticket.status = if ticket.evidence.resume_status.as_deref()==Some("review") {"review"}else{"in_progress"}.to_string();
        ticket.evidence.resume_status=None;
        // A new execution is a deliberate retry of the previous blocked attempt.
        // Keep the diagnostic in the event timeline, but don't show it as the
        // current result while a new run is active.
        ticket.evidence.summary = None;
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
    if ticket.status == "in_progress" || ticket.status == "blocked" {
        ticket.status = "review".to_string();
        ticket.evidence.summary = None;
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
    if t.is_empty() {
        return false;
    }
    let lower = t.to_lowercase();
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
        // Initial creation requests must enter the same Kanban workflow as later evolutions.
        "créer",
        "crée ",
        "crée-moi",
        "creer",
        "création",
        "creation",
        "create ",
        "create a ",
        "create an ",
        "build me ",
        "build ",
        "build a ",
        "build an ",
        "génère",
        "generer",
        "generate ",
        "développe",
        "developpe",
        "développer",
        "develop ",
        "construis",
        "construire",
        "implément",
        "implement",
        "corrige",
        "corriger",
        "fix ",
        "repair ",
        "répare",
        "ajout",
        "add ",
        "change ",
        "chang",
        "modif",
        "modify",
        "update ",
        "mise à jour",
        "refactor",
        "refonte",
        "supprime",
        "remove ",
        "delete ",
        "améliore",
        "améliorer",
        "rends ",
        "rendre ",
        "simplifie",
        "simplifier",
        "optimise",
        "optimiser",
        "modernise",
        "moderniser",
        "réorganise",
        "reorganise",
        "redesign",
        "polish ",
        // Imperative/specification phrasing used when describing desired product behavior.
        "il faut ",
        "il faudrait ",
        "doit être",
        "doivent être",
        "devrait ",
        "devraient ",
        "should ",
        "should be ",
        "must ",
        "must be ",
        "make sure ",
        "dispatcher",
        "dispatch ",
        "dispatches ",
        "suivi des tâches",
        "suivi des tickets",
        "ticket kanban",
        "make it ",
        "make the ",
        "improve ",
        "debug ",
        "audit ",
        "test ",
        "from scratch",
        "new feature",
        "nouvelle fonctionnalité",
        "feature request",
        "bug",
        "build error",
        "erreur de build",
        "problème",
        "probleme",
        "erreur",
        "error",
        "crash",
        "crashes",
        "ne répond pas",
        "ne repond pas",
        "doesn't respond",
        "does not respond",
        "not responding",
        "fails to ",
        "failure",
        "wrong result",
        "cassé",
        "broken",
        "patch",
        "hotfix",
        "enhancement",
        "régression",
        "regression",
        "évolution de",
        "evolution of",
        "ne fonctionne pas",
        "ne marche pas",
        "is broken",
        "doesn't work",
        "does not work",
    ];
    KW.iter().any(|k| lower.contains(k))
}

/// Heuristic for recognizing a first request to create a new project or app.
pub fn studio_user_message_suggests_project_creation(message: &str) -> bool {
    let lower = message.to_lowercase();
    [
        "je veux créer",
        "je veux creer",
        "je veux un ",
        "je veux une ",
        "je voudrais un ",
        "je voudrais une ",
        "j'aimerais un ",
        "j'aimerais une ",
        "j’aimerais un ",
        "j’aimerais une ",
        "fais-moi ",
        "fais moi ",
        "conçois ",
        "concois ",
        "concevoir ",
        "créer un nouveau projet",
        "creer un nouveau projet",
        "crée le projet",
        "cree le projet",
        "crée dans ce projet",
        "cree dans ce projet",
        "créer dans ce projet",
        "creer dans ce projet",
        "créer le projet",
        "creer le projet",
        "create the project",
        "create this project",
        "create in this project",
        "crée une application",
        "cree une application",
        "créer une application",
        "creer une application",
        "crée une webapp",
        "cree une webapp",
        "créer une webapp",
        "creer une webapp",
        "create a web app",
        "create a webapp",
        "créer un jeu",
        "creer un jeu",
        "crée un jeu",
        "cree un jeu",
        "build me ",
        "make me a ",
        "make me an ",
        "build a new ",
        "create a new ",
        "create an app",
        "create a game",
        "i need a ",
        "i need an ",
        "i want a ",
        "i want an ",
        "i would like a ",
        "i would like an ",
        "from scratch",
        "new project",
        "nouvelle application",
        "nouveau projet",
    ]
    .iter()
    .any(|keyword| lower.contains(keyword))
}

pub(crate) fn studio_seed_initial_backlog(
    project_root: &Path,
    root_ticket_id: &str,
) -> Result<Vec<String>, String> {
    let Some(root_ticket) = studio_get_ticket(project_root, root_ticket_id) else {
        return Err("initial_request_ticket_not_found".to_string());
    };
    let seeded_event = studio_list_ticket_events(project_root, root_ticket_id)
        .into_iter()
        .find(|event| event.event_type == "initial_backlog_seeded");
    if let Some(event) = seeded_event.as_ref() {
        let child_ids: Vec<String> = event
            .payload
            .as_ref()
            .and_then(|payload| payload.get("child_ticket_ids"))
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default();
        for (index, child_id) in child_ids.iter().enumerate() {
            let Some(mut child) = studio_get_ticket(project_root, child_id) else {
                continue;
            };
            let phase = match index {
                0 => "foundation",
                1 => "features",
                2 => "integration",
                _ => "validation",
            };
            let mut changed = false;
            if child.parent_ticket_id.as_deref() != Some(root_ticket_id) {
                child.parent_ticket_id = Some(root_ticket_id.to_string());
                changed = true;
            }
            if child.creation_key.is_none() {
                child.creation_key = Some(format!("initial:{root_ticket_id}:{phase}"));
                changed = true;
            }
            if child.depends_on_ticket_ids.iter().any(|id| id == root_ticket_id) {
                child.depends_on_ticket_ids.retain(|id| id != root_ticket_id);
                normalize_ticket_dependencies(&mut child);
                changed = true;
            }
            if changed {
                child.updated_at = chrono::Utc::now().to_rfc3339();
                studio_upsert_ticket(project_root, child)?;
            }
        }
    }
    if !is_initial_backlog_root(&root_ticket)
        || seeded_event.is_some()
    {
        return Ok(Vec::new());
    }
    for mut child in studio_list_tickets(project_root).into_iter().filter(|ticket| {
        ticket.id != root_ticket_id
            && ticket
                .depends_on_ticket_ids
                .iter()
                .any(|dependency| dependency == root_ticket_id)
    }) {
        child.parent_ticket_id = Some(root_ticket_id.to_string());
        child
            .depends_on_ticket_ids
            .retain(|dependency| dependency != root_ticket_id);
        normalize_ticket_dependencies(&mut child);
        child.updated_at = chrono::Utc::now().to_rfc3339();
        studio_upsert_ticket(project_root, child)?;
    }
    let existing = studio_list_tickets(project_root);
    if studio_initial_backlog_is_ready(&root_ticket, &existing) {
        return Ok(Vec::new());
    }

    let phases = [
        (
            "Établir le socle du projet",
            "studio_scaffold",
            "Examiner le dépôt et sa stack, réutiliser la structure existante et ne créer que les éléments de socle réellement absents. `workspace:/` est la racine canonique de l'application: réutilise ses manifests et ne crée pas un second projet imbriqué. Critère: l'architecture, le point d'entrée et les commandes du projet sont identifiés et cohérents avec la demande.",
            Vec::<String>::new(),
        ),
        (
            "Implémenter les fonctionnalités demandées",
            "studio_fullstack",
            "Implémenter les comportements et fonctionnalités décrits dans la demande, avec des critères vérifiables. Critère: chaque comportement demandé possède une implémentation réelle, sans placeholder ni parcours principal simulé.",
            vec!["foundation".to_string()],
        ),
        (
            "Intégrer les fonctionnalités dans l'application",
            "studio_fullstack",
            "Relier les fonctionnalités à l'interface, à l'état et aux services déjà présents, en couvrant les états d'erreur pertinents. Critère: le parcours principal fonctionne de bout en bout dans l'application.",
            vec!["features".to_string()],
        ),
        (
            "Valider le projet et le parcours utilisateur",
            "studio_fullstack",
            "Exécuter les contrôles adaptés au dépôt et parcourir l'application sur les tailles d'écran pertinentes. Lance le build et l'aperçu depuis la racine canonique `workspace:/`; ne valide pas un autre projet imbriqué à la place. Critères: build et vérifications applicables réussis; tout défaut distinct découvert est consigné dans un ticket Kanban.",
            vec!["integration".to_string()],
        ),
    ];
    let mut phase_ids: HashMap<String, String> = HashMap::new();
    let mut created_ids = Vec::new();
    for (key, (title, assigned_agent, phase_description, dependency_keys)) in phases.iter().enumerate() {
        let phase_key = match key {
            0 => "foundation",
            1 => "features",
            2 => "integration",
            _ => "validation",
        };
        let creation_key = format!("initial:{root_ticket_id}:{phase_key}");
        let legacy_criterion_id = format!("initial-phase-{key}");
        if let Some(mut existing_ticket) = studio_find_duplicate_ticket(
            project_root,
            Some(root_ticket_id),
            Some(&creation_key),
            title,
        )
        .or_else(|| {
            studio_list_tickets(project_root).into_iter().find(|ticket| {
                ticket.requested_by == "system_initial_backlog"
                    && ticket
                        .acceptance_criteria
                        .iter()
                        .any(|criterion| criterion.id.as_deref() == Some(legacy_criterion_id.as_str()))
            })
        }) {
            let mut changed = false;
            if existing_ticket.parent_ticket_id.as_deref() != Some(root_ticket_id) {
                existing_ticket.parent_ticket_id = Some(root_ticket_id.to_string());
                changed = true;
            }
            if existing_ticket.creation_key.as_deref() != Some(creation_key.as_str()) {
                existing_ticket.creation_key = Some(creation_key);
                changed = true;
            }
            if changed {
                existing_ticket.updated_at = chrono::Utc::now().to_rfc3339();
                studio_upsert_ticket(project_root, existing_ticket.clone())?;
            }
            phase_ids.insert(phase_key.to_string(), existing_ticket.id.clone());
            created_ids.push(existing_ticket.id);
            continue;
        }
        let ticket_id = uuid::Uuid::new_v4().to_string();
        let dependencies: Vec<String> = dependency_keys
            .iter()
            .filter_map(|key| phase_ids.get(key).cloned())
            .collect();
        let ticket = StudioTicket {
            initial_backlog_root: false,
            id: ticket_id.clone(),
            project_id: root_ticket.project_id.clone(),
            title: (*title).to_string(),
            description: format!(
                "{phase_description}\n\nCe ticket fait partie du backlog initial du ticket racine {root_ticket_id}; lis sa description pour connaître la demande complète. Tous les fichiers de cette application restent sous la racine canonique `workspace:/`; inspecte et réutilise la stack existante au lieu de créer un projet imbriqué. Ne modifie pas le suivi directement: rapporte les fichiers et les résultats au chef de projet via Code Studio."
            ),
            status: "todo".to_string(),
            requested_by: "system_initial_backlog".to_string(),
            assigned_agent: (*assigned_agent).to_string(),
            review_agent: "studio_reviewer".to_string(),
            parent_ticket_id: Some(root_ticket_id.to_string()),
            creation_key: Some(creation_key),
            depends_on_ticket_ids: dependencies,
            depends_on_ticket_id: None,
            related_task_id: None,
            acceptance_criteria: {
                let mut criteria=vec![StudioTicketAcceptanceCriterion {
                id: Some(format!("initial-phase-{key}")),
                text: phase_description.to_string(),
                kind: None,
                path: None,
                argv: None,
                }];
                if key > 0 {criteria.extend(studio_request_criteria(&root_ticket.description));}
                criteria
            },
            evidence: StudioTicketEvidence::default(),
            review_outcome: None,
            review_notes: None,
            corrective_steps: Vec::new(),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        };
        studio_upsert_ticket(project_root, ticket.clone())?;
        studio_append_ticket_event(
            project_root,
            &ticket_id,
            "ticket_created",
            "system_initial_backlog",
            Some(json!({
                "source": "initial_kanban_backlog",
                "root_ticket_id": root_ticket_id,
                "phase": key,
                "depends_on_ticket_ids": ticket.depends_on_ticket_ids,
            })),
        )?;
        phase_ids.insert(phase_key.to_string(), ticket_id.clone());
        created_ids.push(ticket_id);
    }
    studio_append_ticket_event(
        project_root,
        root_ticket_id,
        "initial_backlog_seeded",
        "system",
        Some(json!({
            "child_ticket_ids": created_ids,
            "schema_version": 1,
        })),
    )?;
    Ok(created_ids)
}

/// Complete the initial request ticket only after the refined backlog and all
/// other project tickets have been reviewed and approved.
pub(crate) fn studio_complete_initial_backlog_if_ready(
    project_root: &Path,
) -> Result<Vec<String>, String> {
    let tickets = studio_list_tickets(project_root);
    let mut completed_roots = Vec::new();
    for root in tickets.iter().filter(|ticket| {
        is_initial_backlog_root(ticket)
    }) {
        let mut root = root.clone();
        if !studio_initial_backlog_is_ready_for_project(project_root, &root, &tickets) {
            if root.status == "done" {
                root.status = "in_progress".to_string();
                root.review_outcome = None;
                root.review_notes = Some(
                    "Réouverture automatique : le Kanban initial contient encore des gabarits ou des critères non spécifiques."
                        .to_string(),
                );
                root.updated_at = chrono::Utc::now().to_rfc3339();
                studio_upsert_ticket(project_root, root.clone())?;
                studio_append_ticket_event(
                    project_root,
                    &root.id,
                    "initial_backlog_reopened",
                    "system",
                    Some(json!({ "reason": "initial_backlog_not_refined" })),
                )?;
            }
            continue;
        }
        if root.status == "done" {
            continue;
        }
        let child_ids: Vec<String> = studio_list_ticket_events(project_root, &root.id)
            .into_iter()
            .filter(|event| event.event_type == "initial_backlog_seeded")
            .filter_map(|event| event.payload)
            .filter_map(|payload| payload.get("child_ticket_ids").cloned())
            .filter_map(|value| serde_json::from_value(value).ok())
            .next()
            .unwrap_or_default();
        if child_ids.len() < 3 {
            continue;
        }
        let children: Vec<StudioTicket> = child_ids
            .iter()
            .filter_map(|id| studio_get_ticket(project_root, id))
            .collect();
        if children.len() != child_ids.len()
            || children.iter().any(|ticket| {
                ticket.status != "done" || ticket.review_outcome.as_deref() != Some("approved")
            })
        {
            continue;
        }
        let all_project_work_is_approved = tickets.iter().filter(|ticket| ticket.id != root.id).all(
            |ticket| {
                ticket.status == "done" && ticket.review_outcome.as_deref() == Some("approved")
            },
        );
        if !all_project_work_is_approved {
            continue;
        }

        root.status = "done".to_string();
        root.review_outcome = Some("approved".to_string());
        root.review_notes = Some(
            "Tous les tickets du projet ont été validés, y compris le backlog initial."
                .to_string(),
        );
        root.evidence.summary = root.review_notes.clone();
        for child in &children {
            for file in &child.evidence.files {
                if !root.evidence.files.contains(file) {
                    root.evidence.files.push(file.clone());
                }
            }
            for task_id in &child.evidence.task_ids {
                if !root.evidence.task_ids.contains(task_id) {
                    root.evidence.task_ids.push(task_id.clone());
                }
            }
        }
        root.updated_at = chrono::Utc::now().to_rfc3339();
        studio_upsert_ticket(project_root, root.clone())?;
        studio_append_ticket_event(
            project_root,
            &root.id,
            "initial_backlog_completed",
            "system",
            Some(json!({
                "child_ticket_ids": child_ids,
                "review_outcome": "approved",
            })),
        )?;
        completed_roots.push(root.id);
    }
    Ok(completed_roots)
}

fn studio_chat_ticket_title(message: &str) -> String {
    const MAX_CHARS: usize = 84;
    let title_source=message.split_once("selon ce résumé :").map(|(_,summary)|summary).unwrap_or(message);
    let first_line = title_source
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let compact = first_line.split_whitespace().collect::<Vec<_>>().join(" ");
    let sentence = compact
        .find(|character| matches!(character, '.' | '!' | '?'))
        .map(|end| compact[..end].trim())
        .filter(|head| !head.is_empty())
        .unwrap_or(&compact);
    let mut title = sentence.chars().take(MAX_CHARS).collect::<String>();
    if sentence.chars().count() > MAX_CHARS {
        title.push('…');
    }
    if title.is_empty() {
        "Évolution (chat)".to_string()
    } else {
        title
    }
}

fn studio_request_criteria(message:&str)->Vec<StudioTicketAcceptanceCriterion> {
    let request=message.split_once("selon ce résumé :").map(|(_,summary)|summary).unwrap_or(message);
    request.lines().map(str::trim).filter(|line|!line.is_empty()).take(12).enumerate().map(|(index,line)| {
        StudioTicketAcceptanceCriterion {id:Some(format!("request-behavior-{index}")),text:format!("Résultat à démontrer : {}",line.chars().take(900).collect::<String>()),kind:None,path:None,argv:None}
    }).collect()
}

#[cfg(test)]
mod initial_request_recovery_tests {
    use super::*;
    fn project()->tempfile::TempDir {
        let dir=tempfile::tempdir().unwrap();
        let meta=serde_json::from_value(json!({"id":"todo-test","name":"Todo","created_at":chrono::Utc::now().to_rfc3339(),"evolutions":[],"verify_skip":false})).unwrap();
        save_studio_meta(dir.path(),&meta).unwrap();
        dir
    }
    const REQUEST:&str="Crée le projet « Todo E2E — 2026-10-22 » selon ce résumé :\n\nPetit outil Todo en français, responsive et accessible, avec tâches à ajouter, terminer, rouvrir et supprimer, filtres et persistance locale.";
    #[test]
    fn observed_creation_request_seeds_specific_criteria_and_preserves_the_ticket_on_retry() {
        let dir=project();
        let id=studio_create_evolution_ticket_from_chat(dir.path(),REQUEST).unwrap();
        let mut root=studio_get_ticket(dir.path(),&id).unwrap();
        assert_eq!(root.assigned_agent,"studio_project_manager");
        assert!(root.title.starts_with("Petit outil Todo"));
        let children:Vec<_>=studio_list_tickets(dir.path()).into_iter().filter(|ticket|ticket.parent_ticket_id.as_deref()==Some(&id)).collect();
        assert_eq!(children.len(),4);
        assert!(children.iter().filter(|ticket|!ticket.depends_on_ticket_ids.is_empty()).count()>=3);
        assert!(children.iter().filter(|ticket|ticket.creation_key.as_deref().is_some_and(|key|key.ends_with(":features"))).all(|ticket|ticket.acceptance_criteria.iter().any(|criterion|criterion.text.contains("persistance locale"))));
        root.status="blocked".into();
        studio_upsert_ticket(dir.path(),root).unwrap();
        assert_eq!(studio_create_evolution_ticket_from_chat(dir.path(),REQUEST).unwrap(),id);
        assert_eq!(studio_list_tickets(dir.path()).len(),5);
    }
    #[test]
    fn legacy_single_ticket_is_upgraded_once_without_erasing_the_failed_run() {
        let dir=project();
        let id=studio_create_evolution_ticket_from_chat(dir.path(),REQUEST).unwrap();
        let mut root=studio_get_ticket(dir.path(),&id).unwrap();
        root.acceptance_criteria.retain(|criterion|matches!(criterion.id.as_deref(),Some("request-scope"|"verification")));
        root.status="blocked".into();root.related_task_id=Some("original-run".into());root.evidence.summary=Some("timeout".into());
        let mut legacy = serde_json::to_value(root).unwrap();
        legacy.as_object_mut().unwrap().remove("initial_backlog_root");
        std::fs::write(dir.path().join(STUDIO_TICKETS_FILE),json!({"tickets":[legacy],"events":[]}).to_string()).unwrap();
        studio_upgrade_initial_request(dir.path()).unwrap();studio_upgrade_initial_request(dir.path()).unwrap();
        let root=studio_get_ticket(dir.path(),&id).unwrap();
        assert_eq!(studio_list_tickets(dir.path()).len(),5);
        assert_eq!(root.related_task_id.as_deref(),Some("original-run"));
        assert_eq!(root.status,"blocked");assert_eq!(root.evidence.summary.as_deref(),Some("timeout"));
    }
    #[test]
    fn resuming_a_blocked_review_restores_review_without_duplicate_task_evidence() {
        let dir=project();let id=studio_create_evolution_ticket_from_chat(dir.path(),REQUEST).unwrap();
        let mut root=studio_get_ticket(dir.path(),&id).unwrap();
        root.status="blocked".into();root.evidence.resume_status=Some("review".into());root.evidence.summary=Some("timeout".into());
        studio_upsert_ticket(dir.path(),root).unwrap();
        studio_attach_task_to_ticket(dir.path(),&id,"same-run","test").unwrap();
        studio_attach_task_to_ticket(dir.path(),&id,"same-run","test").unwrap();
        let root=studio_get_ticket(dir.path(),&id).unwrap();
        assert_eq!(root.status,"review");assert_eq!(root.evidence.summary,None);assert_eq!(root.evidence.task_ids,vec!["same-run"]);
    }
}

/// Upgrade a legacy creation ticket in place, preserving its task history.
pub(crate) fn studio_upgrade_initial_request(project_root:&Path)->Result<(),String> {
    let tickets=studio_list_tickets(project_root);
    if tickets.iter().any(is_initial_backlog_root) {return Ok(());}
    let roots:Vec<_>=tickets.iter().filter(|ticket|ticket.requested_by=="user_chat" && ticket.parent_ticket_id.is_none()).collect();
    if roots.len()!=1 || roots[0].status=="done" || !studio_user_message_suggests_project_creation(&roots[0].description) {return Ok(());}
    let mut root=roots[0].clone();
    root.initial_backlog_root=true;
    root.assigned_agent="studio_project_manager".into();
    root.title=studio_chat_ticket_title(&root.description);
    root.acceptance_criteria.extend(studio_request_criteria(&root.description));
    root.acceptance_criteria.push(StudioTicketAcceptanceCriterion{id:Some("initial-kanban-backlog".into()),text:"Décomposer la demande en tickets spécifiques avec critères vérifiables et dépendances avant toute implémentation.".into(),kind:None,path:None,argv:None});
    studio_upsert_ticket(project_root,root.clone())?;
    studio_seed_initial_backlog(project_root,&root.id)?;
    Ok(())
}

/// Auto-register an actionable chat request and seed an initial project backlog
/// when this is the first request to create a project or application.
pub fn studio_create_evolution_ticket_from_chat(
    project_root: &Path,
    message: &str,
) -> Result<String, String> {
    let meta = load_studio_meta(project_root).ok_or_else(|| "studio_meta_missing".to_string())?;
    let normalized_message = normalize_ticket_identity(message);
    if let Some(existing) = studio_list_tickets(project_root).into_iter().find(|ticket| {
        ticket.requested_by == "user_chat"
            && ticket.status != "done"
            && normalize_ticket_identity(&ticket.description) == normalized_message
    }) {
        let _ = studio_append_ticket_event(
            project_root,
            &existing.id,
            "ticket_duplicate_suppressed",
            "system",
            Some(json!({ "source": "chat_evolution_auto", "title": existing.title })),
        );
        studio_upgrade_initial_request(project_root)?;
        return Ok(existing.id);
    }
    let is_first_project_creation = studio_list_tickets(project_root).is_empty()
        && studio_user_message_suggests_project_creation(message);
    let title = studio_chat_ticket_title(message);
    let new_ticket_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let ticket = StudioTicket {
        initial_backlog_root: is_first_project_creation,
        id: new_ticket_id.clone(),
        project_id: meta.id.clone(),
        title,
        description: message.trim().to_string(),
        status: "todo".to_string(),
        requested_by: "user_chat".to_string(),
        assigned_agent: if is_first_project_creation {"studio_project_manager"}else{"studio_fullstack"}.to_string(),
        review_agent: "studio_reviewer".to_string(),
        parent_ticket_id: None,
        creation_key: None,
        depends_on_ticket_ids: Vec::new(),
        depends_on_ticket_id: None,
        related_task_id: None,
        acceptance_criteria: {
            let mut criteria = vec![
                StudioTicketAcceptanceCriterion {
                    id: Some("request-scope".to_string()),
                    text: "Le résultat couvre le besoin décrit dans cette demande.".to_string(),
                    kind: None,
                    path: None,
                    argv: None,
                },
                StudioTicketAcceptanceCriterion {
                    id: Some("verification".to_string()),
                    text: "Les vérifications pertinentes du projet sont exécutées et leurs résultats consignés.".to_string(),
                    kind: None,
                    path: None,
                    argv: None,
                },
            ];
            if is_first_project_creation {
                criteria.extend(studio_request_criteria(message));
                criteria.push(StudioTicketAcceptanceCriterion {
                    id: Some("initial-kanban-backlog".to_string()),
                    text: "Avant l'implémentation, la première demande est décomposée dans le Kanban en tickets de portée claire, avec critères vérifiables et dépendances explicites pour le socle, les fonctionnalités demandées, l'intégration et la validation.".to_string(),
                    kind: None,
                    path: None,
                    argv: None,
                });
            }
            criteria
        },
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
    if is_first_project_creation {
        studio_seed_initial_backlog(project_root, &ticket.id)?;
    }
    Ok(new_ticket_id)
}

/// Agent tool: create ticket (JSON args). `assigned_agent` defaults to `studio_fullstack`; `review_agent` to `studio_reviewer`.
pub fn studio_tool_create_ticket_json(
    project_root: &Path,
    body: &serde_json::Value,
) -> Result<StudioTicket, String> {
    if body.get("initial_backlog_root").is_some() {
        return Err("initial_backlog_root_is_host_owned".into());
    }
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
    let parent_ticket_id = match body.get("parent_ticket_id") {
        Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(value)) => {
            let value = value.trim();
            if value.is_empty() {
                return Err("parent_ticket_id_invalid".to_string());
            }
            Some(value.to_string())
        }
        Some(_) => return Err("parent_ticket_id_invalid".to_string()),
        None if requested_by == "studio_project_manager" => {
            let roots = studio_list_tickets(project_root)
                .into_iter()
                .filter(|ticket| {
                    ticket.requested_by == "user_chat"
                        && ticket.parent_ticket_id.is_none()
                        && ticket.status != "done"
                        && ticket.status != "blocked"
                })
                .collect::<Vec<_>>();
            match roots.as_slice() {
                [] => None,
                [root] => Some(root.id.clone()),
                _ => return Err("parent_ticket_ambiguous".to_string()),
            }
        }
        None => None,
    };
    if let Some(parent_id) = parent_ticket_id.as_deref() {
        let Some(parent) = studio_get_ticket(project_root, parent_id) else {
            return Err("parent_ticket_not_found".to_string());
        };
        if parent.project_id != meta.id {
            return Err("parent_ticket_project_mismatch".to_string());
        }
        if studio_ticket_parent_would_cycle(project_root, &uuid::Uuid::new_v4().to_string(), parent_id) {
            return Err("parent_ticket_cycle".to_string());
        }
    }
    let creation_key = match body.get("creation_key") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(value)) => {
            let value = value.trim();
            if value.is_empty() || value.chars().count() > 160 {
                return Err("creation_key_invalid".to_string());
            }
            Some(value.to_string())
        }
        Some(_) => return Err("creation_key_invalid".to_string()),
    };
    if let Some(existing) = studio_find_duplicate_ticket(
        project_root,
        parent_ticket_id.as_deref(),
        creation_key.as_deref(),
        title,
    ) {
        let _ = studio_append_ticket_event(
            project_root,
            &existing.id,
            "ticket_duplicate_suppressed",
            "agent",
            Some(json!({
                "source": "studio_create_ticket",
                "attempted_title": title,
                "parent_ticket_id": parent_ticket_id,
                "creation_key": creation_key,
                "canonical_ticket_id": existing.id,
            })),
        );
        return Ok(existing);
    }
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
        Some(serde_json::Value::String(s)) if s.trim().is_empty() => {}
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
        initial_backlog_root: false,
        id: new_ticket_id,
        project_id: meta.id.clone(),
        title: title.to_string(),
        description,
        status,
        requested_by,
        assigned_agent,
        review_agent,
        parent_ticket_id,
        creation_key,
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
    if let Some(parent_id) = ticket.parent_ticket_id.as_deref() {
        studio_reopen_user_request_parent_for_followup(
            project_root,
            parent_id,
            &ticket.id,
            "agent",
        )?;
    }
    let mut related_parent_ids = ticket.depends_on_ticket_ids.clone();
    if let Some(parent_id) = ticket.parent_ticket_id.as_ref() {
        if !related_parent_ids.iter().any(|candidate| candidate == parent_id) {
            related_parent_ids.push(parent_id.clone());
        }
    }
    for parent_id in &related_parent_ids {
        let Some(mut parent) = studio_get_ticket(project_root, parent_id) else {
            continue;
        };
        if parent.requested_by == "user_chat"
            && parent.review_outcome.as_deref() == Some("changes_requested")
        {
            let previous_review = parent.review_notes.clone();
            let previous_corrections = parent.corrective_steps.clone();
            parent.review_outcome = None;
            parent.review_notes = None;
            parent.corrective_steps.clear();
            parent.evidence.summary = None;
            parent.updated_at = chrono::Utc::now().to_rfc3339();
            studio_upsert_ticket(project_root, parent.clone())?;
            studio_append_ticket_event(
                project_root,
                &parent.id,
                "ticket_follow_up_created",
                "agent",
                Some(json!({
                    "child_ticket_id": ticket.id,
                    "previous_review_notes": previous_review,
                    "previous_corrective_steps": previous_corrections,
                })),
            )?;
        }
    }
    let _ = studio_append_ticket_event(
        project_root,
        &ticket.id,
        "ticket_created",
        "agent",
        Some(json!({
            "assigned_agent": ticket.assigned_agent,
            "review_agent": ticket.review_agent,
            "parent_ticket_id": ticket.parent_ticket_id,
            "creation_key": ticket.creation_key,
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
    if body.get("initial_backlog_root").is_some() {
        return Err("initial_backlog_root_is_host_owned".into());
    }
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
    let mut guarded_body = body.clone();
    if ticket.initial_backlog_root && guarded_body.get("parent_ticket_id").is_some_and(|v| !v.is_null()) {
        return Err("initial_backlog_root_cannot_be_reparented".into());
    }
    if ticket.requested_by == "system_initial_backlog" {
        if guarded_body.get("parent_ticket_id").is_some_and(|v| v.as_str() != ticket.parent_ticket_id.as_deref()) {
            return Err("initial_phase_parent_is_host_owned".into());
        }
        if let (Some(parent), Some(key)) = (ticket.parent_ticket_id.as_deref(), ticket.creation_key.as_deref()) {
            let prefix = format!("initial:{parent}:");
            let previous = match key.strip_prefix(&prefix) {
                Some("features") => Some("foundation"),
                Some("integration") => Some("features"),
                Some("validation") => Some("integration"),
                _ => None,
            };
            if let Some(previous) = previous {
                let required = studio_list_tickets(project_root).into_iter().find(|t|
                    t.parent_ticket_id.as_deref() == Some(parent) && t.creation_key.as_deref() == Some(&format!("{prefix}{previous}")));
                if let Some(required) = required {
                    let mut deps: Vec<String> = match guarded_body.get("depends_on_ticket_ids") {
                        Some(serde_json::Value::Null) => Vec::new(),
                        Some(serde_json::Value::Array(values)) => values.iter().map(|v|
                            v.as_str().map(str::trim).filter(|s|!s.is_empty()).map(str::to_owned)
                                .ok_or_else(|| "depends_on_ticket_ids_must_be_string_array".to_string())).collect::<Result<_,_>>()?,
                        Some(_) => return Err("depends_on_ticket_ids_must_be_array_or_null".into()),
                        None => match guarded_body.get("depends_on_ticket_id") {
                            Some(serde_json::Value::Null) => Vec::new(),
                            Some(serde_json::Value::String(value)) => if value.trim().is_empty() {Vec::new()} else {vec![value.trim().to_string()]},
                            Some(_) => return Err("depends_on_ticket_id_invalid".into()),
                            None => ticket_dependency_ids(&ticket),
                        },
                    };
                    if !deps.contains(&required.id) { deps.push(required.id); }
                    guarded_body["depends_on_ticket_ids"] = json!(deps);
                }
            }
        }
    }
    let body_v = &guarded_body;
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
    if let Some(parent_value) = body_v.get("parent_ticket_id") {
        let next_parent = match parent_value {
            serde_json::Value::Null => None,
            serde_json::Value::String(value) if !value.trim().is_empty() => {
                Some(value.trim().to_string())
            }
            _ => return Err("parent_ticket_id_invalid".to_string()),
        };
        if let Some(parent_id) = next_parent.as_deref() {
            let Some(parent) = studio_get_ticket(project_root, parent_id) else {
                return Err("parent_ticket_not_found".to_string());
            };
            if parent.project_id != ticket.project_id {
                return Err("parent_ticket_project_mismatch".to_string());
            }
            if studio_ticket_parent_would_cycle(project_root, ticket_id, parent_id) {
                return Err("parent_ticket_cycle".to_string());
            }
        }
        ticket.parent_ticket_id = next_parent;
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
            Some(serde_json::Value::String(s)) if s.trim().is_empty() => None,
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
    if let Some(value) = body_v.get("evidence") {
        let evidence = serde_json::from_value::<StudioTicketEvidence>(value.clone())
            .map_err(|_| "evidence_invalid".to_string())?;
        let mut files = Vec::new();
        for file in evidence.files {
            let file = file.trim();
            if !file.is_empty() && !files.iter().any(|existing| existing == file) {
                files.push(file.to_string());
            }
        }
        ticket.evidence.files = files;
        ticket.evidence.summary = evidence
            .summary
            .map(|summary| summary.trim().to_string())
            .filter(|summary| !summary.is_empty());
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
    if body_v.get("parent_ticket_id").is_some() {
        if let Some(parent_id) = ticket.parent_ticket_id.as_deref() {
            studio_reopen_user_request_parent_for_followup(
                project_root,
                parent_id,
                &ticket.id,
                "agent",
            )?;
        }
    }
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

/// Agent tool: submit a ticket review using the reviewer's bound agent identity.
/// Reviewers cannot approve tickets they did not receive or write review data on another agent's behalf.
pub fn studio_tool_apply_ticket_review_json(
    project_root: &Path,
    reviewer_agent: &str,
    body: &serde_json::Value,
) -> Result<StudioTicket, String> {
    let ticket_id = body
        .get("ticket_id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "ticket_id_required".to_string())?;
    let Some(mut ticket) = studio_get_ticket(project_root, ticket_id) else {
        return Err("ticket_not_found".to_string());
    };
    if ticket.status != "review" {
        return Err("ticket_not_in_review".to_string());
    }
    if reviewer_agent.trim().is_empty() || !reviewer_agent.eq_ignore_ascii_case(&ticket.review_agent) {
        return Err("reviewer_not_authorized".to_string());
    }
    let outcome = body
        .get("outcome")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| "review_outcome_required".to_string())?;
    if !matches!(outcome.as_str(), "approved" | "changes_requested") {
        return Err("review_outcome_invalid".to_string());
    }
    let review_notes = body
        .get("review_notes")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let corrective_steps = body
        .get("corrective_steps")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if outcome == "changes_requested" && corrective_steps.is_empty() {
        return Err("corrective_steps_required".to_string());
    }
    ticket.review_outcome = Some(outcome.clone());
    ticket.review_notes = review_notes.clone();
    ticket.corrective_steps = corrective_steps.clone();
    ticket.evidence.summary = review_notes.clone().or(ticket.evidence.summary.take());
    let child_tickets = studio_list_tickets(project_root)
        .into_iter()
        .filter(|child| {
            child.id != ticket.id
                && (child.parent_ticket_id.as_deref() == Some(ticket.id.as_str())
                    || ticket_dependency_ids(child).iter().any(|id| id == &ticket.id))
        })
        .collect::<Vec<_>>();
    for child in child_tickets {
        for file in child.evidence.files {
            if !ticket.evidence.files.contains(&file) {
                ticket.evidence.files.push(file);
            }
        }
        for task_id in child.evidence.task_ids {
            if !ticket.evidence.task_ids.contains(&task_id) {
                ticket.evidence.task_ids.push(task_id);
            }
        }
    }
    ticket.status = if outcome == "approved" {
        "done".to_string()
    } else {
        "in_progress".to_string()
    };
    ticket.updated_at = chrono::Utc::now().to_rfc3339();
    studio_upsert_ticket(project_root, ticket.clone())?;
    let event_type = if outcome == "approved" {
        "ticket_review_approved"
    } else {
        "ticket_review_changes_requested"
    };
    let _ = studio_append_ticket_event(
        project_root,
        &ticket.id,
        event_type,
        reviewer_agent,
        Some(serde_json::json!({
            "review_notes": review_notes,
            "corrective_steps": corrective_steps,
        })),
    );
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
            matches!(subcommand, Some("run") | Some("install") | Some("ci") | Some("test"))
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

pub(crate) fn argv_looks_safe(argv: &[String]) -> bool {
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
    if ["--file_text", "--content", "--file-content"].iter().any(|prefix|t.starts_with(prefix)) {
        return Some("rejected_tool_argument_leak: source begins with a command argument. Use a native write_file call with path/content fields; preserve source quotes and never include CLI flags in content.".into());
    }
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

/// Run the configured build/check. Ok(true) = command passed; Ok(false) = skipped;
/// Err = configured verification failed, including when a weaker fallback bundles successfully.
pub(crate) fn studio_verification_argv(project_root: &Path) -> Result<Option<Vec<String>>, String> {
    let meta = load_studio_meta(project_root);
    if meta.as_ref().is_some_and(|m|m.verify_skip) { return Ok(None); }
    let argv = if let Some(argv)=meta.as_ref().and_then(|m|m.verify_argv.as_ref()) {
        argv.clone()
    } else if project_root.join("package.json").is_file() {
        vec!["npm".into(),"run".into(),"build".into()]
    } else if project_root.join("Cargo.toml").is_file() {
        vec!["cargo".into(),"check".into()]
    } else {return Ok(None);};
    if argv.is_empty() {return Ok(None);}
    if !argv_looks_safe(&argv) {return Err("verify_argv in .akasha-studio.json is invalid or unsafe".into());}
    Ok(Some(argv))
}

/// Read-only verification helper. Repairs and installations are exclusively
/// executed by the tracked main tool loop, never by a second repair engine.
pub async fn studio_verify_after_agent_task(project_root: &Path) -> Result<bool, String> {
    let Some(argv)=studio_verification_argv(project_root)? else {return Ok(false);};
    match studio_run_command_capture(project_root,&argv,studio_project_verify_timeout_sec(project_root)).await {
        Ok((Some(0),_,_))=>Ok(true),
        Ok((code,out,err))=>Err(format_verify_failure(code,&out,&err)),
        Err(error)=>Err(error),
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
    pub remote: bool,
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
        &["for-each-ref", "--format=%(refname:short)|%(upstream:short)|%(HEAD)|%(refname)", "refs/heads", "refs/remotes"],
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
        let remote=parts.get(3).is_some_and(|reference|reference.starts_with("refs/remotes/"));
        if remote && name.ends_with("/HEAD") {continue;}
        rows.push(StudioGitBranch {
            remote,
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
    #[test]
    fn recognizes_user_and_scheduled_initial_backlog_planning_runs() {
        assert!(super::studio_is_initial_backlog_planning_message(
            "[Code Studio — première demande : planifier le backlog initial]"
        ));
        assert!(super::studio_is_initial_backlog_planning_message(
            "[Planification Kanban initiale Code Studio]"
        ));
        assert!(!super::studio_is_initial_backlog_planning_message(
            "Crée une webapp de todo list avec une page responsive."
        ));
    }

    #[test]
    fn workspace_inventory_marks_empty_roots_and_hides_secret_files() {
        let dir = tempfile::tempdir().expect("temporary workspace");
        let empty = super::studio_workspace_inventory_message_prefix(dir.path());
        assert!(empty.contains("Racine canonique : `workspace:/`"));
        assert!(empty.contains("Aucun manifeste de stack détecté"));
        assert!(empty.contains("ne demande pas de chemin absolu"));

        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
        std::fs::write(dir.path().join(".env"), "TOKEN=private\n").unwrap();
        let populated = super::studio_workspace_inventory_message_prefix(dir.path());
        assert!(populated.contains("package.json (fichier)"));
        assert!(populated.contains("src (dossier)"));
        assert!(populated.contains("Manifestes détectés : package.json"));
        assert!(!populated.contains(".env"));
        assert!(!populated.contains("private"));
    }

    fn backlog_ticket(
        id: &str,
        status: &str,
        dependencies: &[&str],
        acceptance: &[(&str, &str)],
    ) -> super::StudioTicket {
        super::StudioTicket {
            initial_backlog_root: acceptance.iter().any(|(id, _)| *id == "initial-kanban-backlog"),
            id: id.to_string(),
            project_id: "project".to_string(),
            title: id.to_string(),
            description: String::new(),
            status: status.to_string(),
            requested_by: "agent".to_string(),
            assigned_agent: "studio_fullstack".to_string(),
            review_agent: "studio_reviewer".to_string(),
            parent_ticket_id: None,
            creation_key: None,
            depends_on_ticket_ids: dependencies.iter().map(|id| (*id).to_string()).collect(),
            depends_on_ticket_id: dependencies.first().map(|id| (*id).to_string()),
            related_task_id: None,
            acceptance_criteria: acceptance
                .iter()
                .map(|(id, text)| super::StudioTicketAcceptanceCriterion {
                    id: Some((*id).to_string()),
                    text: (*text).to_string(),
                    kind: None,
                    path: None,
                    argv: None,
                })
                .collect(),
            evidence: super::StudioTicketEvidence::default(),
            review_outcome: None,
            review_notes: None,
            corrective_steps: Vec::new(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn initial_backlog_requires_three_actionable_tickets_and_a_dependency() {
        let root = backlog_ticket(
            "root",
            "in_progress",
            &[],
            &[("initial-kanban-backlog", "Create the first project backlog")],
        );
        let foundation = backlog_ticket(
            "foundation",
            "todo",
            &[],
            &[("setup", "Project scaffold builds")],
        );
        let feature = backlog_ticket(
            "feature",
            "todo",
            &["foundation"],
            &[("feature", "Requested feature works")],
        );
        let validation = backlog_ticket(
            "validation",
            "todo",
            &["feature"],
            &[("validation", "Relevant checks pass")],
        );
        let complete = vec![
            root.clone(),
            foundation.clone(),
            feature.clone(),
            validation.clone(),
        ];
        assert!(super::studio_initial_backlog_is_ready(&root, &complete));

        let generic_seed = vec![
            root.clone(),
            {
                let mut value = backlog_ticket(
                "seed-foundation",
                "todo",
                &[],
                &[("initial-phase-0", "The project scaffold is ready")],
                );
                value.title = "Établir le socle du projet".to_string();
                value.acceptance_criteria[0].text = "Examiner le dépôt et sa stack, réutiliser la structure existante et ne créer que les éléments de socle réellement absents.".to_string();
                value
            },
            {
                let mut value = backlog_ticket(
                "seed-features",
                "todo",
                &["seed-foundation"],
                &[("initial-phase-1", "The requested features work")],
                );
                value.title = "Implémenter les fonctionnalités demandées".to_string();
                value.acceptance_criteria[0].text = "Implémenter les comportements et fonctionnalités décrits dans la demande initiale.".to_string();
                value
            },
            {
                let mut value = backlog_ticket(
                "seed-validation",
                "todo",
                &["seed-features"],
                &[("initial-phase-2", "The app passes validation")],
                );
                value.title = "Valider le projet et le parcours utilisateur".to_string();
                value.acceptance_criteria[0].text = "Exécuter les contrôles adaptés au dépôt et parcourir l'application.".to_string();
                value
            },
        ];
        assert!(!super::studio_initial_backlog_is_ready(
            &root,
            &generic_seed
        ));

        let only_two = vec![root.clone(), foundation.clone(), feature.clone()];
        assert!(!super::studio_initial_backlog_is_ready(&root, &only_two));

        let no_dependencies = vec![
            root.clone(),
            foundation.clone(),
            backlog_ticket(
                "feature-2",
                "todo",
                &[],
                &[("feature", "Second requested feature works")],
            ),
            validation.clone(),
        ];
        assert!(!super::studio_initial_backlog_is_ready(
            &root,
            &no_dependencies
        ));
    }

    #[test]
    fn open_initial_backlog_tracker_does_not_block_work_ticket() {
        let dir = tempfile::tempdir().expect("temporary Code Studio project");
        let root = backlog_ticket(
            "root",
            "in_progress",
            &[],
            &[("initial-kanban-backlog", "Track the overall request")],
        );
        let work = backlog_ticket(
            "work",
            "todo",
            &["root"],
            &[("implement", "Implement the requested change")],
        );
        super::studio_upsert_ticket(dir.path(), root).expect("save umbrella ticket");

        assert!(super::studio_ticket_prerequisite_done(dir.path(), &work));
    }

    #[test]
    fn legacy_user_request_ticket_does_not_block_work_ticket() {
        let dir = tempfile::tempdir().expect("temporary Code Studio project");
        let mut root = backlog_ticket(
            "root",
            "in_progress",
            &[],
            &[("request-scope", "Track the overall request")],
        );
        root.requested_by = "user_chat".to_string();
        let mut work = backlog_ticket(
            "work",
            "todo",
            &["root"],
            &[("implement", "Implement the requested change")],
        );
        work.requested_by = "studio_project_manager".to_string();
        super::studio_upsert_ticket(dir.path(), root).expect("save legacy request ticket");

        assert!(super::studio_ticket_prerequisite_done(dir.path(), &work));
    }

    #[test]
    fn initial_backlog_preflight_blocks_delegation_until_breakdown_exists() {
        use super::{
            save_studio_meta, studio_initial_backlog_preflight, studio_tool_create_ticket_json,
            StudioMeta,
        };

        let dir = tempfile::tempdir().expect("temporary Code Studio project");
        save_studio_meta(
            dir.path(),
            &StudioMeta {
                id: "kanban-bootstrap".to_string(),
                name: "Kanban bootstrap".to_string(),
                created_at: chrono::Utc::now().to_rfc3339(),
                evolutions: Vec::new(),
                tech_stack: None,
                verify_skip: false,
                verify_argv: None,
                verify_timeout_sec: None,
                evolution_summary: None,
                policy_notes: None,
                project_summary: None,
                ticket_enforcement_mode: Some("off".to_string()),
            },
        )
        .expect("save project metadata");
        let root = studio_tool_create_ticket_json(
            dir.path(),
            &serde_json::json!({
                "title": "Initial project request",
                "acceptance_criteria": [{
                    "id": "initial-kanban-backlog",
                    "text": "Create the initial task breakdown before implementation."
                }]
            }),
        )
        .expect("create initial request ticket");
        let mut root = root;
        root.initial_backlog_root = true; // Host-owned fixture identity, not a tool argument.
        super::studio_upsert_ticket(dir.path(), root.clone()).unwrap();
        let task_message = format!("[Ticket Kanban obligatoire]\n- ticket_id: {}", root.id);
        assert!(studio_initial_backlog_preflight(dir.path(), "task-1", &task_message).is_err());

        let foundation = super::studio_tool_create_ticket_json(
            dir.path(),
            &serde_json::json!({
                "title": "Project foundation",
                "acceptance_criteria": [{"text":"The project builds from a clean install."}]
            }),
        )
        .expect("create foundation ticket");
        let feature = super::studio_tool_create_ticket_json(
            dir.path(),
            &serde_json::json!({
                "title": "Core requested features",
                "depends_on_ticket_ids": [foundation.id],
                "acceptance_criteria": [{"text":"The requested interaction works."}]
            }),
        )
        .expect("create feature ticket");
        super::studio_tool_create_ticket_json(
            dir.path(),
            &serde_json::json!({
                "title": "Integration and validation",
                "depends_on_ticket_ids": [feature.id],
                "acceptance_criteria": [{"text":"The integrated application passes its checks."}]
            }),
        )
        .expect("create validation ticket");
        assert!(studio_initial_backlog_preflight(dir.path(), "task-1", &task_message).is_ok());
    }

    #[test]
    fn same_open_parent_ticket_title_is_deduplicated_across_creation_keys() {
        use super::{save_studio_meta, studio_list_tickets, studio_tool_create_ticket_json, StudioMeta};

        let dir = tempfile::tempdir().expect("temporary Code Studio project");
        save_studio_meta(
            dir.path(),
            &StudioMeta {
                id: "ticket-dedupe".to_string(),
                name: "Ticket dedupe".to_string(),
                created_at: chrono::Utc::now().to_rfc3339(),
                evolutions: Vec::new(),
                tech_stack: None,
                verify_skip: false,
                verify_argv: None,
                verify_timeout_sec: None,
                evolution_summary: None,
                policy_notes: None,
                project_summary: None,
                ticket_enforcement_mode: Some("off".to_string()),
            },
        )
        .expect("save project metadata");
        let parent = studio_tool_create_ticket_json(
            dir.path(),
            &serde_json::json!({"title":"Todo app request"}),
        )
        .expect("create request tracker");
        let first = studio_tool_create_ticket_json(
            dir.path(),
            &serde_json::json!({
                "title":"Implement todo filters",
                "parent_ticket_id":parent.id,
                "creation_key":"filters-attempt-1"
            }),
        )
        .expect("create implementation ticket");
        let retry = studio_tool_create_ticket_json(
            dir.path(),
            &serde_json::json!({
                "title":"Implement todo filters",
                "parent_ticket_id":parent.id,
                "creation_key":"filters-attempt-2"
            }),
        )
        .expect("reuse matching ticket");

        assert_eq!(first.id, retry.id);
        assert_eq!(studio_list_tickets(dir.path()).len(), 2);
    }

    #[test]
    fn completed_ticket_creation_key_does_not_block_a_corrective_ticket() {
        use super::{save_studio_meta, studio_tool_create_ticket_json, StudioMeta};

        let dir = tempfile::tempdir().expect("temporary Code Studio project");
        save_studio_meta(
            dir.path(),
            &StudioMeta {
                id: "ticket-dedupe-done".to_string(),
                name: "Ticket dedupe done".to_string(),
                created_at: chrono::Utc::now().to_rfc3339(),
                evolutions: Vec::new(),
                tech_stack: None,
                verify_skip: false,
                verify_argv: None,
                verify_timeout_sec: None,
                evolution_summary: None,
                policy_notes: None,
                project_summary: None,
                ticket_enforcement_mode: Some("off".to_string()),
            },
        )
        .expect("save project metadata");
        let parent = studio_tool_create_ticket_json(
            dir.path(),
            &serde_json::json!({"title":"Todo app request"}),
        )
        .expect("create parent");
        let first = studio_tool_create_ticket_json(
            dir.path(),
            &serde_json::json!({
                "title":"Implement todo filters",
                "parent_ticket_id":parent.id,
                "creation_key":"filters"
            }),
        )
        .expect("create first attempt");
        let mut completed = first.clone();
        completed.status = "done".to_string();
        super::studio_upsert_ticket(dir.path(), completed).expect("complete first attempt");

        let correction = studio_tool_create_ticket_json(
            dir.path(),
            &serde_json::json!({
                "title":"Implement todo filters",
                "parent_ticket_id":parent.id,
                "creation_key":"filters"
            }),
        )
        .expect("allow corrective attempt with the same stable key");
        assert_ne!(correction.id, first.id);
        assert_eq!(correction.status, "todo");
    }

    #[test]
    fn scoped_file_context_retains_constraints_without_copying_maintenance_examples() {
        let dir=tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("CODE_STUDIO_PLAN.md"),"# Title\nPREAMBLE_CONSTRAINT\n## Scope\nPRODUCT_SCOPE\n## Stack\nSTACK_CONSTRAINT\n## Fichiers hors scope\nKEEP.txt must remain unchanged\n## Demandes d'évolutions utilisateur par phase\nMAINTENANCE_EXAMPLE_BAIT\n## Recommandations\nSTYLE_CONSTRAINT\n## Todos\nUNRELATED_BACKLOG").unwrap();
        let context=super::scoped_file_plan_context(dir.path()).unwrap();
        for retained in ["PREAMBLE_CONSTRAINT","PRODUCT_SCOPE","STACK_CONSTRAINT","KEEP.txt","STYLE_CONSTRAINT"] {assert!(context.contains(retained));}
        assert!(!context.contains("MAINTENANCE_EXAMPLE_BAIT"));
        assert!(!context.contains("UNRELATED_BACKLOG"));
        assert!(super::studio_code_plan_message_prefix(dir.path()).unwrap().contains("MAINTENANCE_EXAMPLE_BAIT"));
        std::fs::write(dir.path().join("CODE_STUDIO_PLAN.md"),"## Custom constraints\nCUSTOM_RULE_MUST_REMAIN").unwrap();
        let custom=super::scoped_file_plan_context(dir.path()).unwrap();
        assert!(custom.contains("CUSTOM_RULE_MUST_REMAIN"));
        assert!(!custom.contains(crate::api_llm_loop::DIRECT_FILE_TASK_MARKER));
    }
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
        for prefix in ["--file_text '","--content '","--file-content '"] {
            assert!(studio_reject_polluted_code_content(p,&format!("{prefix}import {{useState}} from 'react';")).is_some());
        }
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
        use super::{
            studio_user_message_suggests_evolution, studio_user_message_suggests_project_creation,
        };
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
        assert!(studio_user_message_suggests_evolution(
            "je veux créer un petit jeu type bomberman en solo contre la machine"
        ));
        assert!(studio_user_message_suggests_evolution(
            "Create a lightweight web app for tracking project tasks."
        ));
        assert!(studio_user_message_suggests_evolution(
            "Ajoute un filtre aux résultats de recherche."
        ));
        assert!(studio_user_message_suggests_evolution(
            "Rends le chat plus lisible sur mobile."
        ));
        assert!(studio_user_message_suggests_evolution(
            "Le bouton de sauvegarde ne répond pas quand je clique dessus."
        ));
        assert!(studio_user_message_suggests_evolution(
            "Make the preview simpler and modernize its layout."
        ));
        assert!(studio_user_message_suggests_evolution(
            "Le Kanban doit être rempli à la première demande et un agent devrait vérifier régulièrement les tickets ouverts pour les dispatcher."
        ));
        assert!(studio_user_message_suggests_evolution("Créer un jeu"));
        assert!(studio_user_message_suggests_evolution("Fix bug"));
        assert!(!studio_user_message_suggests_evolution(
            "Can you explain what this API does?"
        ));
        assert!(studio_user_message_suggests_project_creation(
            "je veux créer un petit jeu type bomberman"
        ));
        assert!(studio_user_message_suggests_project_creation(
            "Je voudrais une application de suivi de budget."
        ));
        assert!(studio_user_message_suggests_project_creation(
            "I need a small website for my portfolio."
        ));
        assert!(studio_user_message_suggests_project_creation(
            "Crée le projet « Todo — observation autonome » selon ce résumé : Crée une webapp de todo list."
        ));
        assert!(studio_user_message_suggests_project_creation(
            "Crée dans ce projet une webapp Todo avec ajout, complétion et filtres."
        ));
        assert!(studio_user_message_suggests_project_creation(
            "Create a web app for a personal todo list."
        ));
        assert!(!studio_user_message_suggests_project_creation(
            "Please fix the bug in the existing project."
        ));
    }

    #[test]
    fn chat_ticket_titles_are_single_line_and_compact() {
        assert_eq!(
            super::studio_chat_ticket_title("  Améliorer le suivi Kanban\nDétails: dispatcher les tickets ouverts."),
            "Améliorer le suivi Kanban"
        );
        let title = super::studio_chat_ticket_title(&format!("{}\nDétails complémentaires", "a".repeat(120)));
        assert_eq!(title.chars().count(), 85);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn ticket_review_tool_is_role_bound_and_records_structured_feedback() {
        use super::{studio_list_ticket_events, studio_tool_apply_ticket_review_json};

        let dir = tempfile::tempdir().expect("temporary project");
        let mut ticket = backlog_ticket(
            "review-ticket",
            "review",
            &[],
            &[("acceptance", "The feature behaves as requested")],
        );
        ticket.review_agent = "studio_reviewer".to_string();
        super::studio_upsert_ticket(dir.path(), ticket).expect("save review ticket");
        let mut child = backlog_ticket(
            "review-child",
            "done",
            &[],
            &[("changed", "The feature behaves as requested")],
        );
        child.parent_ticket_id = Some("review-ticket".to_string());
        child.evidence.files.push("src/feature.ts".to_string());
        child.evidence.task_ids.push("worker-task".to_string());
        super::studio_upsert_ticket(dir.path(), child).expect("save completed child evidence");

        let changes_requested = serde_json::json!({
            "ticket_id":"review-ticket",
            "outcome":"changes_requested",
            "review_notes":"One acceptance criterion is still missing.",
            "corrective_steps":[]
        });
        assert_eq!(
            studio_tool_apply_ticket_review_json(dir.path(), "studio_reviewer", &changes_requested)
                .unwrap_err(),
            "corrective_steps_required"
        );
        assert_eq!(
            studio_tool_apply_ticket_review_json(dir.path(), "studio_fullstack", &serde_json::json!({
                "ticket_id":"review-ticket","outcome":"approved"
            })).unwrap_err(),
            "reviewer_not_authorized"
        );

        let approved = studio_tool_apply_ticket_review_json(
            dir.path(),
            "studio_reviewer",
            &serde_json::json!({
                "ticket_id":"review-ticket",
                "outcome":"approved",
                "review_notes":"The requested behavior and build were verified."
            }),
        )
        .expect("reviewer can approve the assigned review");
        assert_eq!(approved.status, "done");
        assert_eq!(approved.review_outcome.as_deref(), Some("approved"));
        assert_eq!(approved.evidence.files, vec!["src/feature.ts"]);
        assert_eq!(approved.evidence.task_ids, vec!["worker-task"]);
        assert!(approved.evidence.summary.is_some());
        assert!(studio_list_ticket_events(dir.path(), "review-ticket")
            .iter()
            .any(|event| event.event_type == "ticket_review_approved"));
    }

    #[test]
    fn ticket_updates_record_file_and_verification_evidence_without_overwriting_task_ids() {
        let dir = tempfile::tempdir().expect("temporary project");
        let mut ticket = backlog_ticket(
            "evidence-ticket",
            "in_progress",
            &[],
            &[("build", "The project builds successfully")],
        );
        ticket.evidence.task_ids.push("daemon-linked-task".to_string());
        super::studio_upsert_ticket(dir.path(), ticket).expect("save working ticket");

        let updated = super::studio_tool_apply_ticket_patch_json(
            dir.path(),
            &serde_json::json!({
                "ticket_id":"evidence-ticket",
                "evidence":{
                    "files":[" src/main.ts ","src/main.ts","tests/main.spec.ts"],
                    "summary":"npm run build passed.",
                    "task_ids":["untrusted-model-task"]
                }
            }),
        )
        .expect("apply the evidence patch");
        assert_eq!(updated.evidence.files, vec!["src/main.ts", "tests/main.spec.ts"]);
        assert_eq!(updated.evidence.summary.as_deref(), Some("npm run build passed."));
        assert_eq!(updated.evidence.task_ids, vec!["daemon-linked-task"]);
    }

    #[test]
    fn retrying_a_blocked_ticket_clears_the_stale_failure_summary() {
        let dir = tempfile::tempdir().expect("temporary project");
        let mut ticket = backlog_ticket("retry-ticket", "blocked", &[], &[]);
        ticket.evidence.summary = Some("The previous agent run failed.".to_string());
        super::studio_upsert_ticket(dir.path(), ticket).expect("save blocked ticket");

        super::studio_attach_task_to_ticket(
            dir.path(),
            "retry-ticket",
            "new-task-id",
            "kanban_scheduler",
        )
        .expect("attach retry task");

        let retried = super::studio_get_ticket(dir.path(), "retry-ticket").expect("reload ticket");
        assert_eq!(retried.status, "in_progress");
        assert_eq!(retried.evidence.summary, None);
        assert_eq!(retried.related_task_id.as_deref(), Some("new-task-id"));
    }

    #[test]
    fn creating_a_correction_child_clears_the_stale_parent_review_outcome() {
        use super::{
            save_studio_meta, studio_get_ticket, studio_list_ticket_events,
            studio_tool_create_ticket_json, StudioMeta,
        };

        let dir = tempfile::tempdir().expect("temporary project");
        save_studio_meta(
            dir.path(),
            &StudioMeta {
                id: "correction-planning-test".to_string(),
                name: "Correction planning test".to_string(),
                created_at: chrono::Utc::now().to_rfc3339(),
                evolutions: Vec::new(),
                tech_stack: None,
                verify_skip: false,
                verify_argv: None,
                verify_timeout_sec: None,
                evolution_summary: None,
                policy_notes: None,
                project_summary: None,
                ticket_enforcement_mode: None,
            },
        )
        .expect("save project metadata");
        let mut parent = backlog_ticket(
            "request-root",
            "in_progress",
            &[],
            &[("result", "The user request is complete")],
        );
        parent.requested_by = "user_chat".to_string();
        parent.project_id = "correction-planning-test".to_string();
        parent.review_outcome = Some("changes_requested".to_string());
        parent.review_notes = Some("The requested keyboard control is missing.".to_string());
        parent.corrective_steps = vec!["Implement and verify the missing control.".to_string()];
        super::studio_upsert_ticket(dir.path(), parent).expect("save reviewed request");

        studio_tool_create_ticket_json(
            dir.path(),
            &serde_json::json!({
                "title":"Implement the missing keyboard control",
                "description":"Correction requested by the reviewer.",
                "depends_on_ticket_ids":["request-root"],
                "acceptance_criteria":[{"text":"The missing control works and passes the relevant check."}]
            }),
        )
        .expect("create corrective child");

        let refreshed = studio_get_ticket(dir.path(), "request-root").expect("reload parent");
        assert_eq!(refreshed.review_outcome, None);
        assert_eq!(refreshed.review_notes, None);
        assert!(refreshed.corrective_steps.is_empty());
        assert!(studio_list_ticket_events(dir.path(), "request-root")
            .iter()
            .any(|event| event.event_type == "ticket_follow_up_created"));
    }

    #[test]
    fn scheduled_kanban_dispatch_preserves_the_latest_user_selected_model() {
        use super::{
            save_studio_meta, studio_append_ticket_event,
            studio_create_evolution_ticket_from_chat, studio_project_model_preference, StudioMeta,
        };

        let dir = tempfile::tempdir().expect("temporary project");
        save_studio_meta(
            dir.path(),
            &StudioMeta {
                id: "kanban-model-test".to_string(),
                name: "Kanban model test".to_string(),
                created_at: chrono::Utc::now().to_rfc3339(),
                evolutions: Vec::new(),
                tech_stack: None,
                verify_skip: false,
                verify_argv: None,
                verify_timeout_sec: None,
                evolution_summary: None,
                policy_notes: None,
                project_summary: None,
                ticket_enforcement_mode: None,
            },
        )
        .expect("save project metadata");
        let ticket_id = studio_create_evolution_ticket_from_chat(
            dir.path(),
            "Ajouter un parcours de validation au projet existant.",
        )
        .expect("create linked request ticket");
        studio_append_ticket_event(
            dir.path(),
            &ticket_id,
            "studio_model_selected",
            "user_chat",
            Some(serde_json::json!({"provider":"ollama","model":"qwen3.5:9b"})),
        )
        .expect("persist selected model on the ticket timeline");

        let preference = studio_project_model_preference(dir.path())
            .expect("scheduled dispatch should load the selected model");
        assert_eq!(preference.provider, "ollama");
        assert_eq!(preference.model, "qwen3.5:9b");
    }

    #[test]
    fn chat_creation_request_creates_a_ticket_even_when_enforcement_is_off() {
        use super::{
            save_studio_meta, studio_create_evolution_ticket_from_chat, studio_get_ticket,
            studio_list_tickets, studio_seed_initial_backlog, studio_user_message_suggests_evolution,
            StudioMeta,
        };

        let dir = tempfile::tempdir().expect("temporary project");
        let meta = StudioMeta {
            id: "kanban-test".to_string(),
            name: "Kanban test".to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            evolutions: Vec::new(),
            tech_stack: None,
            verify_skip: false,
            verify_argv: None,
            verify_timeout_sec: None,
            evolution_summary: None,
            policy_notes: None,
            project_summary: None,
            ticket_enforcement_mode: Some("off".to_string()),
        };
        save_studio_meta(dir.path(), &meta).expect("save project metadata");

        let message = "je veux créer un jeu Bomberman jouable";
        assert!(studio_user_message_suggests_evolution(message));
        let id = studio_create_evolution_ticket_from_chat(dir.path(), message)
            .expect("create initial request ticket");
        let ticket = studio_get_ticket(dir.path(), &id).expect("ticket is visible on board");

        assert_eq!(ticket.status, "todo");
        assert_eq!(ticket.requested_by, "user_chat");
        assert_eq!(ticket.description, message);
        assert_eq!(ticket.acceptance_criteria.len(), 4);
        assert_eq!(
            ticket.acceptance_criteria.last().unwrap().id.as_deref(),
            Some("initial-kanban-backlog")
        );
        let tickets = studio_list_tickets(dir.path());
        assert_eq!(tickets.len(), 5, "global ticket plus four initial work phases");
        assert_eq!(
            tickets
                .iter()
                .filter(|ticket| ticket.requested_by == "system_initial_backlog")
                .count(),
            4
        );
        assert!(studio_seed_initial_backlog(dir.path(), &id)
            .expect("backfill is idempotent")
            .is_empty());
        let mut children: Vec<_> = studio_list_tickets(dir.path())
            .into_iter()
            .filter(|ticket| ticket.requested_by == "system_initial_backlog")
            .collect();
        for child in children.iter_mut() {
            child.status = "done".to_string();
            child.review_outcome = Some("approved".to_string());
            super::studio_upsert_ticket(dir.path(), child.clone()).expect("approve work ticket");
        }
        assert!(super::studio_complete_initial_backlog_if_ready(dir.path())
            .expect("check initial backlog")
            .is_empty());
        let specific_criteria = [
            "Le joueur se déplace dans une arène Bomberman et les murs bloquent son passage.",
            "Les bombes déclenchent des explosions qui détruisent les caisses atteintes.",
            "Les ennemis sont distincts et peuvent être éliminés par une explosion.",
            "Le build et le parcours navigateur confirment que le jeu est jouable.",
        ];
        for (index, child) in children.iter_mut().enumerate() {
            child.title = format!("Bomberman: phase de travail {index}");
            child.acceptance_criteria[0].id = Some(format!("bomberman-phase-{index}"));
            child.acceptance_criteria[0].text = specific_criteria[index].to_string();
            super::studio_upsert_ticket(dir.path(), child.clone())
                .expect("refine the initial work ticket");
        }
        let extra = super::studio_tool_create_ticket_json(
            dir.path(),
            &serde_json::json!({
                "title": "Add an enemy movement behavior",
                "acceptance_criteria": [{"id":"enemy-movement", "text":"Enemies move inside the arena and avoid walls."}]
            }),
        )
        .expect("create a task discovered during planning");
        assert!(super::studio_complete_initial_backlog_if_ready(dir.path())
            .expect("keep the initial request open while project work remains")
            .is_empty());
        let mut extra = extra;
        extra.status = "done".to_string();
        extra.review_outcome = Some("approved".to_string());
        super::studio_upsert_ticket(dir.path(), extra).expect("approve discovered task");
        assert_eq!(
            super::studio_complete_initial_backlog_if_ready(dir.path())
                .expect("complete initial request after every task is approved"),
            vec![id.clone()]
        );
        assert_eq!(
            studio_get_ticket(dir.path(), &id).unwrap().status,
            "done"
        );
    }

    #[test]
    fn completed_initial_summary_reopens_when_its_seeded_backlog_is_still_generic() {
        use super::{
            studio_create_evolution_ticket_from_chat, studio_get_ticket,
            studio_list_ticket_events, studio_upsert_ticket,
        };

        let dir = tempfile::tempdir().expect("temporary project");
        super::save_studio_meta(
            dir.path(),
            &super::StudioMeta {
                id: "reopen-generic-backlog".to_string(),
                name: "Reopen generic backlog".to_string(),
                created_at: chrono::Utc::now().to_rfc3339(),
                evolutions: Vec::new(),
                tech_stack: None,
                verify_skip: false,
                verify_argv: None,
                verify_timeout_sec: None,
                evolution_summary: None,
                policy_notes: None,
                project_summary: None,
                ticket_enforcement_mode: Some("off".to_string()),
            },
        )
        .expect("save project metadata");
        let id = studio_create_evolution_ticket_from_chat(
            dir.path(),
            "Créer une application Todo avec filtres et sauvegarde.",
        )
        .expect("create project request and generic backlog");
        let mut summary = studio_get_ticket(dir.path(), &id).expect("request summary");
        summary.status = "done".to_string();
        studio_upsert_ticket(dir.path(), summary).expect("simulate premature completion");

        assert!(super::studio_complete_initial_backlog_if_ready(dir.path())
            .expect("reconcile premature completion")
            .is_empty());
        assert_eq!(
            studio_get_ticket(dir.path(), &id).expect("reopened summary").status,
            "in_progress"
        );
        assert!(studio_list_ticket_events(dir.path(), &id)
            .iter()
            .any(|event| event.event_type == "initial_backlog_reopened"));
    }

    #[test]
    fn project_creation_form_request_seeds_kanban_before_the_agent_turn() {
        use super::{
            save_studio_meta, studio_create_evolution_ticket_from_chat, studio_get_ticket,
            studio_list_tickets, StudioMeta,
        };

        let dir = tempfile::tempdir().expect("temporary project");
        let summary = "Crée une webapp Todo en français, avec ajout, complétion, suppression, filtres et persistance locale. L'interface doit rester responsive et accessible au clavier.";
        save_studio_meta(
            dir.path(),
            &StudioMeta {
                id: "todo-first-request".to_string(),
                name: "Todo first request".to_string(),
                created_at: chrono::Utc::now().to_rfc3339(),
                evolutions: Vec::new(),
                tech_stack: Some("Vite + React + TypeScript strict; tests Vitest".to_string()),
                verify_skip: false,
                verify_argv: None,
                verify_timeout_sec: None,
                evolution_summary: Some(summary.to_string()),
                policy_notes: None,
                project_summary: Some(summary.to_string()),
                ticket_enforcement_mode: None,
            },
        )
        .expect("save project metadata");

        let request = format!("Crée dans ce projet une webapp Todo selon ce résumé :\n\n{summary}");
        let id = studio_create_evolution_ticket_from_chat(dir.path(), &request)
            .expect("create initial ticket and seed backlog");
        let root = studio_get_ticket(dir.path(), &id).expect("initial request ticket");
        assert!(root
            .acceptance_criteria
            .iter()
            .any(|criterion| criterion.id.as_deref() == Some("initial-kanban-backlog")));
        let tickets = studio_list_tickets(dir.path());
        assert_eq!(tickets.len(), 5, "root request plus four seeded work tickets");
        let (project, evolution, _policy, stack) =
            super::studio_meta_prefixes_for_request(dir.path(), &request);
        assert!(project.is_none(), "the summary is already in the submitted request");
        assert!(evolution.is_none(), "the evolution summary repeats the project brief");
        let stack = stack.expect("the separately saved stack is injected exactly once");
        assert!(stack.contains("Vite + React + TypeScript strict; tests Vitest"));
    }
}
