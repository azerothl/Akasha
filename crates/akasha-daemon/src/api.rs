//! Simple HTTP API: POST /api/message, GET /api/tasks/:id, GET / (health)

pub use crate::agent_profile::{
    get_or_load_agent_profile, new_agent_profile_cache, set_agent_profile_cache, AgentProfile,
    AgentProfileCache,
};
use crate::agents::{interpret_message, EventBus, OrchestratorTask, TaskPriority};
use crate::latency::{
    clear_task_milestones, emit_timeline_once_for_task, env_duration_ms, log_latency_metric,
    resolve_root_task_id,
};
use crate::memory::ShortTermStore;
use crate::memory_actor::LongTermMemoryClient;
use crate::protocol_adapter::unknown_external_message_count;
use crate::autonomous_mission_config::{AutonomousMissionConfig, MissionStatusYaml};
use crate::user_profile::UserProfile;
use akasha_core::{EventEnvelope, EventType};
use akasha_llm::CompletionRequest;
use akasha_plugin_api::{PluginKind, PluginManifest};
pub use akasha_store::tasks::MAX_PROGRESS_PER_TASK;
use akasha_store::{
    format_todos_plan_block, parse_todos_from_payload, Schedule,
    ScheduleException, ScheduleExceptionType, ScheduleStore, Task, TaskRunStatus, TaskStatus,
    TaskStore, TodoStatus, WorkspaceGraphStore,
};
use akasha_vault::Vault;
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub use crate::api_http::{json_response, parse_content_length, parse_request};
pub use crate::api_path_utils::{
    normalize_apostrophes, parse_read_file_args, strip_verbatim_prefix, READ_FILE_DEFAULT_MAX_LINES,
    READ_FILE_FULL_OUTPUT_MAX_BYTES, READ_FILE_PARTIAL_DEFAULT_MARKER,
};

/// Séparateur recommandé entre l’ancien et le nouveau texte (`TOOL:` est découpé sur les espaces, d’où un token `|` seul).
const SEARCH_REPLACE_DELIM: &str = " | ";

const BUDGET_SETTINGS_FILE: &str = "budget_settings.json";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct BudgetSettings {
    pub daily_token_limit: u64,
    pub warn_ratio: f64,
    pub auto_concise: bool,
}

impl Default for BudgetSettings {
    fn default() -> Self {
        Self {
            daily_token_limit: 1_000_000,
            warn_ratio: 0.7,
            auto_concise: true,
        }
    }
}

const SECOND_BRAIN_SETTINGS_FILE: &str = "memory_second_brain.json";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SecondBrainSettings {
    pub enabled: bool,
    pub paused: bool,
}

impl Default for SecondBrainSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            paused: false,
        }
    }
}

pub(crate) fn load_second_brain_settings(data_dir: &Path) -> SecondBrainSettings {
    let path = data_dir.join(SECOND_BRAIN_SETTINGS_FILE);
    let raw = match std::fs::read_to_string(path) {
        Ok(v) => v,
        Err(_) => return SecondBrainSettings::default(),
    };
    serde_json::from_str::<SecondBrainSettings>(&raw).unwrap_or_default()
}

pub(crate) fn save_second_brain_settings(data_dir: &Path, settings: &SecondBrainSettings) -> anyhow::Result<()> {
    let path = data_dir.join(SECOND_BRAIN_SETTINGS_FILE);
    std::fs::write(path, serde_json::to_string_pretty(settings)?)?;
    Ok(())
}

pub(crate) fn load_budget_settings(data_dir: &Path) -> BudgetSettings {
    let path = data_dir.join(BUDGET_SETTINGS_FILE);
    let raw = match std::fs::read_to_string(path) {
        Ok(v) => v,
        Err(_) => return BudgetSettings::default(),
    };
    serde_json::from_str::<BudgetSettings>(&raw).unwrap_or_default()
}

pub(crate) fn save_budget_settings(data_dir: &Path, settings: &BudgetSettings) -> anyhow::Result<()> {
    let path = data_dir.join(BUDGET_SETTINGS_FILE);
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, serde_json::to_string_pretty(settings)?.as_bytes())?;
    std::fs::rename(&tmp_path, &path)?;
    Ok(())
}

pub(crate) fn tool_scope_key(tool: &str, tool_args: &[String]) -> String {
    match tool {
        "run_command" | "run_terminal" | "run_command_background" => tool_args
            .iter()
            .take(2)
            .cloned()
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_string(),
        "write_file" | "write_code" => {
            // write_file / write_code may receive a JSON payload with a `path` key, or plain args[0]
            if let Some(path_from_json) = parse_write_file_request(tool_args)
                .map(|(p, _)| p)
                .filter(|p| !p.is_empty())
            {
                path_from_json
            } else {
                tool_args.get(0).cloned().unwrap_or_else(|| "global".to_string())
            }
        }
        "delete_file" => {
            // delete_file joins all args as a single path (spaces in filenames)
            let joined = tool_args.join(" ").trim().to_string();
            if joined.is_empty() { "global".to_string() } else { joined }
        }
        "edit_file" | "apply_patch" | "rename_path" | "move_tree" => {
            tool_args.get(0).cloned().unwrap_or_else(|| "global".to_string())
        }
        _ => "global".to_string(),
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub(crate) struct PermissionModeState {
    pub(crate) mode: String,
    pub(crate) updated_at: String,
}

impl Default for PermissionModeState {
    fn default() -> Self {
        Self {
            mode: "ask_me".to_string(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        }
    }
}

pub(crate) fn load_permission_mode(data_dir: &Path) -> PermissionModeState {
    let path = data_dir.join("permissions_mode.json");
    let raw = match std::fs::read_to_string(path) {
        Ok(v) => v,
        Err(_) => return PermissionModeState::default(),
    };
    serde_json::from_str::<PermissionModeState>(&raw).unwrap_or_default()
}

pub(crate) fn save_permission_mode(data_dir: &Path, state: &PermissionModeState) -> anyhow::Result<()> {
    let path = data_dir.join("permissions_mode.json");
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, serde_json::to_string_pretty(state)?.as_bytes())?;
    std::fs::rename(&tmp_path, &path)?;
    Ok(())
}

/// `args[0]` = chemin ; le reste = « ancien » puis ` | ` (recommandé) ou `|`, puis « nouveau ».
/// Retire les `|` initiaux issus du découpage (`path | old | new` → `old | new`).
pub(crate) fn parse_search_replace_payload(args: &[String]) -> Result<(String, String), &'static str> {
    let mut rest: String = args.get(1..).map(|a| a.join(" ")).unwrap_or_default();
    rest = rest.trim().to_string();
    while rest.starts_with('|') {
        rest = rest.trim_start_matches('|').trim_start().to_string();
    }
    if rest.is_empty() {
        return Err("usage");
    }
    let (search, replace) = if let Some((s, r)) = rest.split_once(SEARCH_REPLACE_DELIM) {
        (s.trim().to_string(), r.trim().to_string())
    } else if let Some((s, r)) = rest.split_once('|') {
        (s.trim().to_string(), r.trim().to_string())
    } else {
        return Err("usage");
    };
    if search.is_empty() {
        return Err("empty_search");
    }
    Ok((search, replace))
}

pub(crate) fn normalize_tool_path_hint(raw: &str) -> String {
    let mut s = raw
        .trim()
        .trim_matches('`')
        .trim_matches('"')
        .trim_matches('\'')
        .trim()
        .to_string();
    s = normalize_apostrophes(&s);
    if s.starts_with("workspace:/") {
        return format!(
            "workspace:/{}",
            s.trim_start_matches("workspace:/")
                .trim_start_matches(['/', '\\'])
        );
    }
    if let Some(rest) = s.strip_prefix("workspace:") {
        return format!("workspace:/{}", rest.trim_start_matches(['/', '\\']));
    }
    if let Some(rest) = s.strip_prefix("workspace/") {
        return format!("workspace:/{}", rest.trim_start_matches(['/', '\\']));
    }
    if let Some(rest) = s.strip_prefix("workspace\\") {
        return format!("workspace:/{}", rest.trim_start_matches(['/', '\\']));
    }
    s
}

pub(crate) fn is_workspace_virtual_path(raw: &str) -> bool {
    normalize_tool_path_hint(raw).starts_with("workspace:/")
}

/// True if `s` is `WxH` dimensions (e.g. 1024x1024).
pub(crate) fn looks_like_image_size_token(s: &str) -> bool {
    let s = s.trim();
    let mut parts = s.split('x');
    let (Some(w), Some(h)) = (parts.next(), parts.next()) else {
        return false;
    };
    if parts.next().is_some() {
        return false;
    }
    w.parse::<u32>().is_ok() && h.parse::<u32>().is_ok()
}

/// The tool layer splits `TOOL:` lines on whitespace, so multi-word prompts become many args.
/// Rejoin into a single prompt; treat a trailing `WxH` token as optional size.
pub(crate) fn parse_generate_image_tool_args(args: &[String]) -> (String, Option<String>) {
    if args.is_empty() {
        return (String::new(), None);
    }
    if args.len() >= 2 {
        if let Some(last) = args.last() {
            if looks_like_image_size_token(last) {
                return (
                    args[..args.len() - 1].join(" "),
                    Some(last.trim().to_string()),
                );
            }
        }
    }
    (args.join(" "), None)
}

/// Code Studio prepends retrieved index chunks to the user message unless explicitly disabled.
/// `AKASHA_STUDIO_CODE_RAG_DISABLED=1|true|yes|on` turns that prefix off; unset or other values keep RAG on.
pub(crate) fn studio_code_rag_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("AKASHA_STUDIO_CODE_RAG_DISABLED")
            .ok()
            .map(|v| {
                let t = v.trim().to_ascii_lowercase();
                if t.is_empty() {
                    return true;
                }
                !matches!(t.as_str(), "1" | "true" | "yes" | "on")
            })
            .unwrap_or(true)
    })
}

pub(crate) fn schedule_code_studio_index_for_root(store_path: &Path, tool_disk_workspace_root: &Path) {
    let data_dir = store_path.parent().unwrap_or_else(|| store_path);
    let Some(project_id) =
        crate::studio::studio_project_id_from_disk_root(data_dir, tool_disk_workspace_root)
    else {
        return;
    };
    crate::api_studio::schedule_studio_code_rag_index(
        data_dir,
        &project_id,
        tool_disk_workspace_root,
        false,
    );
}

pub(crate) fn debug_log(hypothesis_id: &str, location: &str, message: &str, data: serde_json::Value) {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let enabled = *ENABLED.get_or_init(|| {
        std::env::var_os("AKASHA_DEBUG_LOG")
            .map(|v| {
                let v = v.to_string_lossy().to_ascii_lowercase();
                matches!(v.as_str(), "1" | "true" | "yes" | "on")
            })
            .unwrap_or(false)
    });
    if !enabled {
        return;
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let run_id =
        std::env::var("AKASHA_DEBUG_RUN_ID").unwrap_or_else(|_| "pre-fix".to_string());
    tracing::debug!(
        run_id = %run_id,
        hypothesis_id = hypothesis_id,
        location = location,
        message = message,
        timestamp = timestamp,
        data = %data,
        "api debug log"
    );
}

pub(crate) fn parse_write_file_request(args: &[String]) -> Option<(String, String)> {
    if args.is_empty() {
        return None;
    }

    let joined = args.join("\n");
    let trimmed = joined.trim();
    if trimmed.starts_with('{') {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
            let path = value
                .get("path")
                .or_else(|| value.get("filePath"))
                .or_else(|| value.get("file"))
                .and_then(|v| v.as_str())
                .map(normalize_tool_path_hint);
            let content = value
                .get("content")
                .or_else(|| value.get("text"))
                .or_else(|| value.get("body"))
                .or_else(|| value.get("data"))
                .map(|v| match v {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Array(items) => items
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_string))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    other => {
                        serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string())
                    }
                });
            if let Some(path) = path {
                return Some((path, content.unwrap_or_default()));
            }
        }
    }

    let path = normalize_tool_path_hint(args.first()?.as_str());
    let content_args = args.get(1..).unwrap_or(&[]);
    let content = match content_args {
        [] => String::new(),
        [only] => only.clone(),
        many => {
            // `TOOL:` headers are split on whitespace. Some models put file content on the
            // same line as the `write_file` header (`TOOL: write_file path { "x": ... }`)
            // and may continue it on following lines. The multiline parser always places the
            // collected body as the last element (including single-line bodies), so always
            // join the header prefix and the last element with a newline to preserve line
            // breaks between inline header content and any collected body.
            let (last, prefix) = many.split_last().expect("non-empty by match arm");
            let header = prefix.join(" ");
            if header.trim().is_empty() {
                last.clone()
            } else {
                format!("{}\n{}", header, last)
            }
        }
    };
    Some((path, content))
}

/// If the model wrapped the entire `write_file` body in a markdown fence, strip one layer (repeat up to 3×).
pub(crate) fn strip_markdown_fences_from_write_content(content: &str) -> String {
    let mut s = content.to_string();
    for _ in 0..3 {
        let lead = s.trim_start();
        if !lead.starts_with("```") {
            break;
        }
        s = if let Some(i) = s.find('\n') {
            s[i + 1..].to_string()
        } else {
            String::new()
        };
        let te = s.trim_end();
        if te.ends_with("```") {
            if let Some(i) = te.rfind('\n') {
                let last = te[i + 1..].trim();
                if last == "```" {
                    s = te[..i].to_string();
                    continue;
                }
            } else if te.trim() == "```" {
                s.clear();
                break;
            }
        }
        break;
    }
    s
}

/// Relative path for [`crate::api_studio::path_has_agent_code_extension`] (strips `workspace:` prefix).
pub(crate) fn path_for_agent_write_extension_check(path_str: &str) -> PathBuf {
    let normalized = normalize_tool_path_hint(path_str.trim());
    let rel = normalized
        .strip_prefix("workspace:/")
        .or_else(|| normalized.strip_prefix("workspace:"))
        .map(|s| s.trim_start_matches(|c| c == '/' || c == '\\'))
        .unwrap_or(normalized.as_str());
    PathBuf::from(rel)
}

pub(crate) fn policy_allows_primary_disk_write(policy: &akasha_tools::ToolsPolicy) -> bool {
    policy.can_use_tool("write_file")
        || policy.can_use_tool("write_code")
        || policy.can_use_tool("edit_file")
        || policy.can_use_tool("search_replace")
        || policy.can_use_tool("apply_patch")
}

/// Parse `memory_store` optional `link_to:` / `link_kind:` into `(target_uuid, relation_kind)` pairs.
/// Supports `link_to: uuid1+excludes,uuid2+relates_to` (per-target kind after `+`) or
/// `link_to: uuid1,uuid2` with `link_kind: relates_to` (one kind for all UUIDs; default `related`).
/// Repeated `link_to:` arguments append segments.
pub(crate) fn parse_memory_store_explicit_links(extra_args: &[String]) -> Option<Vec<(String, String)>> {
    let mut segments: Vec<String> = Vec::new();
    let mut global_kind: Option<String> = None;
    for arg in extra_args {
        if let Some(rest) = arg.strip_prefix("link_to:") {
            for part in rest.split(',') {
                let p = part.trim();
                if !p.is_empty() {
                    segments.push(p.to_string());
                }
            }
        } else if let Some(rest) = arg.strip_prefix("link_kind:") {
            let k = rest.trim();
            if !k.is_empty() {
                global_kind = Some(k.to_string());
            }
        }
    }
    if segments.is_empty() {
        return None;
    }
    let default_kind = global_kind.unwrap_or_else(|| "related".to_string());
    let mut out: Vec<(String, String)> = Vec::new();
    for seg in segments {
        let seg = seg.trim();
        if let Some(idx) = seg.rfind('+') {
            let uuid_part = seg[..idx].trim();
            let kind_part = seg[idx + 1..].trim();
            if Uuid::parse_str(uuid_part).is_ok() && !kind_part.is_empty() {
                out.push((uuid_part.to_string(), kind_part.to_string()));
                continue;
            }
        }
        if Uuid::parse_str(seg).is_ok() {
            out.push((seg.to_string(), default_kind.clone()));
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

pub(crate) enum PluginReputationResetBody {
    Empty,
    Parsed(serde_json::Value),
    Invalid(String),
}

pub(crate) fn parse_plugin_reputation_reset_body(body: Option<&[u8]>) -> PluginReputationResetBody {
    match body {
        Some(raw) if !raw.is_empty() => match serde_json::from_slice::<serde_json::Value>(raw) {
            Ok(v) => PluginReputationResetBody::Parsed(v),
            Err(e) => PluginReputationResetBody::Invalid(e.to_string()),
        },
        _ => PluginReputationResetBody::Empty,
    }
}

pub(crate) fn canonicalize_tool_name(tool_name: &str) -> String {
    match tool_name.to_lowercase().as_str() {
        "create_todos" => "write_todos".to_string(),
        "append_todos" => "merge_todos".to_string(),
        _ => tool_name.to_string(),
    }
}

/// Strips `--no-ignore`, `--no-gitignore`, `--regex`, `-r` from tool args (any position).
/// Returns filtered args, `respect_gitignore` (default true), `use_regex` (default false; grep only).
pub(crate) fn strip_file_search_flags(args: &[String]) -> (Vec<String>, bool, bool) {
    let mut respect_gitignore = true;
    let mut use_regex = false;
    let mut out = Vec::new();
    for a in args {
        match a.as_str() {
            "--no-ignore" | "--no-gitignore" => respect_gitignore = false,
            "--regex" | "-r" => use_regex = true,
            _ => out.push(a.clone()),
        }
    }
    (out, respect_gitignore, use_regex)
}

/// Banner injected by `compose_orchestrated_child_message(..., deliverables_required: true)` for subtasks and remediation.
pub(crate) const ORCH_DISK_DELIVERABLES_MARKER: &str = "[Orchestrated — disk deliverables REQUIRED]";

/// Resolve `workspace:/rel` or a normal filesystem path to a concrete disk path for tools that only call `read_dir` / globs on real paths.
/// True if path should be read as PDF (text extraction), not as UTF-8/plain text.
pub(crate) fn path_extension_is_pdf(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

/// Read bytes from disk and return the same user-visible message shape as `pdf` tool.
pub(crate) async fn pdf_extract_message_from_disk(
    disk_path: &Path,
    via_tool: &str,
) -> (bool, String, Option<String>) {
    match tokio::fs::read(disk_path).await {
        Ok(bytes) => match pdf_extract::extract_text_from_mem(&bytes) {
            Ok(text) => {
                let preview = if text.len() > 2000 {
                    format!("{}…", text.chars().take(2000).collect::<String>())
                } else {
                    text.clone()
                };
                (
                    true,
                    format!(
                        "[{}; PDF→text {}] extracted {} chars:\n{}",
                        via_tool,
                        disk_path.display(),
                        text.len(),
                        preview
                    ),
                    None,
                )
            }
            Err(e) => (
                false,
                format!(
                    "[{}] PDF extraction failed for {}: {}",
                    via_tool,
                    disk_path.display(),
                    e
                ),
                None,
            ),
        },
        Err(e) => (
            false,
            format!(
                "[{}] read failed for {}: {}",
                via_tool,
                disk_path.display(),
                e
            ),
            None,
        ),
    }
}

pub(crate) fn resolve_tool_disk_path(raw: &str, workspace_root: Option<&Path>) -> PathBuf {
    let raw = normalize_tool_path_hint(raw);
    let raw = raw.trim();
    if raw.starts_with("workspace:/") || raw.starts_with("workspace:") {
        let key = raw
            .trim_start_matches("workspace:/")
            .trim_start_matches("workspace:")
            .trim_start_matches('/');
        let key = normalize_apostrophes(key);
        strip_verbatim_prefix(
            workspace_root
                .map(|root| root.join(&key))
                .or_else(|| std::env::current_dir().ok().map(|cwd| cwd.join(&key)))
                .unwrap_or_else(|| Path::new(&key).to_path_buf()),
        )
    } else {
        let p = Path::new(raw);
        if p.is_absolute() {
            strip_verbatim_prefix(p.to_path_buf())
        } else {
            strip_verbatim_prefix(
                workspace_root
                    .map(|root| root.join(p))
                    .or_else(|| std::env::current_dir().ok().map(|cwd| cwd.join(p)))
                    .unwrap_or_else(|| p.to_path_buf()),
            )
        }
    }
}

use std::collections::{BinaryHeap, VecDeque};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::sync::RwLock;
use tracing::Instrument;
use uuid::Uuid;

/// Virtual workspace per task (Deep Agents-style). Paths prefixed with "workspace:/" or "workspace:" are read/written here instead of disk.
pub type TaskWorkspaceStore =
    Arc<RwLock<std::collections::HashMap<Uuid, std::collections::HashMap<String, String>>>>;

pub fn new_task_workspace_store() -> TaskWorkspaceStore {
    Arc::new(RwLock::new(std::collections::HashMap::new()))
}

/// Cached result of fetching api/latest.json from the Akasha_app site (version, download_url, etc.).
#[derive(Default, Clone)]
pub struct UpdateStatus {
    pub remote_version: String,
    pub download_url: String,
    pub release_notes_url: Option<String>,
    pub last_checked_at: Option<chrono::DateTime<chrono::Utc>>,
    pub error: Option<String>,
}

pub type UpdateCheckCache = Arc<RwLock<UpdateStatus>>;

pub fn new_update_check_cache() -> UpdateCheckCache {
    Arc::new(RwLock::new(UpdateStatus::default()))
}

/// Fetch {base}/api/latest.json and update the cache. Does not crash on network/parse errors.
pub async fn run_update_check_once(cache: &UpdateCheckCache, base_url: &str) {
    let base = base_url.trim_end_matches('/');
    let url = format!("{}/api/latest.json", base);
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            let mut g = cache.write().await;
            g.error = Some(e.to_string());
            g.last_checked_at = Some(chrono::Utc::now());
            return;
        }
    };
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            let mut g = cache.write().await;
            g.error = Some(e.to_string());
            g.last_checked_at = Some(chrono::Utc::now());
            return;
        }
    };
    if !resp.status().is_success() {
        let mut g = cache.write().await;
        g.error = Some(format!("HTTP {}", resp.status()));
        g.last_checked_at = Some(chrono::Utc::now());
        return;
    }
    let data: serde_json::Value = match resp.json().await {
        Ok(d) => d,
        Err(e) => {
            let mut g = cache.write().await;
            g.error = Some(e.to_string());
            g.last_checked_at = Some(chrono::Utc::now());
            return;
        }
    };
    let mut g = cache.write().await;
    g.remote_version = data
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("0.0.0")
        .to_string();
    g.download_url = data
        .get("download_url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    g.release_notes_url = data
        .get("release_notes_url")
        .and_then(|v| v.as_str())
        .map(String::from);
    g.last_checked_at = Some(chrono::Utc::now());
    g.error = None;
}

/// Request for a sub-agent delegation (from a worker to the orchestrator). Reply is sent on reply_tx.
pub struct DelegationRequest {
    pub requesting_task_id: Uuid,
    pub agent_type: String,
    pub message: String,
    pub reply_tx: oneshot::Sender<Result<String, String>>,
}

/// Limits concurrent background LLM fact-extraction tasks to prevent unbounded queue growth under load.
static EXTRACT_SEMAPHORE: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> =
    std::sync::OnceLock::new();

pub(crate) fn extract_semaphore() -> Arc<tokio::sync::Semaphore> {
    EXTRACT_SEMAPHORE
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(2)))
        .clone()
}


pub const MAX_EVENTS_PER_TASK: usize = 64;

#[derive(Clone, serde::Serialize)]
pub struct ProgressEntry {
    pub progress_pct: u8,
    pub message: String,
    /// Tâche à laquelle cette ligne se rapporte (pour dédoublonnage côté UI).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

pub type ProgressCache = Arc<RwLock<std::collections::HashMap<Uuid, VecDeque<ProgressEntry>>>>;

#[derive(Clone, serde::Serialize)]
pub struct TaskEventEntry {
    /// Contract version for client event envelopes.
    pub schema_version: u8,
    /// Normalized event kind for clients (kept in sync with event_type).
    pub kind: String,
    pub event_type: String,
    pub payload: Option<serde_json::Value>,
    pub at: String,
    /// When present, indicates which task this event belongs to (for merged root+child responses).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

pub type EventsCache = Arc<RwLock<std::collections::HashMap<Uuid, VecDeque<TaskEventEntry>>>>;


pub fn new_progress_cache() -> ProgressCache {
    Arc::new(RwLock::new(std::collections::HashMap::new()))
}

pub fn new_events_cache() -> EventsCache {
    Arc::new(RwLock::new(std::collections::HashMap::new()))
}

/// Per-session result cell for background commands. The spawned task writes the result when done.
pub(crate) type BackgroundResultCell =
    Arc<RwLock<Option<anyhow::Result<(std::process::Output, akasha_tools::ToolResult)>>>>;

/// Registry of background command sessions: session_id -> (task handle to abort, result cell).
pub type ProcessRegistry = Arc<
    RwLock<std::collections::HashMap<Uuid, (tokio::task::JoinHandle<()>, BackgroundResultCell)>>,
>;

pub fn new_process_registry() -> ProcessRegistry {
    Arc::new(RwLock::new(std::collections::HashMap::new()))
}

/// Registry of task-completion notifiers: task_id → Notify. Written by the conversation worker
/// on task completion; awaited by run_delegation_handler and the orchestrator aggregator so they
/// can react immediately instead of polling TaskStore every 500 ms.
pub type TaskCompletionRegistry =
    Arc<RwLock<std::collections::HashMap<Uuid, Arc<tokio::sync::Notify>>>>;

/// Last LLM call stats for a task turn (GET /api/tasks/:id, task_completed events).
#[derive(Clone, Copy, Debug, Default)]
pub struct LastTurnUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cost_usd: f64,
    pub latency_ms: u64,
}

/// Per-task and per-session LLM usage (tokens, cost USD) for GET /api/tasks/:id and cost visibility.
#[derive(Default)]
pub struct TaskUsageStore {
    by_task: RwLock<std::collections::HashMap<Uuid, (u64, f64)>>,
    by_session: RwLock<std::collections::HashMap<String, (u64, f64)>>,
    by_task_last_turn: RwLock<std::collections::HashMap<Uuid, LastTurnUsage>>,
    by_task_last_model: RwLock<std::collections::HashMap<Uuid, String>>,
}

impl TaskUsageStore {
    pub fn new() -> Self {
        Self::default()
    }
    pub async fn add(
        &self,
        task_id: Uuid,
        session_id: &str,
        prompt_tokens: u64,
        completion_tokens: u64,
        cost_usd: f64,
        latency_ms: u64,
        model_used: Option<&str>,
    ) {
        let tokens = prompt_tokens.saturating_add(completion_tokens);
        {
            let mut g = self.by_task.write().await;
            let e = g.entry(task_id).or_insert((0, 0.0));
            e.0 += tokens;
            e.1 += cost_usd;
        }
        {
            let mut g = self.by_task_last_turn.write().await;
            g.insert(
                task_id,
                LastTurnUsage {
                    prompt_tokens,
                    completion_tokens,
                    cost_usd,
                    latency_ms,
                },
            );
        }
        if let Some(model) = model_used.filter(|m| !m.is_empty()) {
            self.by_task_last_model
                .write()
                .await
                .insert(task_id, model.to_string());
        }
        if !session_id.is_empty() {
            let mut g = self.by_session.write().await;
            let e = g.entry(session_id.to_string()).or_insert((0, 0.0));
            e.0 += tokens;
            e.1 += cost_usd;
        }
    }
    pub async fn get_task(&self, task_id: Uuid) -> Option<(u64, f64)> {
        self.by_task.read().await.get(&task_id).copied()
    }
    pub async fn get_session(&self, session_id: &str) -> Option<(u64, f64)> {
        self.by_session.read().await.get(session_id).copied()
    }
    pub async fn get_last_turn(&self, task_id: Uuid) -> Option<LastTurnUsage> {
        self.by_task_last_turn.read().await.get(&task_id).copied()
    }
    pub async fn get_last_model(&self, task_id: Uuid) -> Option<String> {
        self.by_task_last_model
            .read()
            .await
            .get(&task_id)
            .cloned()
    }
    pub async fn reset_session(&self, session_id: &str) {
        self.by_session.write().await.remove(session_id);
    }
    pub async fn totals(&self) -> (u64, f64) {
        let g = self.by_session.read().await;
        g.values().fold((0u64, 0.0f64), |acc, v| (acc.0 + v.0, acc.1 + v.1))
    }
}

pub fn new_task_completion_registry() -> TaskCompletionRegistry {
    Arc::new(RwLock::new(std::collections::HashMap::new()))
}

/// Runs in a loop: receives DelegationRequest, checks depth (root or direct child only), creates child task, sends to conv_tx, waits for child completion, sends reply on oneshot.
/// `delegation_sem`: semaphore for backpressure; when full, replies with "système surchargé".
pub async fn run_delegation_handler(
    mut delegation_rx: mpsc::Receiver<DelegationRequest>,
    conv_tx: mpsc::Sender<OrchestratorTask>,
    store_path: PathBuf,
    bus: EventBus,
    progress: ProgressCache,
    task_completion: TaskCompletionRegistry,
    delegation_sem: std::sync::Arc<tokio::sync::Semaphore>,
    _studio_disk_registry: crate::studio::StudioDiskRootRegistry,
    studio_worktree_registry: crate::studio_worktree::StudioWorktreeRegistry,
) {
    while let Some(req) = delegation_rx.recv().await {
        let permit = match delegation_sem.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                tracing::warn!("Delegation backpressure: max concurrent delegations reached");
                let _ = req
                    .reply_tx
                    .send(Err("Système surchargé, réessayez plus tard.".to_string()));
                continue;
            }
        };
        let span_guard = tracing::info_span!(
            "delegation",
            requesting_task_id = %req.requesting_task_id,
            agent_type = %req.agent_type
        )
        .entered();
        let store = match TaskStore::open(&store_path) {
            Ok(s) => s,
            Err(e) => {
                let _ = req.reply_tx.send(Err(format!("store open: {}", e)));
                continue;
            }
        };
        let requesting = match store.get(req.requesting_task_id) {
            Ok(Some(t)) => t,
            Ok(None) => {
                let _ = req
                    .reply_tx
                    .send(Err("requesting task not found".to_string()));
                continue;
            }
            Err(e) => {
                let _ = req.reply_tx.send(Err(format!("store: {}", e)));
                continue;
            }
        };
        if let Some(parent_id) = requesting.parent_task_id {
            if let Ok(Some(parent)) = store.get(parent_id) {
                if parent.parent_task_id.is_some() {
                    let _ = req.reply_tx.send(Err(
                        "max delegation depth (sous-sous-agent non autorisé)".to_string(),
                    ));
                    continue;
                }
            }
        }
        // Build a delegation message that always includes the root user request context,
        // so child agents don't lose intent when the delegating LLM emits a terse/ambiguous subtask.
        let root_user_request = {
            let mut current = requesting.clone();
            let mut depth = 0usize;
            let mut root = current.initial_message.clone().unwrap_or_default();
            while let Some(pid) = current.parent_task_id {
                if depth >= 8 {
                    break;
                }
                match store.get(pid) {
                    Ok(Some(parent)) => {
                        if let Some(msg) = parent.initial_message.clone() {
                            if !msg.trim().is_empty() {
                                root = msg;
                            }
                        }
                        current = parent;
                        depth += 1;
                    }
                    _ => break,
                }
            }
            root
        };
        let delegated_message = if req.message.trim_start().starts_with("[Task]\n") {
            req.message.clone()
        } else {
            let root_trim = root_user_request.trim();
            if root_trim.is_empty() {
                req.message.clone()
            } else {
                format!(
                    "[Parent user request]\n{}\n\n[Delegated subtask]\n{}",
                    root_trim,
                    req.message.trim()
                )
            }
        };
        let child_id = Uuid::new_v4();
        let agent_type = if crate::agents::is_specialist_agent(&req.agent_type) {
            req.agent_type.trim().to_lowercase()
        } else {
            "conversation".to_string()
        };
        const MAX_INITIAL_MSG: usize = 500;
        let initial_message = if delegated_message.chars().count() > MAX_INITIAL_MSG {
            Some(
                delegated_message
                    .chars()
                    .take(MAX_INITIAL_MSG)
                    .chain(std::iter::once('…'))
                    .collect::<String>(),
            )
        } else if delegated_message.is_empty() {
            None
        } else {
            Some(delegated_message.clone())
        };
        let studio_project_id = store
            .lineage_root_studio_project_id(req.requesting_task_id)
            .ok()
            .flatten();
        let child_task = Task {
            id: child_id,
            parent_task_id: Some(req.requesting_task_id),
            status: TaskStatus::Pending,
            assigned_agent: agent_type.clone(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            initial_message,
            studio_project_id,
        };
        if store.insert(&child_task).is_err() {
            let _ = req.reply_tx.send(Err("store insert failed".to_string()));
            continue;
        }
        // EnteredSpan is not Send and must not cross await points.
        drop(span_guard);
        let lineage_root = store
            .lineage_root_task_id(req.requesting_task_id)
            .ok()
            .unwrap_or(req.requesting_task_id);
        if crate::studio_worktree::worktree_feature_enabled() {
            if let Some(ref project_id) = child_task.studio_project_id {
                if let Some(data_dir) = store_path.parent() {
                    match crate::studio_worktree::create_worktree_for_child_task(
                        data_dir,
                        project_id,
                        lineage_root,
                        child_id,
                    )
                    .await
                    {
                        Ok(wt) => {
                            crate::studio_worktree::register_worktree(&studio_worktree_registry, wt.clone())
                                .await;
                            let worktree_path = wt.worktree_path.to_string_lossy().into_owned();
                            let _ = bus.send(
                                EventEnvelope::new(
                                    EventType::ProgressUpdate,
                                    Some(serde_json::json!({
                                        "task_id": child_id.to_string(),
                                        "event_type": "studio_worktree_created",
                                        "worktree_branch": wt.worktree_branch,
                                        "worktree_path": worktree_path,
                                    })),
                                )
                                .with_correlation(req.requesting_task_id),
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                child_task_id = %child_id,
                                error = %e,
                                "studio worktree creation failed; fallback to shared root"
                            );
                        }
                    }
                }
            }
        }
        let _ = bus.send(
            EventEnvelope::new(
                EventType::SubAgentSpawned,
                Some(serde_json::json!({
                    "task_id": child_id.to_string(),
                    "parent_id": req.requesting_task_id.to_string(),
                    "agent": agent_type,
                    "delegation_reason": "delegate_to_agent"
                })),
            )
            .with_correlation(req.requesting_task_id),
        );
        let _ = bus.send(
            EventEnvelope::new(
                EventType::SubtaskStarted,
                Some(serde_json::json!({
                    "schema_version": 1,
                    "root_task_id": req.requesting_task_id.to_string(),
                    "subtask_id": child_id.to_string(),
                    "agent_type": agent_type,
                    "source": "delegate_to_agent",
                })),
            )
            .with_correlation(req.requesting_task_id),
        );
        // Register a completion notifier *before* sending to conv_tx so the worker can notify
        // even if it completes before the spawned waiter calls notified().
        let notify = Arc::new(tokio::sync::Notify::new());
        {
            let mut reg = task_completion.write().await;
            reg.insert(child_id, notify.clone());
        }
        if conv_tx
            .send(OrchestratorTask {
                task_id: child_id,
                message: delegated_message,
                session_id: String::new(),
                image_data_urls: None,
                execution_mode: None,
                preferred_task_type: None,
                incognito: false,
            })
            .await
            .is_err()
        {
            task_completion.write().await.remove(&child_id);
            let _ = req.reply_tx.send(Err("conv_tx closed".to_string()));
            continue;
        }
        let reply_tx = req.reply_tx;
        let store_path = store_path.clone();
        let progress = progress.clone();
        let task_completion = task_completion.clone();
        let child_id_span = child_id;
        let parent_task_id_span = req.requesting_task_id;
        let agent_type_span = agent_type.clone();
        let bus_for_waiter = bus.clone();
        let studio_worktree_registry = studio_worktree_registry.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let span = tracing::info_span!(
                "delegation_wait",
                child_task_id = %child_id_span,
                assigned_agent = %agent_type_span
            );
            let first_activity_timeout =
                env_duration_ms("AKASHA_DELEGATION_FIRST_ACTIVITY_TIMEOUT_MS", 15_000);
            let completion_timeout =
                env_duration_ms("AKASHA_DELEGATION_COMPLETION_TIMEOUT_MS", 300_000);
            let had_first_activity =
                wait_for_task_activity(&progress, child_id_span, first_activity_timeout).await;
            if !had_first_activity {
                task_completion.write().await.remove(&child_id_span);
                let _ = reply_tx.send(Err(format!(
                    "delegation startup timeout ({} ms without visible activity)",
                    first_activity_timeout.as_millis()
                )));
                return;
            }
            let timed_out =
                tokio::time::timeout(completion_timeout, notify.notified().instrument(span))
                    .await
                    .is_err();
            // Ensure the registry entry is removed regardless of outcome.
            task_completion.write().await.remove(&child_id_span);
            if timed_out {
                let _ = reply_tx.send(Err(format!(
                    "delegation completion timeout ({} ms)",
                    completion_timeout.as_millis()
                )));
                return;
            }
            // Single store read to retrieve the final task status and result message.
            let store = match TaskStore::open(&store_path) {
                Ok(s) => s,
                Err(e) => {
                    let _ = reply_tx.send(Err(format!("TaskStore open failed: {}", e)));
                    return;
                }
            };
            let task = match store.get(child_id_span) {
                Ok(Some(t)) => t,
                _ => {
                    let _ = reply_tx.send(Err("child task not found in store".to_string()));
                    return;
                }
            };
            let msg = {
                let g = progress.read().await;
                g.get(&child_id_span)
                    .and_then(|q| q.back())
                    .map(|e| e.message.clone())
                    .unwrap_or_else(|| {
                        if matches!(task.status, TaskStatus::Failed) {
                            "Échec.".to_string()
                        } else {
                            "Terminé.".to_string()
                        }
                    })
            };
            if crate::studio_worktree::worktree_feature_enabled() {
                if let Some(wt) = crate::studio_worktree::complete_and_cleanup_worktree(
                    &studio_worktree_registry,
                    child_id_span,
                )
                .await
                {
                    let event_type = match wt.lifecycle_state {
                        crate::studio_worktree::WorktreeLifecycleState::NeedsUserResolution => {
                            "studio_worktree_integration_conflict"
                        }
                        crate::studio_worktree::WorktreeLifecycleState::Cleaned => {
                            "studio_worktree_integration_cleaned"
                        }
                        crate::studio_worktree::WorktreeLifecycleState::Integrated => {
                            "studio_worktree_integration_merged"
                        }
                        crate::studio_worktree::WorktreeLifecycleState::Failed => {
                            "studio_worktree_integration_failed"
                        }
                        _ => "studio_worktree_integration_state",
                    };
                    let worktree_path = wt.worktree_path.to_string_lossy().into_owned();
                    let _ = bus_for_waiter.send(
                        EventEnvelope::new(
                            EventType::ProgressUpdate,
                            Some(serde_json::json!({
                                "task_id": child_id_span.to_string(),
                                "event_type": event_type,
                                "integration_status": wt.integration_status,
                                "conflict_state": wt.conflict_state,
                                "worktree_branch": wt.worktree_branch,
                                "worktree_path": worktree_path,
                            })),
                        )
                        .with_correlation(parent_task_id_span),
                    );
                }
            }
            let _ = reply_tx.send(match task.status {
                TaskStatus::Completed => {
                    let _ = bus_for_waiter.send(
                        EventEnvelope::new(
                            EventType::SubtaskCompleted,
                            Some(serde_json::json!({
                                "schema_version": 1,
                                "root_task_id": parent_task_id_span.to_string(),
                                "subtask_id": child_id_span.to_string(),
                                "agent_type": agent_type_span,
                                "status": "completed",
                                "content_preview": msg.chars().take(600).collect::<String>(),
                                "source": "delegate_to_agent",
                            })),
                        )
                        .with_correlation(parent_task_id_span),
                    );
                    Ok(msg)
                }
                TaskStatus::Failed => {
                    let _ = bus_for_waiter.send(
                        EventEnvelope::new(
                            EventType::SubtaskCompleted,
                            Some(serde_json::json!({
                                "schema_version": 1,
                                "root_task_id": parent_task_id_span.to_string(),
                                "subtask_id": child_id_span.to_string(),
                                "agent_type": agent_type_span,
                                "status": "failed",
                                "content_preview": msg.chars().take(600).collect::<String>(),
                                "source": "delegate_to_agent",
                            })),
                        )
                        .with_correlation(parent_task_id_span),
                    );
                    Err(msg)
                }
                _ => Err("child task did not complete successfully".to_string()),
            });
        });
    }
}

/// Pending "human in the loop" request: agent is waiting for the user to answer.
pub struct PendingHumanInput {
    pub question: String,
    pub context: String,
    pub choices: Option<Vec<String>>,
    pub response_tx: tokio::sync::oneshot::Sender<String>,
}

/// Store of pending human-input requests by task_id. Used by the conversation worker (to register and wait) and by the API (to return question/context/choices and to submit the reply).
pub type HumanInputStore = Arc<RwLock<std::collections::HashMap<Uuid, PendingHumanInput>>>;

pub fn new_human_input_store() -> HumanInputStore {
    Arc::new(RwLock::new(std::collections::HashMap::new()))
}

pub type SteeringQueueStore = crate::steering_queue::SteeringQueueStore;

pub fn new_steering_queue_store() -> SteeringQueueStore {
    SteeringQueueStore::new()
}


/// Liste des outils disponibles (source unique pour le prompt et la doc).
/// Format: une ligne par outil "nom — usage".
/// Note: "Session terminal" (spec 33) est optionnel et prévu pour une version ultérieure.
pub const AVAILABLE_TOOLS: &[(&str, &str)] = &[
    ("read_file", "read_file <path> [--full] [<offset_ligne> <nb_lignes>] — lire un fichier texte. Par défaut : **500 premières lignes** seulement (évite de saturer le contexte). `TOOL: read_file <chemin> --full` pour tout le fichier (plafond octets côté daemon si très gros). Fenêtre explicite : `read_file workspace:/fichier.ts 1 200`. PDF : texte extrait automatiquement. Path réel ou workspace:/<path>."),
    ("write_file", "write_file <path> puis contenu sur les lignes suivantes — écrire un fichier complet (création/remplacement). Format préféré : première ligne `TOOL: write_file workspace:/fichier`, puis le corps du fichier seul sur les lignes suivantes. Ne pas compresser un fichier entier sur la même ligne que le header. Préférer workspace:/<fichier> si l'utilisateur n'a pas donné de chemin. Si le fichier existe déjà et qu'il faut modifier une partie, préférer edit_file ou search_replace."),
    ("write_code", "write_code <path> puis contenu — comme write_file mais **uniquement** pour fichiers source (.ts, .tsx, .js, .jsx, .rs, .py, …). Le daemon rejette markdown parasite / lignes TOOL dans le corps. Pour CODE_STUDIO_PLAN.md, DESIGN.md, JSON, YAML : utiliser write_file."),
    ("delete_file", "delete_file <path> — supprimer un fichier (pas un répertoire). Chemin workspace:/ ou disque autorisé par tools_policy (mêmes règles que write_file). Code Studio : préférer workspace:/chemin/relatif."),
    ("rename_path", "rename_path <from> <to> — renommer ou déplacer un fichier ou un répertoire (rename atomique si possible ; copie+suppression pour un fichier en cross-device). La destination ne doit pas exister. Deux arguments : le chemin source est le premier token ; tout le reste forme le chemin cible (espaces dans <to> OK). Pas d’espaces dans <from> sans utiliser workspace:/…"),
    ("move_tree", "move_tree <from_dir> <to_dir> — déplacer un répertoire et son contenu (rename atomique si possible, sinon copie récursive + suppression). La destination ne doit pas exister. Même convention d’arguments que rename_path (cible = args après le premier token)."),
    ("search_files", "search_files <dir> <pattern> [--no-ignore] — chercher des fichiers (glob) sous un répertoire ; par défaut respecte .gitignore et ignore node_modules/target/dist/… ; --no-ignore pour tout parcourir."),
    ("grep_content", "grep_content <dir> <pattern> [file_glob] [--regex|-r] [--no-ignore] — chercher dans les fichiers ; défaut = sous-chaîne insensible à la casse + .gitignore ; --regex = motif regex insensible à la casse ; --no-ignore = ignorer .gitignore."),
    ("run_command", "run_command [--cwd <path>] <cmd> [arg1 arg2 ...] — exécuter une commande (autorisée par la politique). Optionnel : --cwd workspace:/ ou chemin disque (allowed_read_paths). Si tools_policy run_command_default_cwd_workspace: true, cwd par défaut = workspace de la tâche. Pour GitHub depuis le shell, préférer gh-axi (npm install -g gh-axi ; principes AXI https://axi.md/) s'il est installé — sorties compactes pour l'agent. Pour l'automation navigateur en CLI, chrome-devtools-axi (même dépôt https://github.com/kunchenguid/axi) en complément d'Akasha browser."),
    ("run_terminal", "run_terminal [--cwd <path>] <cmd> [args...] — exécuter une commande (même que run_command)"),
    ("run_command_background", "run_command_background [--cwd <path>] <cmd> [args...] — lancer en arrière-plan, retourne session_id pour process poll/kill"),
    ("terminal_session", "terminal_session — PTY interactif: GET /api/terminal/capabilities ; API HTTP /api/terminal/pty/sessions (spec/43_session_terminal.md). Sinon: run_command / run_terminal, run_command_background + process list|poll|kill, GET /api/process/watch/recent."),
    ("process", "process list | process poll <session_id> | process kill <session_id> — lister, consulter ou arrêter des commandes en arrière-plan"),
    ("file_diff", "file_diff <path_a> <path_b> — diff texte entre deux fichiers (ligne à ligne)"),
    ("diff_unified", "diff_unified <path_a> <path_b> [context_lines] — diff unifié style patch (défaut context_lines=3) ; chemins réels ou workspace:/"),
    ("dir_compare", "dir_compare <dir_a> <dir_b> [max_depth] [max_files] — comparer deux arborescences (fichiers uniquement dans A/B, contenu différent) ; profondeur et nombre de fichiers bornés (défaut 8 et 100)"),
    ("git_status", "git_status <repo> — git status --porcelain=v1 -b dans le dépôt (nécessite git dans allowed_commands)"),
    ("git_diff", "git_diff <repo> [--staged] [pathspec...] — git diff ; pathspecs relatifs au dépôt, validés sous la racine"),
    ("git_log", "git_log <repo> [n] — git log -n N --oneline (N entre 1 et 100, défaut 20)"),
    ("git_rev_parse", "git_rev_parse <repo> — git rev-parse HEAD"),
    ("edit_file", "edit_file <path> <start_line> <end_line> <new_content> — remplacer les lignes start..end par new_content (lignes 1-based)"),
    ("apply_patch", "apply_patch <path> <patch_content> — appliquer un patch unifié (contenu du patch après le path)"),
    ("search_replace", "search_replace <path> <ancien_texte> | <nouveau_texte> — une seule ligne TOOL:. Séparateur : **espace | espace** (` | `). Après le chemin, mettre tout de suite le texte exact à remplacer (pas un `|` seul : le découpage sur espaces le transforme en token et vide la recherche). Si le motif contient ` | `, utiliser edit_file ou apply_patch. Exemple : TOOL: search_replace workspace:/src/App.tsx const x = 1 | const x = 2"),
    ("web_fetch", "web_fetch <url> — récupérer le contenu d'une URL (domaine autorisé dans tools_policy allowed_web_domains)"),
    ("web_search", "web_search <query> [max_results] — recherche web multi-fournisseurs (Brave, SearXNG, DuckDuckGo, … ; web_search_enabled)"),
    ("web_crawl", "web_crawl <url> [limit] — lancer un crawl Cloudflare Browser Rendering (web_crawl_enabled, cloudflare_account_id, token vault cloudflare_api_token ou CLOUDFLARE_API_TOKEN ; domaines = allowed_web_domains). Retourne un job_id ; poller avec web_crawl_status."),
    ("web_crawl_status", "web_crawl_status <job_id> — statut / résultat d’un job crawl Cloudflare (même config que web_crawl)."),
    ("run_in_container", "run_in_container <work_dir> <image> <command> [args...] — exécuter une commande dans un conteneur (work_dir autorisé en lecture, ex. node:20 node index.js)"),
    ("memory_search", "memory_search <query> [top_k] — rechercher dans la mémoire long terme (si activée)"),
    ("user_rag_search", "user_rag_search <query> [top_k] — rechercher dans les documents utilisateur indexés (user RAG)"),
    ("notes_list", "notes_list — lister les notes utilisateur (id + titre)"),
    ("notes_read", "notes_read <id> — lire le contenu markdown d'une note"),
    ("notes_write", "notes_write <id> puis contenu markdown sur les lignes suivantes — mettre à jour une note"),
    ("notes_search", "notes_search <query> [limit] — rechercher dans les notes utilisateur"),
    ("workspace_graph_search", "workspace_graph_search <query> [--workspace <uuid>] — rechercher dans les graphes projet indexés (nœuds label/chemin) ; limite ~20 lignes ; --workspace pour un espace enregistré uniquement"),
    ("memory_store", "memory_store <content> <source> [link_to: uuid1+kind1,uuid2+kind2,...] [link_kind: default_kind] — mémoire long terme. Types recommandés : similar, relates_to, related, updates, supersedes, excludes, contradicts, supports, derived_from, same_as, spouse, child, birth_date, … ; par cible utiliser uuid+kind, ou uuid seuls avec link_kind (défaut related)."),
    ("memory_delete", "memory_delete <id> — supprimer une entrée de la mémoire long terme par son id (UUID)"),
    ("memory_update", "memory_update <id> <new_content> — mettre à jour le contenu d'une entrée (re-embedding automatique)"),
    ("memory_forget", "memory_forget <query> — supprimer les entrées dont le contenu correspond aux mots-clés (plan moyen terme 9)"),
    ("memory_stats", "memory_stats — nombre d'entrées et taille approximative de la mémoire long terme"),
    ("memory_gc", "memory_gc [retention_days] [protect_sources...] — supprimer les entrées plus anciennes que N jours (sources protégées optionnelles, ex. user_fact project)"),
    ("sessions_list", "sessions_list [limit] — lister les tâches/sessions récentes"),
    ("sessions_spawn", "sessions_spawn <message> [session_id] — créer une sous-tâche et la lancer"),
    ("session_status", "session_status <task_id> — statut d'une tâche donnée"),
    ("schedule_task", "schedule_task <cron> <prompt> [title] — créer une tâche planifiée active."),
    ("list_scheduled_tasks", "list_scheduled_tasks [limit] — lister les schedules actifs."),
    ("cancel_scheduled_task", "cancel_scheduled_task <schedule_id> — supprimer un schedule par UUID."),
    ("wake_in", "wake_in <minutes> <message> — programmer un rappel agent unique dans la session courante (plus léger que schedule_task)."),
    ("calendar_query", "calendar_query <from_iso> <to_iso> [account_id] — lister les événements calendrier externes (CalDAV/ICS) sur une plage ISO8601."),
    ("calendar_create", "calendar_create <summary> <from_iso> <to_iso> [account_id] [description] — créer un événement calendrier externe (file d'attente sync CalDAV)."),
    ("calendar_update", "calendar_update <event_id> <json_fields> — mettre à jour un événement (summary, dtstart, dtend, description, location)."),
    ("calendar_delete", "calendar_delete <event_id> — supprimer (soft-delete) un événement externe."),
    ("budget_status", "budget_status [session_id] — état budget (usage tokens/coût, seuil, auto-concise)."),
    ("message", "message send <channel> <text> — envoyer un message vers un canal (webhook configuré via AKASHA_MESSAGE_WEBHOOK_URL)"),
    ("browser", "browser navigate <url> — navigate (http/https; domain allowed). browser snapshot — texte + liens. browser screenshot | browser click <css> | browser fill <css> <texte> | browser wait <css_selector|ms> — automation Playwright (spec 39)."),
    ("install_playwright", "install_playwright — run npm install and npx playwright install chromium in the Playwright runner directory (scripts/playwright-runner or AKASHA_PLAYWRIGHT_RUNNER). Requires browser_enabled. Use after ask_user consent if you need explicit approval before download; optional require_approval in tools_policy."),
    ("image", "image <path|url> [prompt] — vision: joindre l'image en pièce jointe au chat (modèle vision dans llm_router)"),
    ("pdf", "pdf <path> — extraire le texte d'un PDF (path dans allowed_read_paths)"),
    ("ask_user", "ask_user — demande une information à l'utilisateur (human in the loop). Ligne suivante : JSON avec question (requis), context (optionnel), choices (optionnel, tableau de chaînes pour choix multiples). Pour une réponse ouverte (chemin, texte libre, secret), omettre choices ou laisser un tableau vide. Si choices est fourni, l'UI propose quand même une saisie libre en plus des boutons. Exemple : {\"question\":\"Quel fichier ?\",\"context\":\"...\",\"choices\":[\"a.txt\",\"b.txt\"]}"),
    ("studio_list_tickets", "studio_list_tickets — (Code Studio) lister les tickets Kanban du projet (JSON compact : id, titre, statut, prérequis, agents). Réservé au disque projet courant (workspace)."),
    ("studio_create_ticket", "studio_create_ticket <json> — (Code Studio) créer un ticket. Un seul objet JSON en argument (titre requis, description, assigned_agent, review_agent, depends_on_ticket_ids, status). Réservé studio-projects."),
    ("studio_update_ticket", "studio_update_ticket <json> — (Code Studio) mettre à jour un ticket (ticket_id ou id requis, champs optionnels comme PATCH HTTP). Réservé studio-projects."),
    ("delegate_to_agent", "delegate_to_agent <agent_type> <message> — déléguer à un sous-agent (Code Studio : réservé à studio_project_manager). agent_type utiles : studio_frontend | studio_backend | studio_fullstack | studio_scaffold | studio_planner | qa | code | conversation | … (voir liste des spécialistes). Un seul niveau depuis la tâche racine : les sous-agents ne rappellent pas delegate_to_agent."),
    ("install_skill", "install_skill <url> — installer un skill depuis une URL GitHub (ex. https://github.com/BankrBot/skills/tree/main/bankr). Télécharge SKILL.md, l'enregistre dans le dossier skills, puis recharge les skills."),
    ("uninstall_skill", "uninstall_skill <name> — désinstaller un skill (supprime data_dir/skills/<name>, retire la commande de tools_policy si présente, recharge les skills)."),
    ("device_discover", "device_discover [interface] — lister les appareils accessibles (optionnel: local_media, system, network, usb). Filtre par politique allowed_device_interfaces / blocked_device_interfaces."),
    ("device_invoke", "device_invoke <interface> <device_id> <action> [params] — exécuter une action sur un appareil. local_media: caméra (device_id camera, action capture), micro (device_id microphone, action record). Appelle directement ; une fenêtre d'autorisation s'affichera dans l'UI. Ne pas demander à l'utilisateur d'« ouvrir l'UI » — utiliser l'outil. synthetic_input: device_id keyboard|mouse, action shortcut|key|type|mouse_move|mouse_click|..."),
    ("generate_image", "generate_image <prompt> [size] — générer une image par IA (ex. OpenAI DALL·E). Prompt en texte libre (plusieurs mots après generate_image sont un seul prompt) ; size optionnel en dernier (1024x1024, 512x512). Retourne l'image en data URL dans la réponse (spec 42)."),
    ("speech_synthesize", "speech_synthesize <text> — TTS: synthétiser le texte en audio (Kyutai Unmute/Pocket TTS). Retourne une data URL audio (voice_router.yaml tts.base_url)."),
    ("speech_transcribe", "speech_transcribe <data_url_audio> — STT: transcrire l'audio en texte. Passer la data URL de l'audio (ex. après device_invoke local_media microphone record). La data URL est obligatoire (voice_router.yaml stt.base_url)."),
    ("write_todos", "write_todos <payload> — définir la liste d'étapes (todo). Remplace toute la liste (plan initial ou re-découpage complet). Pour ajouter sans effacer : merge_todos. Payload: JSON array ou lignes."),
    ("merge_todos", "merge_todos <payload> — ajoute des étapes (même format que write_todos) sans supprimer les existantes ; titres déjà présents ignorés (casse insensible)."),
    ("read_todos", "read_todos — retourne la liste des étapes (todos) de la tâche courante."),
    ("update_todo", "update_todo <index> <status> — marquer l'étape à l'index (1-based) comme status (done, cancelled)."),
    ("list_skills", "list_skills — retourne la liste des skills installés (nom et description). Utiliser avant read_skill pour charger le détail d'un skill."),
    ("search_skills_catalog", "search_skills_catalog <query> [max] — rechercher dans la galerie Akasha_skills (skills.json distant)"),
    ("read_skill", "read_skill <name> — charge le contenu (instructions, usage) du skill. À utiliser quand tu as besoin du détail d'un skill avant de l'invoquer par son nom."),
    ("github_repo_info", "github_repo_info <owner> <repo> — métadonnées GitHub (stars, langue, licence, activité)"),
    ("analyze_table", "analyze_table <inspect|summary> <path.csv> — analyse tabulaire CSV native (SQL/XLSX: skill tabular-insights)"),
    ("arxiv_search", "arxiv_search <query> [max] — recherche arXiv (API Atom)"),
    ("http_probe", "http_probe <METHOD> <url> [body] — sonde HTTP contrôlée (domaines allowed_web_domains)"),
    ("plugin.call", "plugin.call <plugin_id> <json_or_args...> — exécuter un plugin de type tool chargé dans le daemon. Exemple: TOOL: plugin.call maps {\"action\":\"distance\",\"from\":{\"lat\":45.698,\"lon\":0.328},\"to\":{\"lat\":49.009,\"lon\":2.547},\"mode\":\"car\"}"),
    ("maps_distance", "maps_distance <from_lat> <from_lon> <to_lat> <to_lon> [mode] — via plugin maps, calcule distance et durée estimée."),
    ("maps_route", "maps_route <from_lat> <from_lon> <to_lat> <to_lon> [mode] — via plugin maps, retourne un itinéraire simplifié avec geometry map-ready."),
    ("graph_plot", "graph_plot <chart> <y1> <y2> ... | plugin.call graph <json> — via plugin graph, génère une figure Plotly (line/bar/scatter/histogram)."),
    ("graph_stats", "graph_stats <json_or_args...> — via plugin graph, calcule min/max/moyenne/compte par série et retourne une vue table."),
    ("sim_run", "sim_run <initial> <growth_rate> <noise> <horizon> | plugin.call simulation <json> — via plugin simulation, exécute une simulation déterministe et retourne une vue timeseries + métriques."),
    ("sim_compare", "sim_compare <initial> <growth_rate> <noise> <horizon> | plugin.call simulation <json> — via plugin simulation, compare scénario de base et alternatif, retourne delta + tableau de résultats."),
    ("ha_get_state", "ha_get_state <entity_id> — via plugin homeassistant, lit l'état d'une entité HA (ex. light.salon). Connecteur HA activé + token vault requis."),
    ("ha_list_entities", "ha_list_entities [domain] — via plugin homeassistant, liste les entités (filtre domain optionnel : light, sensor, …)."),
    ("ha_call_service", "ha_call_service <domain> <service> <entity_id> [json_data] — via plugin homeassistant, appelle un service (ex. light turn_on light.salon). Domaines lock/alarm exigent confirm:true ou HITL."),
    ("ha_run_script", "ha_run_script <script_id> — via plugin homeassistant, lance script.turn_on (script_id avec ou sans préfixe script.)."),
    ("mcp_server_add", "mcp_server_add <name> <command> [args...] — ajouter ou remplacer une entrée dans data_dir/mcp.json (stdio MCP). Exemple: mcp_server_add demo npx -y @modelcontextprotocol/server-filesystem /tmp"),
    ("mcp_server_remove", "mcp_server_remove <name> — supprimer un serveur MCP de mcp.json."),
];

/// Tools advertised in the Code Studio prompt: dev/repo tools only (policy still gates execution).
/// Omits browser, memory_*, sessions_*, device_*, speech, maps plugins, etc.
/// `delegate_to_agent` n’est ajouté que pour `studio_project_manager` et seulement si la politique l’autorise.
pub(crate) fn code_studio_tools_for_prompt(allowed_tools: Option<&[String]>, assigned_agent: &str) -> Vec<String> {
    const STUDIO: &[&str] = &[
        "read_file",
        "write_file",
        "write_code",
        "delete_file",
        "rename_path",
        "move_tree",
        "search_files",
        "grep_content",
        "run_command",
        "run_terminal",
        "run_command_background",
        "process",
        "file_diff",
        "diff_unified",
        "dir_compare",
        "git_status",
        "git_diff",
        "git_log",
        "git_rev_parse",
        "edit_file",
        "apply_patch",
        "search_replace",
        "web_fetch",
        "web_search",
        "run_in_container",
        "ask_user",
        "write_todos",
        "merge_todos",
        "read_todos",
        "update_todo",
        "list_skills",
        "read_skill",
        "workspace_graph_search",
        "pdf",
        "image",
    ];
    let mut out: Vec<String> = match allowed_tools {
        None => STUDIO.iter().map(|s| (*s).to_string()).collect(),
        Some(list) => {
            let allowed_lc: std::collections::HashSet<String> =
                list.iter().map(|s| s.to_ascii_lowercase()).collect();
            STUDIO
                .iter()
                .filter(|t| allowed_lc.contains(&t.to_ascii_lowercase()))
                .map(|s| (*s).to_string())
                .collect()
        }
    };
    if out.is_empty() {
        out.extend(
            ["read_file", "write_file", "write_code", "grep_content", "run_command", "ask_user"]
                .iter()
                .map(|s| (*s).to_string()),
        );
    }
    let delegate_allowed_by_policy = allowed_tools.map_or(true, |l| {
        l.iter()
            .any(|t| t.eq_ignore_ascii_case("delegate_to_agent"))
    });
    if assigned_agent.eq_ignore_ascii_case("studio_project_manager") && delegate_allowed_by_policy {
        if !out
            .iter()
            .any(|t| t.eq_ignore_ascii_case("delegate_to_agent"))
        {
            out.push("delegate_to_agent".to_string());
        }
    }
    if assigned_agent.eq_ignore_ascii_case("studio_project_manager") {
        for t in [
            "studio_list_tickets",
            "studio_create_ticket",
            "studio_update_ticket",
        ] {
            if !out.iter().any(|x| x.eq_ignore_ascii_case(t)) {
                out.push(t.to_string());
            }
        }
    }
    if assigned_agent.eq_ignore_ascii_case("studio_reviewer") {
        out.retain(|t| {
            !matches!(
                t.as_str(),
                "write_file"
                    | "write_code"
                    | "delete_file"
                    | "rename_path"
                    | "move_tree"
                    | "edit_file"
                    | "apply_patch"
                    | "search_replace"
                    | "delegate_to_agent"
            )
        });
        for t in ["studio_list_tickets", "studio_update_ticket"] {
            if !out.iter().any(|x| x.eq_ignore_ascii_case(t)) {
                out.push(t.to_string());
            }
        }
    }
    out
}

/// Like [`available_tools_instruction`] but **only** listed tool names — no `always_misc` merge
/// (Code Studio must not advertise install_skill, browser, delegate_to_agent, etc.).
pub(crate) fn available_tools_instruction_exact(allowed: &[String]) -> String {
    if allowed.is_empty() {
        return String::new();
    }
    let allowed_lc: std::collections::HashSet<String> =
        allowed.iter().map(|s| s.to_ascii_lowercase()).collect();
    AVAILABLE_TOOLS
        .iter()
        .filter(|(name, _)| allowed_lc.contains(&name.to_ascii_lowercase()))
        .map(|(_, desc)| *desc)
        .collect::<Vec<_>>()
        .join(" ; ")
}

pub(crate) fn available_tools_instruction(allowed_tools: Option<&[String]>) -> String {
    let iter: Box<dyn Iterator<Item = &(&str, &str)>> = if let Some(allowed) = allowed_tools {
        Box::new(AVAILABLE_TOOLS.iter().filter(move |(name, _)| {
            let always_misc = *name == "ask_user"
                || *name == "install_skill"
                || *name == "uninstall_skill"
                || *name == "write_todos"
                || *name == "merge_todos"
                || *name == "read_todos"
                || *name == "update_todo"
                || *name == "list_skills"
                || *name == "read_skill";
            let in_profile = allowed.iter().any(|a| a == *name);
            always_misc || in_profile
        }))
    } else {
        Box::new(AVAILABLE_TOOLS.iter())
    };
    iter.map(|(_, desc)| *desc).collect::<Vec<_>>().join(" ; ")
}

/// Single-pass intent flags for a message (one to_lowercase() shared by all checks).
pub(crate) struct MessageIntentFlags {
    pub(crate) save_file: bool,
    pub(crate) external_info: bool,
    /// User wants posts/timeline from X, Twitter, or similar (inject SOCIAL_FEED_REMINDER).
    pub(crate) social_feed_fetch: bool,
    pub(crate) camera_or_mic: bool,
    pub(crate) image_generation: bool,
    pub(crate) code_generation: bool,
    /// User asks for GitHub repo/API info and mentions vault or GITHUB_TOKEN.
    pub(crate) github_with_vault: bool,
    /// User asks about transport schedules, routes, or travel info (train, bus, flight, etc.)
    pub(crate) transport: bool,
    /// User asks for a geographic distance/route between two places.
    pub(crate) geolocation_distance: bool,
}

/// Heuristic intent labels for **debug only** (`GET/POST /api/plugins/routing_rules`).
/// Runtime tool execution is no longer gated on manifest `routing_rules` / these intents.
pub(crate) fn active_intents_from_flags(flags: &MessageIntentFlags) -> Vec<&'static str> {
    let mut out = Vec::new();
    if flags.save_file {
        out.push("save_file");
    }
    if flags.external_info {
        out.push("external_info");
    }
    if flags.social_feed_fetch {
        out.push("social_feed_fetch");
    }
    if flags.camera_or_mic {
        out.push("camera_or_mic");
    }
    if flags.image_generation {
        out.push("image_generation");
    }
    if flags.code_generation {
        out.push("code_generation");
    }
    if flags.github_with_vault {
        out.push("github_with_vault");
    }
    if flags.transport {
        out.push("transport");
    }
    if flags.geolocation_distance {
        out.push("geolocation_distance");
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SmallTalkLanguage {
    French,
    English,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SmallTalkIntent {
    pub(crate) language: SmallTalkLanguage,
    pub(crate) asks_status: bool,
}

pub(crate) fn classify_small_talk_message(message: &str) -> Option<SmallTalkIntent> {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed
        .to_lowercase()
        .replace('’', "'")
        .replace(['!', '?', '.', ',', ';', ':'], " ");
    let collapsed = lower.split_whitespace().collect::<Vec<_>>().join(" ");

    // Reject messages that mingle small-talk with "real request" keywords
    let real_request_keywords = [
        "peux tu",
        "peux-tu",
        "peut tu",
        "peut-tu",
        "pouvez vous",
        "pouvez-vous",
        "tu peux",
        "peux me",
        "peux nous",
        "could you",
        "can you",
        "would you",
        "can you help",
        "can you tell",
        "me rappeler",
        "me rappelle",
        "help me",
        "show me",
        "tell me",
        "lire",
        "read",
        "fichier",
        "file",
        "faire",
        "do",
        "créer",
        "create",
        "écrire",
        "write",
        "générer",
        "generate",
        "projet",
        "project",
        "avait",
        "avaient",
        "avez",
        "have",
        "has",
        "fait",
        "done",
        "hier",
        "yesterday",
        "dernier",
        "last",
    ];
    for keyword in real_request_keywords {
        if collapsed.contains(keyword) {
            return None;
        }
    }

    let has_french = [
        "salut",
        "bonjour",
        "bonsoir",
        "coucou",
        "ca va",
        "ça va",
        "comment ca va",
        "comment ça va",
    ]
    .iter()
    .any(|k| collapsed.contains(k));
    let has_english = [
        "hello",
        "hi",
        "hey",
        "good morning",
        "good evening",
        "how are you",
        "hows it going",
        "how's it going",
    ]
    .iter()
    .any(|k| collapsed.contains(k));
    let asks_status = [
        "ca va",
        "ça va",
        "comment ca va",
        "comment ça va",
        "how are you",
        "hows it going",
        "how's it going",
    ]
    .iter()
    .any(|k| collapsed.contains(k));
    if has_french {
        Some(SmallTalkIntent {
            language: SmallTalkLanguage::French,
            asks_status,
        })
    } else if has_english {
        Some(SmallTalkIntent {
            language: SmallTalkLanguage::English,
            asks_status,
        })
    } else {
        None
    }
}

pub(crate) fn small_talk_fast_lane(message: &str) -> Option<SmallTalkIntent> {
    let intent = classify_small_talk_message(message)?;
    let trimmed = message.trim();
    if trimmed.contains('\n') || trimmed.chars().count() > 80 {
        return None;
    }
    let word_count = trimmed
        .split_whitespace()
        .filter(|w| {
            !w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'' && c != '-')
                .is_empty()
        })
        .count();
    if word_count > 8 {
        return None;
    }
    let lower = trimmed.to_lowercase();
    let flags = compute_message_intent_flags(trimmed);
    if flags.save_file
        || flags.external_info
        || flags.social_feed_fetch
        || flags.camera_or_mic
        || flags.image_generation
        || flags.code_generation
        || flags.github_with_vault
        || message_suggests_project(trimmed)
    {
        return None;
    }
    if [
        "fichier",
        "file",
        "code",
        "projet",
        "project",
        "browser",
        "navigateur",
        "outil",
        "tool",
        "github",
        "api",
        "cargo",
        "rust",
        "erreur",
        "error",
        "bug",
    ]
    .iter()
    .any(|k| lower.contains(k))
    {
        return None;
    }
    Some(intent)
}

pub(crate) fn small_talk_fast_reply(message: &str, intent: SmallTalkIntent) -> String {
    let lower = message.to_lowercase();
    match intent.language {
        SmallTalkLanguage::French => {
            if intent.asks_status {
                "Salut ! Oui, ça va bien 😊 Et toi ?".to_string()
            } else if lower.contains("bonsoir") {
                "Bonsoir ! 👋".to_string()
            } else if lower.contains("bonjour") {
                "Bonjour ! 👋".to_string()
            } else if lower.contains("coucou") {
                "Coucou ! 👋".to_string()
            } else {
                "Salut ! 👋".to_string()
            }
        }
        SmallTalkLanguage::English => {
            if intent.asks_status {
                "Hi! I'm doing well 😊 How about you?".to_string()
            } else if lower.contains("good morning") {
                "Good morning! 👋".to_string()
            } else if lower.contains("good evening") {
                "Good evening! 👋".to_string()
            } else {
                "Hi! 👋".to_string()
            }
        }
    }
}

pub(crate) fn response_looks_off_topic_for_small_talk(response: &str) -> bool {
    let lower = response.to_lowercase();
    lower.contains("tool:")
        || lower.contains("tools_policy")
        || lower.contains("allowed_write_paths")
        || lower.contains("allowed_read_paths")
        || lower.contains("write_file")
        || lower.contains("read_file")
        || lower.contains("browser navigate")
        || lower.contains("web_search")
        || lower.contains("memory_store")
        || lower.contains("delegate_to_agent")
        || lower.contains("vault:")
        || lower.len() > 240
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionRecallRange {
    Yesterday,
    CurrentDay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionRecallIntent {
    pub(crate) range: SessionRecallRange,
    pub(crate) language: SmallTalkLanguage,
}

/// True when the user likely asks for a session recap using the English word "recap".
/// Avoid `contains("recap")` — it false-positives on phrases like "task recap" in long
/// Code Studio DESIGN.md instructions shipped by the UI.
fn english_session_recap_word(lower: &str) -> bool {
    let sans_doc_phrase = lower.replace("task recap", " ");
    sans_doc_phrase
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|w| w == "recap")
}

/// French tokens suggesting « remind me / session recap ».
/// Uses whole-token matching so phrases like « ne **rappellent** pas » do not false-positive
/// on substring `rappel` (regression: mandatory delegation prefix + session recap fast path).
fn french_session_recall_word_tokens(lower: &str) -> bool {
    const TOKENS: &[&str] = &[
        "rappel",
        "rappeler",
        "rappelle",
        "rappelles",
        "rappelez",
        "rappelons",
    ];
    lower
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '\'')
        .filter(|t| !t.is_empty())
        .any(|w| TOKENS.contains(&w))
}

pub(crate) fn detect_session_recall_intent(message: &str) -> Option<SessionRecallIntent> {
    let lower = message
        .trim()
        .to_lowercase()
        .replace('’', "'")
        .replace(['!', '?', '.', ',', ';', ':'], " ");
    if lower.is_empty() {
        return None;
    }
    let asks_recall = [
        "ce qu'on a fait",
        "ce qu on a fait",
        "on a fait",
        "what we did",
        "remind me",
        "recap what we did",
        "what did we do",
    ]
    .iter()
    .any(|k| lower.contains(k))
        || french_session_recall_word_tokens(&lower)
        || english_session_recap_word(&lower);
    if !asks_recall {
        return None;
    }
    let range = if ["hier", "yesterday", "last night", "hier soir"]
        .iter()
        .any(|k| lower.contains(k))
    {
        SessionRecallRange::Yesterday
    } else {
        SessionRecallRange::CurrentDay
    };
    let language = if [
        "bonjour", "salut", "merci", "hier", "rappeler", "qu'on", "quoi",
    ]
    .iter()
    .any(|k| lower.contains(k))
    {
        SmallTalkLanguage::French
    } else {
        SmallTalkLanguage::English
    };
    Some(SessionRecallIntent { range, language })
}

pub(crate) fn build_session_recap_reply(
    turns: &[crate::memory::ConversationTurn],
    intent: SessionRecallIntent,
) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    for t in turns {
        let c = t.content.trim();
        if c.is_empty() {
            continue;
        }
        let lower = c.to_lowercase();
        // Filter out system/error messages
        if lower.contains("tools_policy")
            || lower.contains("allowed_write_paths")
            || lower.contains("allowed_read_paths")
            || lower.contains("allowed_paths")
            || lower.contains("llm response timed out")
            || lower.contains("timed out")
            || lower.starts_with("[recent context")
            || lower.starts_with("[mémoire")
            || lower.starts_with("[system")
            || lower.starts_with("[error")
            || lower.contains("not authorized")
            || lower.contains("pas autorisé")
            || lower.contains("cannot create")
            || lower.contains("ne peux pas créer")
            || lower.contains("cannot write")
            || lower.contains("cannot read")
            // Filter out previous recap generations
            || lower.starts_with("bien sûr — voici")
            || lower.starts_with("sure — here's")
            || lower.starts_with("voici ce qu'on a fait")
            || lower.starts_with("here's what we")
            // Filter out generic greetings/closings
            || (lower.contains("bonjour") && lower.len() < 100)
            || (lower.contains("salut") && lower.len() < 80)
            || lower == "oui" || lower == "yes"
            || lower == "ok" || lower == "d'accord"
            || lower == "merci" || lower == "thanks"
            || lower == "merci beaucoup" || lower == "thank you"
        {
            continue;
        }
        let compact = c.replace('\n', " ").trim().to_string();
        // Skip very short fragments
        if compact.len() < 20 {
            continue;
        }
        lines.push(compact);
    }
    lines.dedup();
    if lines.is_empty() {
        return None;
    }
    let selected: Vec<String> = lines
        .into_iter()
        .rev()
        .take(5)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    match intent.language {
        SmallTalkLanguage::French => {
            let period = match intent.range {
                SessionRecallRange::Yesterday => "hier",
                SessionRecallRange::CurrentDay => "dans cette session",
            };
            let mut out = format!("Bien sûr — voici ce qu'on a fait {} :\n", period);
            for item in selected {
                out.push_str("- ");
                let truncated = if item.len() > 400 {
                    format!("{}…", item.chars().take(397).collect::<String>())
                } else {
                    item
                };
                out.push_str(&truncated);
                out.push('\n');
            }
            Some(out.trim_end().to_string())
        }
        SmallTalkLanguage::English => {
            let period = match intent.range {
                SessionRecallRange::Yesterday => "yesterday",
                SessionRecallRange::CurrentDay => "in this session",
            };
            let mut out = format!("Sure — here's what we did {}:\n", period);
            for item in selected {
                out.push_str("- ");
                let truncated = if item.len() > 400 {
                    format!("{}…", item.chars().take(397).collect::<String>())
                } else {
                    item
                };
                out.push_str(&truncated);
                out.push('\n');
            }
            Some(out.trim_end().to_string())
        }
    }
}

/// Best-effort X handle from user text (e.g. `@akasha_anthiam` → `akasha_anthiam`).
pub(crate) fn extract_x_profile_handle(message: &str) -> Option<String> {
    for token in message.split_whitespace() {
        let t = token.trim_end_matches(|c| matches!(c, '.' | ',' | ':' | ';'));
        if let Some(rest) = t.strip_prefix('@') {
            let handle: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if handle.len() >= 2 {
                return Some(handle);
            }
        }
    }
    None
}

/// True when policy allows at least one follow-up path after web_search: HTTP fetch and/or Playwright browser.
pub(crate) fn web_followup_tools_configured(policy: &akasha_tools::ToolsPolicy) -> bool {
    let web_fetch_ok = policy.can_use_tool("web_fetch") && !policy.allowed_web_domains.is_empty();
    let browser_ok = policy.can_use_tool("browser")
        && policy.browser_enabled
        && (policy
            .browser_allowed_domains
            .iter()
            .any(|d| d.trim().eq_ignore_ascii_case("*"))
            || !policy.browser_allowed_domains.is_empty());
    web_fetch_ok || browser_ok
}

/// True when the user likely refers to the SNCF regional product "TER".
/// Substring `ter ` must not be used: it matches inside "connecter", "twitter", etc.
fn message_mentions_ter_train_line(message_lower: &str) -> bool {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"(?i)\bter\b").expect("valid regex"));
    re.is_match(message_lower)
}

pub(crate) fn compute_message_intent_flags(message: &str) -> MessageIntentFlags {
    let m = message.to_lowercase();
    MessageIntentFlags {
        save_file: [
            "enregistre",
            "enregistrer",
            "sauvegarde",
            "sauvegarder",
            "écris dans",
            "ecris dans",
            "write to file",
            "save to",
            "save the file",
            "write the file",
            "dans le dossier",
            "dans le fichier",
            "dans un fichier",
            "sur le disque",
            "to disk",
            "to the file",
        ]
        .iter()
        .any(|k| m.contains(k)),
        external_info: [
            "météo",
            "meteo",
            "weather",
            "prévisions",
            "previsions",
            "actualités",
            "actualites",
            "horaires",
            "trafic",
            "prix",
            "cours ",
            "bourse",
            "news",
            "nouvelle",
            "semaine à",
            "aujourd'hui",
            "demain",
            "connaître la",
            "connaitre la",
            "quelle est la météo",
            "quel temps",
            "prévision",
            "prevision",
        ]
        .iter()
        .any(|k| m.contains(k)),
        social_feed_fetch: {
            let on_x_or_twitter = m.contains("x.com")
                || m.contains("twitter.com")
                || m.contains(" sur x")
                || m.contains("sur x.")
                || (m.contains("twitter") && m.contains('@'));
            let wants_posts = m.contains("post")
                || m.contains("tweet")
                || m.contains("récup")
                || m.contains("recup")
                || m.contains("retrieve")
                || m.contains("latest")
                || m.contains("dernier")
                || m.contains("timeline")
                || m.contains("fil d'actualité")
                || m.contains("actualité de @");
            on_x_or_twitter && wants_posts
        },
        camera_or_mic: [
            "webcam",
            "caméra",
            "camera",
            "prend une photo",
            "prends une photo",
            "prendre une photo",
            "take a photo",
            "take a picture",
            "prends moi en photo",
            "photo avec la webcam",
            "accède à la webcam",
            "accede a la webcam",
            "utilise la caméra",
            "utilise la camera",
            "affiche-la dans le chat",
            "afficher dans le chat",
            "display in the chat",
            "show in the chat",
            "micro",
            "microphone",
            "enregistre avec le micro",
            "enregistrer avec le micro",
            "obtenir une image",
            "get an image",
            "image avec la webcam",
            "image avec la caméra",
            "continuer pour obtenir une image",
            "proceed to get an image",
        ]
        .iter()
        .any(|k| m.contains(k)),
        image_generation: [
            "génère une image",
            "genere une image",
            "générer une image",
            "génère moi une image",
            "generate an image",
            "generate a picture",
            "draw",
            "dessine",
            "dessiner",
            "crée une image",
            "cree une image",
            "creer une image",
            "creer moi une image",
            "créer une image",
            "create an image",
            "image par ia",
            "ai image",
            "dall-e",
            "dalle",
        ]
        .iter()
        .any(|k| m.contains(k)),
        code_generation: [
            "écris un script",
            "ecris un script",
            "écrire un script",
            "ecrire un script",
            "write a script",
            "write the script",
            "génère du code",
            "genere du code",
            "generate code",
            "génère le code",
            "code python",
            "python script",
            "un programme qui",
            "a program that",
            "fonction qui",
            "function that",
            "snippet",
            "extrait de code",
            "piece of code",
            "exemple de code",
            "analyse ce projet",
            "analyze this project",
            "analyse le projet",
            "analyze the project",
            "review the codebase",
            "auditer le code",
            "code review",
            "dépôt git",
            "depot git",
        ]
        .iter()
        .any(|k| m.contains(k)),
        github_with_vault: {
            let github = [
                "github",
                "dépôt privé",
                "depot prive",
                "private repo",
                "api.github.com",
            ]
            .iter()
            .any(|k| m.contains(k));
            let vault = [
                "vault",
                "github_token",
                "clé du vault",
                "cle du vault",
                "key in the vault",
                "token dans le vault",
                "clef dans le vault",
            ]
            .iter()
            .any(|k| m.contains(k));
            github && vault
        },
        transport: message_mentions_ter_train_line(&m)
            || [
                "train",
                "tgv",
                "sncf",
                "gare ",
                "horaires de train",
                "horaires de bus",
                "horaires du train",
                "billet de train",
                "trajet en train",
                "rer ",
                "transilien",
                "bus ",
                "métro ",
                "metro ",
                "tramway",
                "tram ",
                "vol ",
                "aéroport",
                "aeroport",
                "airport",
                "terminal ",
                "itinéraire",
                "itineraire",
                "horaires de métro",
                "horaires du métro",
                "départ de ",
                "depart de ",
                "arrivée à ",
                "arrivee a ",
            ]
            .iter()
            .any(|k| m.contains(k)),
        geolocation_distance: [
            "distance entre",
            "distance between",
            "distance from",
            "distance to",
            "combien de km",
            "how far",
            "itinéraire entre",
            "itineraire entre",
            "route entre",
            "trajet entre",
            "km entre",
            "kilomètre",
            "kilometre",
        ]
        .iter()
        .any(|k| m.contains(k)),
    }
}

/// True if the user message suggests a tool-only action (camera, web search, save file, image gen) without asking for code generation. Used by the orchestrator to override mistaken "code" decomposition.
pub fn message_suggests_tool_only_action(message: &str) -> bool {
    let flags = compute_message_intent_flags(message);
    (flags.camera_or_mic
        || flags.save_file
        || flags.external_info
        || flags.social_feed_fetch
        || flags.image_generation)
        && !flags.code_generation
}

/// True if the user message suggests a long-running project (novel, comic, code project) or continuing one.
/// Capture limits for post-reply memory promotion (plan court terme 7, inspired by OpenClaw plugin).
const CAPTURE_MIN_CHARS: usize = 16;
pub(crate) const CAPTURE_MAX_CHARS: usize = 4000;
pub(crate) const CAPTURE_MAX_PER_TURN: usize = 20;

/// Short acknowledgments that should not be stored in long-term memory.
static CAPTURE_ACK_PATTERNS: &[&str] = &[
    "ok",
    "okay",
    "oui",
    "non",
    "merci",
    "thanks",
    "thank you",
    "d'accord",
    "daccord",
    "👍",
    "👌",
    "ok.",
    "parfait",
    "super",
    "cool",
    "noted",
    "compris",
    "c'est noté",
];

/// If the text ends with an unclosed fenced code block (odd number of ```), appends "\n```\n" so that
/// content appended after (e.g. image markdown) is not rendered inside a code block.
pub(crate) fn ensure_no_open_code_block(text: &str) -> String {
    let count = text.matches("```").count();
    if count % 2 == 1 {
        format!("{}\n```\n", text.trim_end())
    } else {
        text.to_string()
    }
}

/// Build markdown image snippet: `\n\n![label](<url>)`. Angle brackets around URL allow parens in data URLs.
pub(crate) fn build_image_markdown(label: &str, url: &str) -> String {
    format!("\n\n![{}](<{}>)", label, url)
}

/// Max chars for a single vision attachment on the next completion (`AKASHA_VISION_INJECT_MAX_CHARS`).
pub(crate) fn vision_inject_max_chars() -> usize {
    std::env::var("AKASHA_VISION_INJECT_MAX_CHARS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(crate::tool_output::VISION_INJECT_DEFAULT_MAX_CHARS)
        .max(100_000)
}

/// True if tool `captured_image` should be forwarded to the multimodal LLM on the following turn (excludes audio TTS).
pub(crate) fn captured_media_suitable_for_vision_injection(s: &str) -> bool {
    if s.starts_with("data:audio/") {
        return false;
    }
    if s.starts_with("data:image/") {
        return true;
    }
    !s.starts_with("data:") && !s.is_empty()
}

pub(crate) fn guess_image_mime_from_path(path: &Path) -> &'static str {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .map(|e| match e.as_str() {
            "png" => "image/png",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "bmp" => "image/bmp",
            "svg" => "image/svg+xml",
            "jpg" | "jpeg" => "image/jpeg",
            _ => "application/octet-stream",
        })
        .unwrap_or("application/octet-stream")
}

/// Best-effort tool name from a streaming `TOOL:` delta chunk (partial lines allowed).
pub(crate) fn parse_tool_name_from_toolcall_delta(delta: &str) -> Option<String> {
    let idx = delta.find("TOOL:")?;
    let rest = delta[idx + 5..].trim_start();
    let name = rest.split_whitespace().next()?.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

pub(crate) fn normalize_captured_media_for_vision_turn(s: &str) -> String {
    if s.starts_with("data:image/")
        || (s.starts_with("data:") && !s.starts_with("data:audio/"))
    {
        s.to_string()
    } else if !s.starts_with("data:") {
        format!("data:image/jpeg;base64,{}", s)
    } else {
        s.to_string()
    }
}

pub(crate) fn vision_payload_within_cap(normalized: &str) -> bool {
    normalized.len() <= vision_inject_max_chars()
}

/// Returns true if content should be skipped when capturing to long-term memory (noise, loop risk, or too short/long).
pub(crate) fn should_skip_capture_content(content: &str) -> bool {
    let t = content.trim();
    if t.is_empty() {
        return true;
    }
    if t.len() < CAPTURE_MIN_CHARS {
        return true;
    }
    if t.len() > CAPTURE_MAX_CHARS {
        return true;
    }
    // Avoid storing the injected memory block (would cause recall loops).
    if t.contains("[Mémoire à long terme]")
        || t.contains("<relevant-memories>")
        || t.contains("[Projet en cours")
    {
        return true;
    }
    let lower = t.to_lowercase();
    let lower_trim = lower.trim();
    if CAPTURE_ACK_PATTERNS
        .iter()
        .any(|p| lower_trim == *p || lower_trim.starts_with(&format!("{} ", p)))
    {
        return true;
    }
    false
}

pub(crate) fn message_suggests_project(message: &str) -> bool {
    let m = message.to_lowercase();
    let keywords = [
        "résumé du projet",
        "resume du projet",
        "summary of project",
        "workspace",
        "graphe projet",
        "project graph",
        "roman",
        "bd",
        "bande dessinée",
        "bande dessinee",
        "comic",
        "novel",
        "projet de code",
        "code project",
        "écris un",
        "ecris un",
        "écris le",
        "ecris le",
        "chapitre",
        "chapter",
        "continue",
        "la suite",
        "and the rest",
        "poursuis",
        "reprends",
        "crée un projet",
        "cree un projet",
        "create a project",
        "set up a project",
    ];
    keywords.iter().any(|k| m.contains(k))
}

/// Default hosts when policy does not set allowed_skill_install_hosts (GitHub only).
const INSTALL_SKILL_DEFAULT_HOSTS: &[&str] =
    &["github.com", "raw.githubusercontent.com", "www.github.com"];

fn is_github_host(host: &str) -> bool {
    INSTALL_SKILL_DEFAULT_HOSTS
        .iter()
        .any(|h| host == *h || host.ends_with(&format!(".{}", *h)))
}

/// Append one JSON line to `data_dir/skills.lock.jsonl` (supply-chain / pin trail).
fn append_skills_lock_entry(data_dir: &Path, entry: &serde_json::Value) {
    let Ok(mut line) = serde_json::to_string(entry) else {
        eprintln!("failed to serialize skills.lock.jsonl entry");
        return;
    };
    line.push('\n');
    let path = data_dir.join("skills.lock.jsonl");
    let write = move || {
        match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            Ok(mut f) => {
                if let Err(e) = std::io::Write::write_all(&mut f, line.as_bytes()) {
                    eprintln!("failed to append to {}: {}", path.display(), e);
                }
            }
            Err(e) => {
                eprintln!("failed to open {}: {}", path.display(), e);
            }
        }
    };
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        let _ = handle.spawn_blocking(write);
    } else {
        std::thread::spawn(write);
    }
}

/// Result of parsing a skill install URL: raw SKILL.md URL, skill name, and optional GitHub API path for listing contents.
pub(crate) struct ParsedSkillUrl {
    raw_skill_url: String,
    skill_name: String,
    /// (owner, repo, branch, path) for GitHub API only; None for other hosts (single-file install).
    api_path: Option<(String, String, String, String)>,
}

/// Parse a skill install URL (GitHub, GitLab, or any allowed HTTPS host) into raw SKILL.md URL, skill name, and optional API path.
/// allowed_hosts: from policy; if it contains "*", any HTTPS host is allowed. Otherwise only listed hosts (and subdomains) are allowed.
fn parse_skill_install_url(url: &str, allowed_hosts: &[String]) -> Option<ParsedSkillUrl> {
    let url = url.trim();
    let parsed = url.parse::<url::Url>().ok()?;
    let scheme = parsed.scheme();
    if scheme != "https" {
        return None;
    }
    let host = parsed.host_str()?.to_lowercase();
    let host_allowed = allowed_hosts.iter().any(|h| h == "*")
        || allowed_hosts
            .iter()
            .any(|h| host == *h || host.ends_with(&format!(".{}", h)));
    if !host_allowed {
        return None;
    }
    let path = parsed.path().trim_matches('/');
    if path.is_empty() {
        return None;
    }
    let segments: Vec<&str> = path.split('/').collect();

    if is_github_host(&host) {
        if host.contains("raw.githubusercontent.com") {
            if segments.len() < 4 {
                return None;
            }
            let (raw_skill_url, skill_name) = if path.ends_with("SKILL.md") {
                let skill_name = segments
                    .get(segments.len().saturating_sub(2))
                    .copied()
                    .unwrap_or("skill")
                    .to_string();
                if !crate::user_rag::is_safe_relative_filename(&skill_name) {
                    return None;
                }
                (url.to_string(), skill_name)
            } else {
                let raw_url = format!(
                    "https://raw.githubusercontent.com/{}",
                    path.trim_end_matches('/')
                );
                let raw_skill_url = if raw_url.ends_with(".md") {
                    raw_url
                } else {
                    format!("{}/SKILL.md", raw_url)
                };
                let skill_name = segments.last().copied().unwrap_or("skill").to_string();
                if !crate::user_rag::is_safe_relative_filename(&skill_name) {
                    return None;
                }
                (raw_skill_url, skill_name)
            };
            let api_path = if segments.len() >= 4 {
                let owner = segments[0].to_string();
                let repo = segments[1].to_string();
                let branch = segments[2].to_string();
                let path_part = segments[3..].join("/");
                let path_part = path_part
                    .strip_suffix("SKILL.md")
                    .map(|s| s.trim_end_matches('/'))
                    .unwrap_or(&path_part)
                    .to_string();
                Some((owner, repo, branch, path_part))
            } else {
                None
            };
            Some(ParsedSkillUrl {
                raw_skill_url,
                skill_name,
                api_path,
            })
        } else {
            let tree_idx = segments.iter().position(|s| *s == "tree")?;
            if tree_idx + 2 > segments.len() {
                return None;
            }
            let owner = (*segments.get(0)?).to_string();
            let repo = (*segments.get(1)?).to_string();
            let branch = (*segments.get(tree_idx + 1)?).to_string();
            let path_segments = &segments[tree_idx + 2..];
            let path_part = path_segments.join("/");
            let skill_name = path_segments.last().copied().unwrap_or("skill").to_string();
            if !crate::user_rag::is_safe_relative_filename(&skill_name) {
                return None;
            }
            let raw_skill_url = if path_part.is_empty() {
                format!(
                    "https://raw.githubusercontent.com/{}/{}/{}/SKILL.md",
                    owner, repo, branch
                )
            } else {
                format!(
                    "https://raw.githubusercontent.com/{}/{}/{}/{}/SKILL.md",
                    owner, repo, branch, path_part
                )
            };
            let api_path = Some((owner, repo, branch, path_part));
            Some(ParsedSkillUrl {
                raw_skill_url,
                skill_name,
                api_path,
            })
        }
    } else {
        // Generic host (site web, GitLab, etc.): single-file install. URL must point to a .md file or we use path as skill name.
        let raw_skill_url = if path.ends_with(".md") {
            url.to_string()
        } else if path.ends_with('/') {
            format!("{}SKILL.md", url.trim_end_matches('/'))
        } else {
            format!("{}/SKILL.md", url.trim_end_matches('/'))
        };
        let skill_name = segments
            .last()
            .and_then(|s| s.strip_suffix(".md"))
            .or_else(|| segments.last().copied())
            .unwrap_or("skill")
            .to_string();
        let skill_name = skill_name.to_lowercase().replace(' ', "-");
        if !crate::user_rag::is_safe_relative_filename(&skill_name) {
            return None;
        }
        Some(ParsedSkillUrl {
            raw_skill_url,
            skill_name,
            api_path: None,
        })
    }
}

/// Extract required CLI commands (bins) from SKILL.md front matter.
/// Looks for metadata.<key>.requires.bins or requires.bins (array of strings). Falls back to [skill_name] for CLI skills.
fn skill_required_bins(skill_md_content: &str, skill_name: &str) -> Vec<String> {
    let yaml_str = match skill_md_content.strip_prefix("---") {
        Some(r) => r,
        None => return vec![skill_name.to_string()],
    };
    let end = match yaml_str.find("\n---") {
        Some(i) => i,
        None => return vec![skill_name.to_string()],
    };
    let yaml_str = yaml_str[..end].trim();
    let value: serde_yaml::Value = match serde_yaml::from_str(yaml_str) {
        Ok(v) => v,
        Err(_) => return vec![skill_name.to_string()],
    };
    fn bins_from_value(v: &serde_yaml::Value) -> Option<Vec<String>> {
        let arr = v.get("bins")?.as_sequence()?;
        let list: Vec<String> = arr
            .iter()
            .filter_map(|a| a.as_str().map(String::from))
            .collect();
        if list.is_empty() {
            None
        } else {
            Some(list)
        }
    }
    if let Some(requires) = value.get("requires") {
        if let Some(bins) = bins_from_value(requires) {
            return bins;
        }
    }
    if let Some(metadata) = value.get("metadata").and_then(|m| m.as_mapping()) {
        for (_key, val) in metadata {
            if let Some(requires) = val.get("requires") {
                if let Some(bins) = bins_from_value(requires) {
                    return bins;
                }
            }
        }
    }
    vec![skill_name.to_string()]
}

/// Extract the Markdown body (instructions) from SKILL.md content (after the second ---).
fn skill_md_body(content: &str) -> &str {
    let rest = match content.strip_prefix("---") {
        Some(r) => r,
        None => return content,
    };
    let body_start = match rest.find("\n---") {
        Some(i) => 3 + 1 + i + 4, // "---" + "\n" + "---" + "\n" after second ---
        None => return content,
    };
    content.get(body_start..).unwrap_or(content).trim()
}

/// Fetch additional files from a GitHub repo path (scripts, references, etc.) and write into skill_dir. Uses a queue to avoid recursive async.
async fn fetch_github_skill_extra_files(
    client: &reqwest::Client,
    owner: &str,
    repo: &str,
    branch: &str,
    root_path: &str,
    skill_dir: &Path,
) -> Vec<String> {
    let mut downloaded = Vec::new();
    let mut queue: Vec<(String, PathBuf)> = vec![(root_path.to_string(), skill_dir.to_path_buf())];
    while let Some((path, dir)) = queue.pop() {
        let url = format!(
            "https://api.github.com/repos/{}/{}/contents/{}?ref={}",
            owner, repo, path, branch
        );
        let resp = match client.get(&url).header("User-Agent", "Akasha").send().await {
            Ok(r) if r.status().is_success() => r,
            _ => continue,
        };
        let items: Vec<serde_json::Value> = match resp.json().await {
            Ok(arr) => arr,
            Err(_) => continue,
        };
        for item in items {
            let name = item.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let typ = item.get("type").and_then(|t| t.as_str()).unwrap_or("file");
            if !crate::user_rag::is_safe_relative_filename(name) {
                continue;
            }
            if typ == "file" && name != "SKILL.md" {
                let download_url = item.get("download_url").and_then(|u| u.as_str());
                if let Some(url) = download_url {
                    if let Ok(resp) = client.get(url).send().await {
                        if let Ok(content) = resp.text().await {
                            let dest = dir.join(name);
                            if std::fs::write(&dest, &content).is_ok() {
                                let rel = if path == root_path {
                                    name.to_string()
                                } else {
                                    format!(
                                        "{}/{}",
                                        path.strip_prefix(root_path)
                                            .unwrap_or(path.as_str())
                                            .trim_start_matches('/'),
                                        name
                                    )
                                };
                                downloaded.push(rel);
                            }
                        }
                    }
                }
            } else if typ == "dir" {
                let subpath = if path.is_empty() {
                    name.to_string()
                } else {
                    format!("{}/{}", path, name)
                };
                let subdir = dir.join(name);
                let _ = std::fs::create_dir_all(&subdir);
                queue.push((subpath, subdir));
            }
        }
    }
    downloaded
}

/// Install a skill from a URL (Agent Skills spec: https://agentskills.io/specification).
/// Supports GitHub (with directory listing), or any HTTPS host allowed in tools_policy (allowed_skill_install_hosts).
/// Fetches SKILL.md, optional scripts/references/assets (GitHub only), writes to data_dir/skills/<name>/,
/// reloads the registry, adds required commands to tools_policy.yaml, and optionally hot-reloads the in-memory policy.
pub(crate) async fn do_install_skill(
    url: &str,
    data_dir: &Path,
    spec_dir: &Path,
    skill_registry: &crate::skills::SkillRegistry,
    allowed_hosts: &[String],
    tools_reload: Option<(
        &std::sync::Arc<tokio::sync::RwLock<std::sync::Arc<akasha_tools::ToolExecutor>>>,
        &Path,
    )>,
) -> (bool, String) {
    let parsed = match parse_skill_install_url(url, allowed_hosts) {
        Some(x) => x,
        None => {
            let hint = if allowed_hosts.iter().any(|h| h == "*") {
                "URL invalide ou schéma non supporté (utilisez https://).".to_string()
            } else {
                format!(
                    "URL non autorisée ou invalide. Hôtes autorisés (tools_policy.yaml allowed_skill_install_hosts) : {}.",
                    allowed_hosts.join(", ")
                )
            };
            return (false, format!("[install_skill] {}", hint));
        }
    };
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent("Akasha")
        .build()
    {
        Ok(c) => c,
        Err(e) => return (false, format!("[install_skill] client HTTP: {}", e)),
    };
    let body = match client.get(&parsed.raw_skill_url).send().await {
        Ok(r) if r.status().is_success() => match r.text().await {
            Ok(t) => t,
            Err(e) => return (false, format!("[install_skill] lecture réponse: {}", e)),
        },
        Ok(r) => {
            return (
                false,
                format!(
                    "[install_skill] HTTP {} — {}",
                    r.status(),
                    parsed.raw_skill_url
                ),
            )
        }
        Err(e) => return (false, format!("[install_skill] requête: {}", e)),
    };
    if body.trim().is_empty() {
        return (
            false,
            format!(
                "[install_skill] SKILL.md vide ou introuvable: {}",
                parsed.raw_skill_url
            ),
        );
    }
    let skill_dir = data_dir.join("skills").join(&parsed.skill_name);
    if let Err(e) = std::fs::create_dir_all(&skill_dir) {
        return (
            false,
            format!(
                "[install_skill] impossible de créer le dossier {}: {}",
                skill_dir.display(),
                e
            ),
        );
    }
    let skill_md_path = skill_dir.join("SKILL.md");
    if let Err(e) = std::fs::write(&skill_md_path, &body) {
        return (
            false,
            format!(
                "[install_skill] écriture {}: {}",
                skill_md_path.display(),
                e
            ),
        );
    }
    let extra_files = if let Some((ref owner, ref repo, ref branch, ref path)) = parsed.api_path {
        fetch_github_skill_extra_files(&client, owner, repo, branch, path, &skill_dir).await
    } else {
        vec![]
    };
    match skill_registry.reload(data_dir, spec_dir).await {
        Ok(count) => {
            let pin_ref = parsed
                .api_path
                .as_ref()
                .map(|(_, _, branch, _)| branch.clone())
                .unwrap_or_default();
            let digest = hex::encode(Sha256::digest(body.as_bytes()));
            append_skills_lock_entry(
                data_dir,
                &serde_json::json!({
                    "v": 1,
                    "skill_name": parsed.skill_name,
                    "source_url": url,
                    "raw_skill_url": parsed.raw_skill_url,
                    "ref": pin_ref,
                    "skill_md_sha256": digest,
                    "installed_at": chrono::Utc::now().to_rfc3339(),
                }),
            );
            let body_instructions = skill_md_body(&body);
            let total_chars = body_instructions.chars().count();
            let body_preview = if total_chars > 8000 {
                let truncated: String = body_instructions.chars().take(8000).collect();
                format!(
                    "{}... [tronqué, {} caractères au total]",
                    truncated, total_chars
                )
            } else {
                body_instructions.to_string()
            };
            let extra_msg = if extra_files.is_empty() {
                String::new()
            } else {
                format!(
                    " Fichiers additionnels récupérés (scripts/, references/, assets/) : {}.",
                    extra_files.join(", ")
                )
            };
            let (mut commands_added_msg, need_hot_reload) = {
                let bins = skill_required_bins(&body, &parsed.skill_name);
                let policy_path = data_dir.join("tools_policy.yaml");
                if let Ok(mut policy) = akasha_tools::ToolsPolicy::load_from_path(&policy_path) {
                    let added = policy.add_allowed_commands(&bins);
                    if !added.is_empty() {
                        if let Err(e) = policy.save_to_path(&policy_path) {
                            (format!(" Commandes {} non ajoutées à tools_policy.yaml (écriture: {}).", added.join(", "), e), false)
                        } else {
                            let added_joined = added.join(", ");
                            (format!(" Commande(s) ajoutée(s) à tools_policy.yaml (allowed_commands) : {}.", added_joined), true)
                        }
                    } else {
                        (String::new(), false)
                    }
                } else {
                    (String::new(), false)
                }
            };
            if need_hot_reload {
                if let Some((r, path)) = tools_reload {
                    if reload_tools_executor_policy(r, path, data_dir).await.is_ok() {
                        commands_added_msg = commands_added_msg
                            .replace(" (allowed_commands) :", " ; politique rechargée à chaud :");
                    }
                }
            }
            let skill_dir_display = skill_dir.display().to_string();
            let msg = format!(
                "[install_skill] Skill « {} » installé et rechargé ({} skill(s) chargé(s)).{}{}\n\
                 Répertoire du skill (pour read_file sur references/, scripts/, assets/) : {}\n\n\
                 Contenu du skill (à utiliser pour savoir comment l'utiliser) :\n\n---\n{}",
                parsed.skill_name,
                count,
                extra_msg,
                commands_added_msg,
                skill_dir_display,
                body_preview
            );
            (true, msg)
        }
        Err(e) => (
            false,
            format!(
                "[install_skill] skill écrit mais rechargement échoué: {}",
                e
            ),
        ),
    }
}

/// Uninstall a skill by name: remove data_dir/skills/<name>, remove command from tools_policy, reload registry (and optionally hot-reload executor).
pub(crate) async fn do_uninstall_skill(
    name: &str,
    data_dir: &Path,
    spec_dir: &Path,
    skill_registry: &crate::skills::SkillRegistry,
    tools_reload: Option<(
        &std::sync::Arc<tokio::sync::RwLock<std::sync::Arc<akasha_tools::ToolExecutor>>>,
        &Path,
    )>,
) -> (bool, String) {
    let name = name.trim();
    if name.is_empty() {
        return (
            false,
            "[uninstall_skill] usage: uninstall_skill <name> (ex. uninstall_skill bankr)"
                .to_string(),
        );
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return (
            false,
            "[uninstall_skill] nom invalide (utiliser uniquement lettres, chiffres, _ et -)"
                .to_string(),
        );
    }
    let skill_dir = data_dir.join("skills").join(name);
    if !skill_dir.exists() {
        return (
            false,
            format!(
                "[uninstall_skill] skill « {} » introuvable dans data_dir/skills/ (dossier absent).",
                name
            ),
        );
    }
    if !skill_dir.is_dir() {
        return (
            false,
            format!(
                "[uninstall_skill] « {} » n'est pas un répertoire.",
                skill_dir.display()
            ),
        );
    }
    if let Err(e) = std::fs::remove_dir_all(&skill_dir) {
        return (
            false,
            format!(
                "[uninstall_skill] impossible de supprimer {}: {}",
                skill_dir.display(),
                e
            ),
        );
    }
    let policy_path = data_dir.join("tools_policy.yaml");
    let mut policy_updated = false;
    if policy_path.exists() {
        if let Ok(mut policy) = akasha_tools::ToolsPolicy::load_from_path(&policy_path) {
            if policy.remove_allowed_command(name) {
                if policy.save_to_path(&policy_path).is_ok() {
                    policy_updated = true;
                }
            }
        }
    }
    if policy_updated {
        if let Some((r, path)) = tools_reload {
            let _ = reload_tools_executor_policy(r, path, data_dir).await;
        }
    }
    match skill_registry.reload(data_dir, spec_dir).await {
        Ok(count) => {
            let policy_msg = if policy_updated {
                format!(
                    " Commande « {} » retirée de tools_policy.yaml (allowed_commands).",
                    name
                )
            } else {
                String::new()
            };
            (
                true,
                format!(
                    "[uninstall_skill] Skill « {} » désinstallé ({} skill(s) chargé(s) restants).{}",
                    name, count, policy_msg
                ),
            )
        }
        Err(e) => (
            false,
            format!(
                "[uninstall_skill] dossier supprimé mais rechargement du registry échoué: {}",
                e
            ),
        ),
    }
}

pub(crate) const WRITE_FILE_REMINDER: &str = "\n[Reminder: the user is asking to save a file. You MUST reply ONLY with a header line TOOL: write_file <full_path>, then the file content on the following lines. Do not put the full file content on the same TOOL line. Never say you cannot write to disk.]\n\n";

pub(crate) const WEB_SEARCH_REMINDER: &str = "\n[Reminder: the user is asking for external information (weather/météo, news, etc.). You MUST use TOOL: web_search <query> first — do NOT use bankr or portfolio for weather. If search snippets do not contain the precise facts (temperatures, sky state, rain risk, figures, tables), you MUST follow up with TOOL: web_fetch <url> on a relevant result URL, or TOOL: browser navigate <url> then TOOL: browser snapshot for JS-heavy or dynamic pages (e.g. many weather portals). Do not end by telling the user to visit links yourself if web_fetch or browser snapshot is available in your tool list and policy allows those domains — fetch and summarize. Do not suggest visiting a site without having used web_search first.]\n\n";

/// Injected only when web_search is available and policy allows web_fetch and/or configured browser follow-up.
pub(crate) const WEB_SEARCH_FOLLOWUP_REMINDER: &str = "\n[Reminder — page fetch: Search snippets are often incomplete. After web_search, if you still lack concrete details, call web_fetch on a result URL and/or browser navigate + browser snapshot (then answer from that output). Do not reply with only URLs for the user to open when these tools work.]\n\n";

/// Reminder injected when the user asks for external information (weather, news, etc.) but
/// web_search is not available in the current tools policy. Prevents the model from ignoring
/// the question and falling back to a generic capability introduction.
pub(crate) const WEB_SEARCH_UNAVAILABLE_REMINDER: &str = "\n[Note: the user is asking for weather, news, or other live external information. web_search is not currently enabled. Answer as best you can from your training knowledge, clearly state that the data may be outdated, and explain how to enable web search: set web_search_enabled: true in tools_policy.yaml (SearXNG/DuckDuckGo work without API keys; optional Brave/Tavily/Serper keys in vault or env). Do NOT respond with a generic capabilities introduction — address the user's question directly.]\n\n";

pub(crate) const TRANSPORT_REMINDER: &str = "\n[Reminder: the user is asking about transport schedules, routes, or travel information. You MUST use TOOL: web_search <query> first (e.g. web_search \"horaires train Angoulême Paris CDG dimanche\"). Do NOT write any files, generate HTML, or ask about project file paths — the user wants travel information only. If web_search is unavailable, say so clearly and suggest the relevant site (e.g. sncf.com, ratp.fr, transilien.com).]\n\n";

/// Transport reminder when web_search is not enabled: model cannot use the tool so we only anchor it to the domain.
pub(crate) const TRANSPORT_REMINDER_NO_SEARCH: &str = "\n[Reminder: the user is asking about transport routes or travel (train, car, bus, etc.). Do NOT write any files, create HTML pages, or ask about project file paths — the user wants travel information only. Answer from your knowledge (e.g. compare train vs car for this route). If you cannot give accurate live schedules, say so clearly and suggest the relevant site (e.g. sncf.com, ratp.fr, transilien.com).  Do NOT ask about file paths or project details — this is a travel question.]\n\n";

/// Generic distance reminder used when no plugin-specific routing rule matched.
pub(crate) const GEO_DISTANCE_REMINDER_WITH_TOOLS: &str = "\n[Reminder: the user asks for a geographic distance/route between places. PRIORITY: use a relevant distance/route tool available in the current tools list. If location details are missing, use TOOL: web_search with a focused distance query. STRICTLY FORBIDDEN for this request: memory_store, project planning, code generation, file writes, and unrelated queries.]\n\n";

pub(crate) const GEO_DISTANCE_REMINDER_NO_TOOL: &str = "\n[Reminder: the user asks for a geographic distance/route between places. No distance/search tool is available. Provide a concise best-effort estimate and clearly state uncertainty. Do NOT write files, ask for project paths, or perform unrelated tasks.]\n\n";

/// X/Twitter/social feed fetches: do not use ask_user for unrelated onboarding; use tools first.
pub(crate) const SOCIAL_FEED_REMINDER: &str = "\n[Reminder: SOCIAL / X / TWITTER — PRIORITY: The user wants posts, tweets, or timeline content from X (Twitter) or similar. Do NOT use TOOL: ask_user for generic greetings or unrelated menu choices — fulfill this request with tools. First TOOL: web_search <query> (e.g. site:x.com handle latest posts). If results are empty or insufficient, use TOOL: browser navigate <profile URL> then TOOL: browser snapshot (if browser is enabled in policy). Do not answer \"no context\" or \"blocked\" without having called web_search or browser.]\n\n";

pub(crate) const DEVICE_CAMERA_REMINDER: &str = "\n[Reminder: webcam/camera photo request. You MUST chain directly: TOOL: device_discover local_media then TOOL: device_invoke local_media camera capture. Do NOT ask the user \"which device action?\" with ask_user — they already said they want a photo; call device_invoke camera capture. Do NOT suggest: file upload, open UI, AI image. Do NOT mention tools_policy.yaml or allowed_write_paths for this request: the user wants a camera photo, not to configure file writing. If the user asked to \"display the photo in the chat\", after capture reply ONLY with a short confirmation in their language (e.g. \"Photo captured. It is shown below.\"): do NOT suggest \"save to file\", \"get a description\", \"take another photo\" or \"What would you like to do next?\" — the image is added automatically below your reply. Reply in the same language as the user.]\n\n";
pub(crate) const IMAGE_GENERATION_REMINDER: &str = "\n[Reminder: request to \"generate an image\", \"draw\", \"create an image\" (by AI, not webcam). You MUST use TOOL: generate_image <prompt> (e.g. TOOL: generate_image a cat on a sofa). Spec 42.]\n\n";

/// Reminder when the user asks for GitHub (private repo / API) and mentions the vault (e.g. GITHUB_TOKEN).
pub(crate) const GITHUB_VAULT_REMINDER: &str = "\n[Reminder GitHub + vault: you MUST run the request yourself via TOOL: run_command. Exact format: TOOL: run_command VAULT:GITHUB_TOKEN=GITHUB_TOKEN curl -sS -H \"Authorization: Bearer $GITHUB_TOKEN\" https://api.github.com/repos/owner/repo (or gh repo view owner/repo). FORBIDDEN: telling the user to do GITHUB_TOKEN=VAULT:... or export GITHUB_TOKEN=... or to put the token in plain text — you must emit the TOOL: line so the system injects the secret. Do not reply \"I did not find\" without having called run_command with VAULT:GITHUB_TOKEN=GITHUB_TOKEN.]\n\n";

/// Injected when the message looks like code/script work: prefer workspace paths, git/diff tools, and explicit cwd for commands.
pub(crate) const CODE_DEV_SANDBOX_REMINDER: &str = "\n[Reminder — code / project work: use workspace:/ paths for files in this task when no absolute path is given. For Git operations prefer TOOL: git_status, git_diff, git_log, git_rev_parse on the repo path (e.g. workspace:/ or an allowed folder) instead of raw git via run_command, unless you need a subcommand not covered. For file comparison use diff_unified or file_diff; for two trees use dir_compare. For build/test commands use TOOL: run_command --cwd workspace:/ cargo test (or npm test, etc.) so the command runs in the project root; or set run_command_default_cwd_workspace: true in tools_policy.yaml. For isolated execution with a toolchain image, use run_in_container when policy allows.]\n\n";
pub(crate) const STUDIO_DISK_REMINDER: &str = "\n[Code Studio — périmètre disque: cette tâche s'exécute sous le dossier projet studio uniquement (miroir workspace:/ et cwd des outils). Ne pas cibler de chemins hors de ce répertoire. Pour npm install / builds à risque, privilégier run_in_container si la politique d'outils l'autorise. Renommer/déplacer un dossier: si `run_command` est autorisé, utiliser `git mv` ou `mv` avec `--cwd workspace:/` puis corriger tous les imports; sinon `read_file` chaque fichier concerné puis `write_file workspace:/nouveau/chemin/...` (les répertoires parents sont créés) et `grep_content`/`search_replace` pour les imports — ne pas boucler sur une tactique qui ne modifie pas réellement les chemins sur disque.]\n\n";

/// Injected with STUDIO_DISK_REMINDER: raise quality bar and user-visible wrap-up for Code Studio agents.
pub(crate) const STUDIO_AGENT_QUALITY_REMINDER: &str = concat!(
    "\n[Code Studio — exigences avant de considérer la demande comme terminée]\n",
    "- Quand tu écris un fichier, son contenu doit être STRICTEMENT le contenu attendu du fichier (code, JSON, Markdown, config). ",
    "Interdiction d'y ajouter du texte conversationnel, des explications, des statuts, des raisonnements, ou des phrases comme ",
    "\"fichier corrigé\", \"je relance le build\", \"voici la correction\". Ces messages vont uniquement dans la réponse chat.\n",
    "- Pour les fichiers de code (ex: .ts, .tsx, .js, .rs, .py), n'écris que du code syntaxiquement valide pour ce langage ; ",
    "ne mets jamais de prose libre hors commentaires valides du langage.\n",
    "- Interdiction dans les fichiers .ts / .tsx / .js / .jsx : lignes « documentation » pour l’utilisateur (titres markdown `**…**`, listes du type « 4. fichier.ts — … », résumés de lot) — le daemon **rejette** l’écriture ; ce texte va **uniquement** dans le chat.\n",
    "- Ne déclare PAS la tâche terminée tant que le livrable n'est pas vérifié quand c'est possible : lance un build ou des tests ",
    "via TOOL: run_command avec --cwd workspace:/ (ou la racine du projet) quand la politique d'outils le permet — ",
    "par ex. npm run build, npm test, cargo build, cargo test, pytest, tsc --noEmit. Corrige les erreurs de compilation ",
    "ou de typage que tu peux corriger sans dériver du besoin utilisateur.\n",
    "- Après des changements qui touchent au comportement à l'exécution, exécute aussi une vérification d'exécution minimale ",
    "quand c'est raisonnable (tests automatisés, ou une commande courte qui exerce le chemin modifié avec timeout). ",
    "Ne te contente pas d'un build seul si la demande porte sur un bug ou un comportement runtime.\n",
    "- Si un build complet est trop lourd ou bloqué par la politique, exécute au moins une vérification ciblée (lint, typecheck) ",
    "ou explique clairement ce que tu n'as pas pu valider et pourquoi.\n",
    "- Ta dernière réponse à l'utilisateur (même langue que lui) doit résumer en langage accessible : ce qui a été ajouté ou modifié, ",
    "comment lancer ou essayer le résultat, et les limites éventuelles. Pas de jargon inutile sauf si l'utilisateur demande le détail technique.\n",
    "- N'achève pas seulement par « Terminé » / « Done » : fournis un paragraphe utile lisible sans ouvrir les fichiers.\n",
    "- Après un **échec de tests** ou de build : lire la sortie d’erreur ; corriger le **minimum** de fichiers (idéalement ceux cités par la stack trace) ; **interdiction** de refactoriser ou réécrire tout le projet « au hasard ». Ne pas modifier les fichiers de tests sauf demande explicite de l’utilisateur. Si trois tentatives ciblées échouent encore, utiliser `ask_user` plutôt que de boucler.\n",
    "- Fichier `CODE_STUDIO_PLAN.md` (racine) : **gabarit fixe** — ligne d'ouverture `# Titre : …` puis dans l'ordre les sections `## Description`, `## Scope`, `## Stack`, `## Structure du projet`, `## Commandes`, `## Fichiers hors scope`, `## Demandes d'évolutions utilisateur par phase`, `## Recommandations`, `## Todos`, `## Informations complémentaires` (conserver ces titres et cet ordre).\n",
    "  Avant d'écrire : `read_file workspace:/CODE_STUDIO_PLAN.md`. Ne **pas** remplacer tout le fichier pour une modification ciblée : mettre à jour **par section** (search_replace ciblé ou une seule section réécrite), en conservant les titres `##` et le reste inchangé.\n",
    "  Ne **jamais** dupliquer une section `## …` déjà présente (pas de second gabarit collé en bas du fichier) : le daemon rejette les écritures qui répètent les titres de section.\n",
    "  Suivi des lots : ajouter une **ligne datée courte** dans `## Informations complémentaires` ou `## Demandes d'évolutions utilisateur par phase` plutôt que de réécrire l'ensemble du plan.\n",
    "  Si le fichier est absent (import), le créer avec ce gabarit en synthétisant le dépôt. Remplacement complet réservé à une demande **explicite** de réinitialisation du plan (bouton ou consigne utilisateur).\n",
    "- Premier lot d'un projet Code Studio : après avoir créé ou mis à jour `CODE_STUDIO_PLAN.md`, créer `workspace:/DESIGN.md` **avant** les développements applicatifs si le fichier est absent. `DESIGN.md` doit fixer le contrat design (front matter YAML + sections markdown) à partir de la demande, de la stack et du plan ; ensuite seulement générer/modifier `src/`, configs, tests, etc.\n",
    "- **Corrections sur le disque (obligatoire quand les outils le permettent)** : pour corriger du code (imports, erreurs TS/build, etc.), utiliser des lignes `TOOL:` — `search_replace` pour des changements localisés, `edit_file` pour un intervalle de lignes, `write_code` pour créer ou remplacer un **fichier source** entier (.ts, .tsx, .rs, …), `write_file` pour plans Markdown / JSON / config, `apply_patch` si adapté. ",
    "Ne pas faire du **chat** le canal principal de livraison : éviter « voici le fichier corrigé à coller dans workspace:/… », les longs blocs de remplacement manuel ou les résumés à la place d’écritures réelles tant que la politique d’outils autorise les écritures.\n",
    "- **Si une écriture est impossible** (outil refusé, erreur explicite de `write_file` / `write_code` / `search_replace` / etc., chemin hors périmètre) : indiquer **pourquoi** tu ne peux pas appliquer la correction toi-même (citer le message d’erreur ou la contrainte), puis seulement proposer un secours (diff, extrait à copier).\n\n",
);

/// Contexte système court pour les tâches dont le disque outil est sous `studio-projects/` (Code Studio).
/// Remplace le bloc général `APP_CONTEXT` (TUI, skills globales, caméra, etc.).
pub(crate) const CODE_STUDIO_APP_CONTEXT: &str = concat!(
    "[Code Studio — contexte]\n",
    "Tu travailles sur le dépôt du projet ouvert dans Akasha Code Studio. ",
    "Chemins : préfère `workspace:/…` (racine virtuelle de la tâche) ; les fichiers sont synchronisés sur le disque du projet studio.\n",
    "Outils usuels : read_file, write_file, write_code (fichiers source uniquement), delete_file, rename_path, move_tree (si autorisés), search_replace, edit_file, apply_patch, run_command (avec `--cwd workspace:/` pour builds/tests), git_* si exposés, ask_user pour une question bloquante dans la même tâche.\n",
    "Concentre-toi sur le code et la documentation de ce dépôt — pas sur l’interface générale d’Akasha (TUI, onglets, skills hors projet, caméra, météo). ",
    "Si une capacité externe est indispensable, indique brièvement ce qu’il faudrait côté utilisateur (clé, politique d’outils).\n",
    "Réponds dans la même langue que le dernier message utilisateur. ",
    "Avant d’éditer : lire les fichiers concernés ; ne pas inventer de dépendances — vérifier le manifeste (package.json, Cargo.toml, etc.).\n",
    "Sur le premier lot d’un projet : stabiliser d’abord `CODE_STUDIO_PLAN.md`, puis créer `workspace:/DESIGN.md` avant de commencer le développement applicatif si ce fichier est absent.\n",
    "Corrections : appliquer les changements sur le dépôt avec les outils (`search_replace`, `edit_file`, `write_code` pour le code source, `write_file` pour markdown/json/config, `apply_patch`, chemins `workspace:/…`) — ne pas se contenter de décrire ou coller un fichier entier pour que l’utilisateur le fasse à ta place. ",
    "Si un outil d’écriture échoue ou est interdit, expliquer clairement la raison avant toute solution de secours.\n\n",
);

/// Application context injected into the prompt: the agent knows it runs inside Akasha and can talk about it.
pub(crate) const APP_CONTEXT: &str = concat!(
    "[Akasha context] You are the assistant embedded in Akasha. Akasha is the application you are currently running in. ",
    "If the user talks about Akasha, the program, the app or how it works, you can explain: ",
    "commands (akasha start, akasha init, akasha doctor), interfaces (TUI with Chat/Router/Memory/Doc/Activity tabs), ",
    "slash commands in Chat (/help, /status, /doctor, /advice, /config, /models, /routes, /newsession, /skills reload, etc.). ",
    "To install a CLI globally (e.g. \"install the bankr CLI\", \"npm install -g @bankr/cli\"), reply with TOOL: run_command npm install -g <package> (do not generate a script for the user to run). ",
    "To use a vault key in a command: TOOL: run_command VAULT:bankr_api_key=BANKR_API_KEY bankr whoami (the system injects the vault value). ",
    "GitHub + vault: run TOOL: run_command VAULT:GITHUB_TOKEN=GITHUB_TOKEN curl -sS -H \"Authorization: Bearer $GITHUB_TOKEN\" https://api.github.com/repos/owner/repo (not GITHUB_TOKEN=VAULT:... or export or plain token). If gh-axi is installed (npm install -g gh-axi), prefer it for issues/PRs/repos — token-efficient CLI per AXI (https://axi.md/, https://github.com/kunchenguid/axi). ",
    "Browser automation from shell: chrome-devtools-axi (same repo) can complement Akasha TOOL: browser navigate + browser snapshot for heavy browsing tasks; use whichever fits policy and environment. ",
    "Skills (extra capabilities): the user can add them without changing code. When the user asks to install, download, fetch or add a skill from a URL (e.g. \"install the bankr skill from …\", \"download the skill at this url\"), you MUST reply with TOOL: install_skill <url>. If the user says \"follow the SKILL.md instructions\", you must first do TOOL: install_skill <url> (the system registers the skill); then the skill is available and can be invoked by name (e.g. TOOL: security-audit <args>). Do not fetch SKILL.md with web_fetch to execute its content manually. ",
    "To uninstall a skill: TOOL: uninstall_skill <name> (e.g. TOOL: uninstall_skill bankr). ",
    "When the user asks you to perform an action with a skill (e.g. \"check my bankr wallet\", \"run bankr whoami\"), you MUST reply ONLY with one line TOOL: <skill_name> <arguments> (e.g. TOOL: bankr whoami) so the system runs the command; do not tell the user to run the command themselves. ",
    "Otherwise the user can place files in the skills folder and run /skills reload. ",
    "Full documentation is available in the Doc tab of the interface. ",
    "Language: ALWAYS reply in the same language as the user's last message (French → French, English → English, etc.). Do not switch language even if tool results or context are in another language. ",
    "Autonomy: work autonomously until the user's request is completely resolved. Do not stop in the middle of a task to ask for confirmation unless you hit a hard blocker (missing credentials, genuinely ambiguous requirements that cannot be inferred from context). For anything you can discover via a tool (read_file, web_search, run_command, etc.), prefer the tool over asking the user. ",
    "Research before acting: when you are unsure about file contents or codebase structure, use read_file and search tools before editing or answering. Never guess or invent code — your answer must be grounded in actual research. ",
    "Code conventions: when editing code, first read the file to understand its existing conventions, imports, and style. Mimic existing patterns. Never assume a library or dependency is available — verify it is already declared in the project's dependency file (Cargo.toml, package.json, requirements.txt, etc.) before using it. ",
    "Tests discipline: when tests fail, never modify the tests themselves unless the user explicitly asks you to. Assume the bug is in the code under test. If the same test or CI still fails after three consecutive attempts, stop and ask the user for guidance rather than iterating blindly. ",
    "Debugging: when debugging, address the root cause rather than the symptoms. Add descriptive logging statements to track variable state. If multiple approaches have failed, step back and think big-picture before making more changes. ",
    "Never invent data. If you do not have the information to answer, say so clearly (e.g. \"I did not find that information\"). ",
    "For questions about information you do not have (weather, forecasts, news, schedules, etc.), you must use the web_search tool first, then if snippets are insufficient use web_fetch and/or browser navigate plus browser snapshot to read the page itself and reply with the synthesized facts. When you have just received tool results (e.g. web_search, web_fetch, browser snapshot), you must answer immediately with the synthesized result — do not reply with a promise (e.g. \"I will fetch…\", \"Action in progress\"); the task ends after your message, so give the actual answer. ",
    "Do not suggest the user visit a site without having used web_search first if you have access to that tool; do not only list URLs for the user when web_fetch or browser snapshot can retrieve the content. ",
    "If web_search returns an error (e.g. not enabled), you can then suggest sites and explain how to enable web search (tools_policy.yaml, web_search_enabled; keyless SearXNG/DuckDuckGo or optional API keys). ",
    "Playwright / managed browser: the daemon may auto-install Chromium on first browser use unless AKASHA_PLAYWRIGHT_AUTO_INSTALL=0. If browser fails for missing Chromium, the runner is missing, or the user must explicitly approve a large download, use TOOL: ask_user (e.g. choices agreeing to install), then TOOL: install_playwright. List install_playwright in tool_profiles when using a profile. tools_policy require_approval can include install_playwright for UI approval before the install runs. For other dependencies (npm, cargo, etc.), use run_command with allowed_commands after ask_user consent. ",
    "You have access to the write_file tool: you MUST use it whenever the user asks to save, store or write a file (e.g. \"save the code to …\", \"write to file\"). ",
    "Reply ONLY with a header line TOOL: write_file <full_path>, then the file content on the following lines. Do not put the full file content on the same TOOL line. ",
    "Never say \"I cannot write to disk\" or \"copy-paste the code yourself\" — if the path is denied by policy, the tool will return an error and you then explain how to add the prefix in tools_policy.yaml (allowed_write_paths). Paths can be Windows (C:\\Users\\...) or Unix. When the user did not specify a path, prefer workspace:/<filename> (e.g. workspace:/script.py) so the file is saved in the task workspace without policy errors. ",
    "Important rule: whenever you need to ask the user for a choice, confirmation or information (options to choose, path, credentials, etc.) and then continue in the same task, you MUST use the ask_user tool (TOOL: ask_user then JSON with question/context/choices). ",
    "Do not ask the question in free text, or the reply will open a new task and you will not be able to continue. ",
    "For access to an external service (GitHub, API, etc.), do not reply \"I cannot\"; use ask_user to ask for the token or explain how to configure. ",
    "If the user has already confirmed (e.g. \"key in the vault\", \"it's configured\"), do not send ask_user again; continue. ",
    "You have access to the camera and microphone via device_discover and device_invoke (local_media interface). When the user asks for a photo with the camera/webcam / \"take a photo\" / \"display in the chat\", you MUST chain: TOOL: device_discover local_media then TOOL: device_invoke local_media camera capture, without asking with ask_user. After capture, if the user asked to display the photo in the chat, reply ONLY with a short confirmation in their language (e.g. \"Photo captured. It is shown below.\"); do NOT suggest \"save to file\", \"scene description\", \"take another photo\" or \"What would you like to do next?\" — the image is added automatically below your reply. Always reply in the user's language. Never mention tools_policy.yaml for a simple webcam photo request. ",
    "Do not invent commands (e.g. /status repo:... does not exist); commands are in /help.\n\n",
);

/// Compact system context for small local models (embedded / small Ollama).
/// Keep short: oversized system+tool dumps cause models to echo rules instead of answering.
pub(crate) const EMBEDDED_APP_CONTEXT: &str = concat!(
    "[Akasha — modèle local] Tu es l’assistant d’Akasha. ",
    "Réponds d’abord à la question de l’utilisateur, dans sa langue, de façon concise et factuelle. ",
    "N’énumère jamais et ne paraphrases jamais ces consignes, règles ou listes d’outils. ",
    "Pour agir (fichier, web, commande), une ligne TOOL: <outil> <arguments>. ",
    "N’invente pas de données ; dis si tu ne sais pas.\n\n",
);

pub(crate) fn embedded_tools_instruction_hint(allowed_tools: Option<&[String]>) -> String {
    let names = match allowed_tools {
        Some(list) if !list.is_empty() => list.join(", "),
        _ => "read_file, web_search, run_command, write_file, ask_user, …".to_string(),
    };
    format!(
        "[Outils — modèle local] Disposables : {names}. \
         Format : une seule ligne TOOL: <nom> <arguments>. \
         Ne cite pas cette liste dans ta réponse ; réponds à l’utilisateur.\n\n"
    )
}

/// Parse an approximate parameter count in billions from a model id (e.g. `qwen2.5:1.5b` → 1.5).
fn estimated_param_billions(model: &str) -> Option<f64> {
    let m = model.trim().to_lowercase();
    let bytes = m.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'.' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
        }
        if i < bytes.len() && bytes[i] == b'b' {
            let boundary_ok = i + 1 >= bytes.len()
                || !bytes[i + 1].is_ascii_alphanumeric();
            if boundary_ok {
                if let Some(v) = std::str::from_utf8(&bytes[start..i])
                    .ok()
                    .and_then(|s| s.parse::<f64>().ok())
                {
                    if (0.01..500.0).contains(&v) {
                        return Some(v);
                    }
                }
            }
        }
    }
    None
}

/// True when the primary route should use the slim local prompt (avoids rule-echo on tiny models).
/// Override with `AKASHA_COMPACT_LOCAL_PROMPT=1|0`.
pub fn should_use_compact_local_prompt(provider: &str, model: &str) -> bool {
    match std::env::var("AKASHA_COMPACT_LOCAL_PROMPT")
        .ok()
        .map(|s| s.trim().to_lowercase())
        .as_deref()
    {
        Some("1") | Some("true") | Some("yes") | Some("on") => return true,
        Some("0") | Some("false") | Some("no") | Some("off") => return false,
        _ => {}
    }
    let p = provider.trim().to_lowercase();
    if p == "akasha_embedded" || p == "akasha_core" {
        return true;
    }
    if p != "ollama" {
        return false;
    }
    let m = model.trim().to_lowercase();
    if m.is_empty() {
        return true;
    }
    // Known tiny / nano families (avoid bare "phi"/"gemma" — those also name larger variants).
    const SMALL_NAMES: &[&str] = &[
        "tinyllama",
        "tinydolphin",
        "smollm",
        "smolvlm",
        "orca-mini",
        "minicpm",
        "phi2",
        "phi-2",
        "phi3:mini",
        "phi3:3.8b",
        "phi4-mini",
        "gemma:2b",
        "gemma2:2b",
        "stablelm-zephyr",
        "qwen2.5:0.5b",
        "qwen3:0.6b",
        "llama3.2:1b",
        "llama3.2:3b",
    ];
    if SMALL_NAMES.iter().any(|n| m.contains(n)) {
        return true;
    }
    // Local Ollama mid-size (7B–14B) still saturates on the full RULE dump and often
    // echoes policies instead of answering. Keep the full prompt for clearly large models.
    match estimated_param_billions(&m) {
        Some(b) if b <= 14.0 => true,
        Some(b) if b >= 30.0 => false,
        Some(_) => true,
        // No size tag (e.g. mistral:latest): prefer compact — override with AKASHA_COMPACT_LOCAL_PROMPT=0.
        None => true,
    }
}

/// Returns an English [Role] system prompt for the given agent type, or None for conversation/unknown.
/// Plan: Architecture agents et pipeline — Phase 2 (new roles).
pub fn agent_role_system_prompt(agent_type: &str) -> Option<&'static str> {
    match agent_type {
        "conversation" => None,
        "code" => Some("You are the code generation agent. Produce correct, readable code. Prefer write_file and workspace:/ paths for new files. For Git inspection use git_status, git_diff, git_log, git_rev_parse on the repo path when available; use diff_unified or dir_compare to compare files or trees. Run builds and tests with run_command --cwd workspace:/ (or the project root). Use run_in_container when policy allows and you need an isolated toolchain. Do not invent APIs; use read_file when needed to match existing code. When the user asks to *perform* an action (take a photo, search the web, save a file), use the appropriate TOOL; do not generate a script for that. Use code only when the user explicitly asks to *write* or *generate* code or a script. Before editing any file, read it to understand its conventions, imports, and style; mimic existing patterns. Never assume a library is available — verify it is already declared in the project dependency file (Cargo.toml, package.json, etc.). When tests fail, never modify the tests themselves; fix the code under test. If the same test still fails after three attempts, stop and ask the user for guidance."),
        "search" => Some("You are the search agent. Use web_search to find external information (weather, news, facts). When search snippets lack concrete details, use web_fetch on a result URL and/or browser navigate plus browser snapshot, then synthesize and cite sources. Do not claim information you have not retrieved via tools when they are available."),
        "financial" => Some("You are the financial specialist. Help with budgets, cost analysis, financial reports, numeric reasoning. Be precise with figures and units. Do not invent data; state what is missing if needed."),
        "documentalist" => Some("You are the documentalist. Transform a pile of files into exploitable data. Answer from the user's document base (RAG). Prioritize [User documents] and [Long-term memory]. Use memory_search when relevant. Quote or summarize from excerpts; if insufficient, say so and suggest adding documents. Produce structured summaries when asked."),
        "project_manager" => Some("You are the project manager. Help with project tracking, milestones, task breakdown, planning. Refer to schedules and recurring tasks when relevant. Propose clear next steps and deliverables."),
        "technical_writer" => Some("You are the technical writing agent. Produce clear technical documentation, procedures, tutorials. Use a structured style (headings, steps, code blocks when relevant). Prefer clarity and precision. Use write_file when the user asks to save documentation."),
        "research" => Some("You are the research agent. Perform in-depth research using web_search, memory_search, and the document base. After web_search, use web_fetch and/or browser navigate plus browser snapshot when snippets or excerpts are insufficient to answer factually. Synthesize multiple sources; cite or summarize clearly. Do not invent facts."),
        "security_audit" => Some("You are the security audit agent. Review code, config, or practices for security. Be methodical; highlight risks and suggest mitigations. Do not claim certainty where you lack context; recommend human review for critical decisions."),
        "creative" => Some("You are the creative agent. You have a strong creative sense for text and images. Produce marketing copy, creative content, stories, and audience-adapted text. Match tone and format to the requested channel and goal. When the task is to get a photo from the user's webcam/camera, use TOOL: device_invoke local_media camera capture first; for AI-generated images use generate_image."),
        "analyst" => Some("You are the product / functional analyst. Formalize the need before any production. Output: reformulated need, scope, assumptions, acceptance criteria, initial backlog. Do not jump to implementation; clarify and structure the request first."),
        "architect" => Some("You are the technical architect. Design the skeleton of the project. Output: proposed architecture, task list, dependencies between tasks, execution order, definition of done. Stay at design level; do not write full implementation."),
        "frontend" => Some("You are the frontend agent. Produce UI components, views, and client-side logic. Focus on UX, accessibility, responsive layout, and integration with the design system. List impacted components. Do not modify database schema unless explicitly asked. Before editing any file, read it to understand existing conventions and patterns; mimic the existing code style. Verify that any library you use is already declared in package.json before importing it."),
        "backend" => Some("You are the backend agent. Produce server-side logic, APIs, and business rules. Focus on correctness, performance, and clear contracts. Do not change frontend or DB schema unless the task explicitly requires it. Before editing any file, read it to understand existing conventions, imports, and patterns; mimic the existing code style. Never assume a dependency is available — verify it in the project's dependency file. When debugging failures, address the root cause rather than the symptoms; add targeted logging to isolate the issue. If the same problem persists after three attempts, stop and ask the user."),
        "database" => Some("You are the database / data agent. Produce schemas, migrations, queries, and data pipelines. Focus on consistency, indexing, and data integrity. Output clear DDL or migration steps when applicable."),
        "integration" => Some("You are the integration agent. Wire components together: APIs, events, external services. Focus on contracts, error handling, and end-to-end flows. Produce a precise deliverable (config, glue code, or runbook)."),
        "qa" => Some("You are the quality control agent. You prevent false 'work done'. Verify coherence, requirement coverage, missing files, hidden TODOs, incomplete sections. Do not rewrite; report defects and gaps by severity. Do not validate if acceptance criteria are incomplete; output a clear report for rework."),
        "system" => Some("You are the system agent. You have full knowledge of the Akasha application: commands (akasha start, init, doctor), interfaces (TUI, Chat, Router, Memory, Doc, Calendar), slash commands, skills, tools, and configuration. You can resolve issues and answer any question about how Akasha works. Be precise and refer to real features only."),
        "image_generation" => Some("You are the image generation agent. Produce images from text prompts using the generate_image tool. Focus on clear, concrete prompts that yield the requested visual. One precise deliverable per request."),
        "studio_project_manager" => Some("You are the Code Studio project manager (chef de projet). Tu coordonnes chaque demande sur le dépôt ouvert (chemins `workspace:/…`).\n\
Contrainte de stack : la stack active du projet (injectée dans le message) est verrouillée par défaut. Sans demande explicite de l’utilisateur, n’autorise pas de migration d’écosystème/langage (ex. Python -> TypeScript) et refuse toute délégation qui réécrit `CODE_STUDIO_PLAN.md` pour changer la stack.\n\
Règles d’orchestration :\n\
- **Kanban / tickets** : après lecture de `CODE_STUDIO_PLAN.md`, `DESIGN.md` et du résumé projet, assure-toi que le tableau Kanban reflète le travail : utilise `TOOL: studio_list_tickets`, puis `TOOL: studio_create_ticket` / `TOOL: studio_update_ticket` avec un **JSON sur une ligne** (ex. `TOOL: studio_create_ticket {\"title\":\"…\",\"description\":\"…\",\"depends_on_ticket_ids\":[\"uuid\"]}`). Crée ou ajuste les tickets **avant** de déléguer l’implémentation aux sous-agents.\n\
- **Dossier `specs/`** : pour toute demande d’**évolution** (nouvelle fonctionnalité, changement de comportement, refonte ciblée, branche d’évolution active, ou demande explicitement traitée comme évolution), crée un fichier plan dédié `workspace:/specs/<YYYYMMDD>-<slug-court>.md` avant de lancer l’implémentation. Le plan doit contenir : objectif, périmètre, critères d’acceptation, liste d’étapes numérotées, **marquage des étapes parallélisables** (ex. « (parallèle avec 3) »), risques, et une section **Iterations** pour suivre les passes de correction.\n\
- **Délégation** : tu es le **seul** à appeler `TOOL: delegate_to_agent <agent> <message>` vers des sous-agents (`conversation`, `code`, `studio_frontend`, `studio_backend`, `studio_fullstack`, `studio_scaffold`, `studio_planner` pour lecture/plan seul, `qa`, etc.). Les sous-agents **ne** doivent **pas** rappeler `delegate_to_agent`. **Mode anti-conflit Code Studio** : exécute une délégation **séquentielle** (un seul `delegate_to_agent` à la fois), attends le résultat, relis les fichiers impactés, puis lance le suivant. Quand le runtime injecte une consigne « délégation obligatoire », tu délègue avant toute implémentation applicative.\n\
- **Boucle de correction** : après chaque vague de sous-agents, lis les résultats / erreurs de build (`run_command --cwd workspace:/` quand autorisé), mets à jour le plan dans `specs/…` et relance des sous-tâches ciblées. **Maximum 5** vagues de retours sous-agents pour la même demande racine ; si au-delà le besoin n’est pas satisfait, réponds à l’utilisateur avec ce qui a été fait, les blocages, et des suggestions concrètes.\n\
- **Phase review ticket** : si le ticket est en `review`, délègue à `studio_reviewer` (ou au `review_agent` du ticket s’il vaut `studio_reviewer`) pour audit et feedback seulement. En review, ne mandate pas un agent d’implémentation pour réécrire le code ; la sortie attendue est un verdict + commentaires + retour `in_progress` si corrections requises.\n\
- **Synthèse utilisateur** : une fois le besoin rempli (ou en échec contrôlé), termine par un résumé clair en langage accessible.\n\
- **Fichiers** : respecte les règles Code Studio existantes pour `CODE_STUDIO_PLAN.md` et `DESIGN.md` ; n’écrase pas le plan global sans nécessité.\n\
Langue : aligne-toi sur le dernier message utilisateur."),
        "studio_scaffold" => Some("You are the Code Studio scaffold agent. Create a minimal, runnable project skeleton (README, package.json or Cargo.toml, clear entrypoints). Prefer workspace:/ paths when no absolute path is given; mirror files to the studio disk root. When the user message contains a [Stack technique du projet] block at the top, follow it strictly for languages, frameworks, package manager, and tooling; otherwise align with the stack recorded for the project or keep the skeleton generic. Never switch to another language/ecosystem unless the user explicitly asks for that migration; do not rewrite the Stack section of `CODE_STUDIO_PLAN.md` to a different stack on your own. Do not add dead files; keep structure conventional. Maintain workspace:/CODE_STUDIO_PLAN.md per the injected Code Studio plan rules (read_file first; section-wise edits only—never replace the whole file for a routine change). FILE OUTPUT RULE (strict): when writing files, write only the file content itself; never insert chat prose/status/explanations/reflection inside files. If a previous generation polluted a file with prose, clean it and keep only valid file content. Before finishing: run an appropriate build or typecheck when possible; in your final reply summarize what you created and how to run it in plain language."),
        "studio_frontend" => Some("You are the Code Studio frontend agent. Build UI components, routing, and styles with accessibility in mind. Prefer workspace:/ paths. When a [Stack technique du projet] block is present in the user message, obey it for UI libraries, bundler, CSS approach, and TypeScript/JavaScript choice. Never switch to another language/ecosystem unless the user explicitly asks for that migration; do not rewrite the Stack section of `CODE_STUDIO_PLAN.md` to a different stack on your own. Verify dependencies exist in package.json before importing. Use read_file before editing. Maintain workspace:/CODE_STUDIO_PLAN.md per the injected Code Studio plan rules (section-wise updates; no full-file rewrite for small tasks). FILE OUTPUT RULE (strict): when writing files, write only the file content itself; never insert chat prose/status/explanations/reflection inside files. For code files, output syntactically valid code only (except valid language comments). Run build/lint/typecheck via run_command --cwd workspace:/ when policy allows, and fix issues you introduced. End with a clear user-facing summary of changes and how to preview or test — not only \"Done\"."),
        "studio_backend" => Some("You are the Code Studio backend agent. Add APIs, env-based config, and CORS as needed. Prefer workspace:/ paths. When a [Stack technique du projet] block is present, follow it for runtime (Node, Python, Rust, etc.), framework, and persistence choices. Never switch to another language/ecosystem unless the user explicitly asks for that migration; do not rewrite the Stack section of `CODE_STUDIO_PLAN.md` to a different stack on your own. Never assume dependencies exist without checking the manifest. Use git_* tools on the project root when inspecting history. Maintain workspace:/CODE_STUDIO_PLAN.md per the injected Code Studio plan rules (section-wise updates; no full-file rewrite for small tasks). FILE OUTPUT RULE (strict): when writing files, write only the file content itself; never insert chat prose/status/explanations/reflection inside files. For code files, output syntactically valid code only (except valid language comments). Before declaring completion: run tests or at least start/build checks when feasible; summarize APIs and behavior for the user in accessible terms."),
        "studio_fullstack" => Some("You are the Code Studio full-stack agent. Coordinate frontend and backend changes in one pass: clear API contracts, shared types when applicable, and a coherent folder layout. Prefer workspace:/ paths; use run_in_container when policy allows for installs and builds. When a [Stack technique du projet] block is present in the user message, treat it as binding for the whole stack unless the user explicitly contradicts it in the same message. Never switch to another language/ecosystem unless the user explicitly asks for that migration; do not rewrite the Stack section of `CODE_STUDIO_PLAN.md` to a different stack on your own. Maintain workspace:/CODE_STUDIO_PLAN.md per the injected Code Studio plan rules (section-wise updates; no full-file rewrite for small tasks). FILE OUTPUT RULE (strict): when writing files, write only the file content itself; never insert chat prose/status/explanations/reflection inside files. If prose was accidentally inserted in a source file, remove it and keep only valid syntax for that file type. Verify end-to-end coherence; run combined build/test when policy allows. Close with a plain-language recap of what changed and how to run the app."),
        "studio_planner" => Some("You are the Code Studio planning agent. READ-ONLY on application source: do NOT write_file, write_code, edit_file, delete_file, rename_path, move_tree, search_replace, or apply_patch to any path except workspace:/CODE_STUDIO_PLAN.md. Do NOT run_command except read-only diagnostics (git status, git log, git diff, ls, cat, npm/yarn/pnpm only if the user explicitly asked for a read-only check). You MAY update workspace:/CODE_STUDIO_PLAN.md by sections to capture the plan. Explore with read_file, list_dir, grep_content. Deliver a clear implementation plan, critical files, and risks; end with next steps for a human or for an implement agent."),
        "studio_reviewer" => Some("You are the Code Studio QA/Review agent. STRICTLY NON-MUTATING: never modify application files and never run write tools (write_file, write_code, edit_file, search_replace, apply_patch, delete_file, rename_path, move_tree). Your job in ticket `review` is to verify that the work linked to the ticket is present and behaves as expected, then post structured feedback on the ticket. Use read-only inspection (read_file, grep_content, git_* and optional diagnostics) and finish by updating the ticket via `studio_update_ticket`: if accepted set review_outcome=approved and status=done; if issues remain set review_outcome=changes_requested, add concrete corrective_steps, and set status=in_progress. Do not rewrite the implementation during review."),
        _ => None,
    }
}

/// If AKASHA_TOOLS_JOURNAL_PATH is set, append a line for write tool invocations (Phase 4 modification journal).
pub(crate) async fn log_tool_journal_if_write(tool: &str, args: &[String], result_preview: &str) {
    const WRITE_TOOLS: &[&str] = &[
        "write_file",
        "write_code",
        "delete_file",
        "rename_path",
        "move_tree",
        "search_replace",
        "edit_file",
        "apply_patch",
    ];
    if !WRITE_TOOLS.contains(&tool) {
        return;
    }
    if let Ok(path) = std::env::var("AKASHA_TOOLS_JOURNAL_PATH") {
        let line = format!(
            "{} {} {} {}\n",
            chrono::Utc::now().to_rfc3339(),
            tool,
            args.join(" ").replace('\n', " "),
            result_preview
                .replace('\n', " ")
                .chars()
                .take(200)
                .collect::<String>()
        );
        if let Ok(mut f) = tokio::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&path)
            .await
        {
            let _ = tokio::io::AsyncWriteExt::write_all(&mut f, line.as_bytes()).await;
        }
    }
}

pub(crate) use crate::api_tool_parser::parse_tool_calls;

pub(crate) use crate::api_tool_dispatch::process_ops::{parse_run_command_args, resolve_run_command_working_dir};

pub(crate) use crate::api_tool_dispatch::web_device_memory_ops::parse_device_invoke_params;

pub(crate) use crate::api_tool_dispatch::fs_ops::{rewrite_workspace_plan_key_to_lineage_root, rewrite_workspace_plan_path_str, sync_workspace_store_from_disk, workspace_lineage_root_task_id};

pub(crate) fn parse_plugin_tool_invocation(
    plugin_registry: Option<&std::sync::Arc<crate::plugins::PluginRegistry>>,
    tool_name: &str,
    args: &[String],
) -> Option<(String, String)> {
    let registry = plugin_registry?;
    let available_ids: std::collections::HashSet<String> =
        registry.list().into_iter().map(|p| p.id).collect();

    if available_ids.is_empty() {
        return None;
    }

    let (plugin_id, action, forwarded_args): (String, Option<String>, Vec<String>) = if tool_name
        .eq_ignore_ascii_case("plugin.call")
        || tool_name.eq_ignore_ascii_case("plugin_call")
    {
        let plugin_id = args.first()?.trim().to_string();
        let forwarded = args.get(1..).map(|v| v.to_vec()).unwrap_or_default();
        (plugin_id, None, forwarded)
    } else if let Some(id) = tool_name.strip_prefix("plugin.") {
        (id.trim().to_string(), None, args.to_vec())
    } else if available_ids.contains("homeassistant")
        && (tool_name.starts_with("ha_") || tool_name.eq_ignore_ascii_case("homeassistant"))
    {
        let action = if tool_name.starts_with("ha_") {
            Some(tool_name.strip_prefix("ha_").unwrap_or("").to_string())
        } else {
            None
        };
        ("homeassistant".to_string(), action, args.to_vec())
    } else if available_ids.contains(tool_name) {
        (tool_name.to_string(), None, args.to_vec())
    } else if let Some((prefix, suffix)) = tool_name.split_once('_') {
        if available_ids.contains(prefix) {
            (
                prefix.to_string(),
                Some(suffix.trim().to_string()),
                args.to_vec(),
            )
        } else {
            return None;
        }
    } else {
        return None;
    };

    if plugin_id.is_empty() || !available_ids.contains(&plugin_id) {
        return None;
    }

    let payload = serde_json::json!({
        "tool": tool_name,
        "plugin_id": plugin_id,
        "action": action,
        "args": forwarded_args,
    });
    Some((
        payload
            .get("plugin_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        payload.to_string(),
    ))
}

pub(crate) async fn execute_tool_call(
    executor: &std::sync::Arc<akasha_tools::ToolExecutor>,
    tool_name: &str,
    args: &[String],
    process_registry: Option<&ProcessRegistry>,
    long_term_client: Option<&LongTermMemoryClient>,
    task_id: Uuid,
    store_path: Option<&std::path::Path>,
    conv_tx: Option<mpsc::Sender<OrchestratorTask>>,
    message_webhook_url: Option<&str>,
    plugin_registry: Option<&std::sync::Arc<crate::plugins::PluginRegistry>>,
    device_bridge: Option<&std::sync::Arc<crate::device_bridge::DeviceBridge>>,
    workspace_store: Option<&TaskWorkspaceStore>,
    browser_registry: Option<&crate::browser::BrowserSessionRegistry>,
    workspace_root: Option<&std::path::Path>,
    session_id: Option<&str>,
) -> (bool, String, Option<String>) {
    crate::api_tool_dispatch::execute_tool_call(
        crate::api_tool_dispatch::ToolCallContext {
            process_registry,
            long_term_client,
            task_id,
            store_path,
            conv_tx,
            message_webhook_url,
            plugin_registry,
            device_bridge,
            workspace_store,
            browser_registry,
            workspace_root,
            session_id,
        },
        executor,
        tool_name,
        args,
    )
    .await
}


/// Compact short-term memory when it would exceed context: summarize oldest turns via LLM and replace in store.
/// Optionally promote the summary to long-term memory (embed + store).
/// KinBot-style continuous session: never force a new session when compaction ceiling is hit.
fn session_continuous_mode() -> bool {
    std::env::var("AKASHA_SESSION_CONTINUOUS")
        .ok()
        .as_deref()
        .map(|s| matches!(s, "1" | "true" | "yes" | "on" | "TRUE" | "YES" | "ON"))
        .unwrap_or(false)
}

/// Refuses compaction beyond MAX_COMPACTIONS_PER_SESSION per session to avoid costly loops.
pub(crate) async fn compact_short_term_if_needed(
    short_term: &Arc<ShortTermStore>,
    session_id: &str,
    llm_router: &akasha_llm::LLMRouter,
    new_message_tokens: usize,
    long_term_client: Option<&LongTermMemoryClient>,
    tokenizer_provider: &str,
    tokenizer_model: &str,
    tasks_db_path: &std::path::Path,
) {
    if short_term.get_compaction_count(session_id).await
        >= crate::memory::MAX_COMPACTIONS_PER_SESSION
    {
        if session_continuous_mode() {
            short_term.reset_compaction_count(session_id).await;
            tracing::debug!(
                session_id,
                "Continuous session: compaction counter reset (AKASHA_SESSION_CONTINUOUS)"
            );
        } else {
            tracing::info!(session_id, "Short-term compaction skipped: max compactions per session reached (start a new session if context is too long)");
            return;
        }
    }
    const MAX_CONTEXT_DEFAULT: usize = 8192;
    let max_context_tokens = std::env::var("AKASHA_MAX_CONTEXT_TOKENS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(MAX_CONTEXT_DEFAULT);
    let trigger = (max_context_tokens as f64 * short_term.compaction_trigger_ratio) as usize;

    let turns = short_term.get_turns(session_id).await;
    let history_tokens =
        ShortTermStore::turns_tokens_calibrated(tokenizer_provider, tokenizer_model, &turns);
    if history_tokens + new_message_tokens <= trigger || turns.len() <= 2 {
        return;
    }

    let to_summarize = turns.len() / 2;
    let old_turns: Vec<_> = turns.into_iter().take(to_summarize).collect();
    let archive_batch: Vec<(usize, String, String)> = old_turns
        .iter()
        .enumerate()
        .map(|(i, t)| (i, t.role.clone(), t.content.clone()))
        .collect();
    if !archive_batch.is_empty() {
        let sp = tasks_db_path.to_path_buf();
        let sid = session_id.to_string();
        let batch = archive_batch.clone();
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(store) = akasha_store::ConversationArchiveStore::open(&sp) {
                let _ = store.archive_turns(&sid, &batch);
            }
        })
        .await;
    }
    let blob = ShortTermStore::turns_to_context(&old_turns);
    let summary_prompt = format!(
        "Summarize in a short paragraph in English, keeping important facts and decisions:\n\n{}",
        blob
    );
    let summary_max_tokens = std::env::var("AKASHA_SYSTEM_TASK_MAX_TOKENS")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(2048);
    let req = CompletionRequest {
        prompt: summary_prompt,
        max_tokens: Some(summary_max_tokens),
        temperature: Some(0.2),
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
    let compaction_timeout = env_duration_ms("AKASHA_COMPACTION_TIMEOUT_MS", 1_500);
    match tokio::time::timeout(compaction_timeout, llm_router.complete(&req)).await {
        Err(_) => {
            tracing::debug!(
                session_id,
                timeout_ms = compaction_timeout.as_millis() as u64,
                "Short-term compaction skipped: budget exceeded"
            );
        }
        Ok(Err(e)) => tracing::warn!(error = %e, "Compaction LLM failed, keeping full history"),
        Ok(Ok(resp)) => {
            let summary = resp.text.trim();
            if !summary.is_empty() {
                short_term
                    .replace_oldest_with_summary(session_id, summary.to_string(), to_summarize)
                    .await;
                short_term.increment_compaction_count(session_id).await;
                tracing::debug!(session_id, to_summarize, "Short-term memory compacted");
                crate::memory_hierarchical::maybe_run_hierarchical_compaction(
                    short_term,
                    llm_router,
                    long_term_client,
                    session_id,
                    max_context_tokens,
                )
                .await;
                // Promote summary to long-term memory (spec 06)
                if let Some(client) = long_term_client {
                    let summary = summary.to_string();
                    let session_id_attr = session_id.to_string();
                    let client = client.clone();
                    tokio::task::spawn_blocking(move || {
                        if let Err(e) = client.promote(summary.clone(), "compaction".to_string(), None, None, Some(session_id_attr.clone()), Some(1), Some("session".to_string()), None, None) {
                            tracing::warn!(error = %e, "Long-term promote after compaction failed");
                        } else if let Err(e) = client.emit_event("memory_promoted".to_string(), summary, None, None, Some(session_id_attr), None, Some(1), Some("session".to_string()), Some("compaction".to_string())) {
                            tracing::debug!(error = %e, "Episodic emit after compaction promote failed");
                        }
                    })
                    .await
                    .ok();
                }
            }
        }
    }
}

/// At daemon startup: if yesterday's short-term file exists, summarize it via LLM and promote to long-term (source "daily_summary").
/// Skips if a daily summary for that date already exists in long-term memory.
pub async fn summarize_yesterday_and_promote(
    short_term_dir: PathBuf,
    llm_router: Arc<akasha_llm::LLMRouter>,
    long_term_client: Option<LongTermMemoryClient>,
) {
    let Some(client) = long_term_client else {
        return;
    };
    let yesterday = chrono::Utc::now() - chrono::Duration::days(1);
    let yesterday_str = yesterday.format("%Y-%m-%d").to_string();
    let session_id = format!("day-{}", yesterday_str);
    let turns = match ShortTermStore::read_day_from_disk(&session_id, &short_term_dir) {
        Some(t) if !t.is_empty() => t,
        _ => return,
    };
    let has_summary = {
        let client = client.clone();
        let date = yesterday_str.clone();
        tokio::task::spawn_blocking(move || client.has_daily_summary_for_date(date))
            .await
            .unwrap_or(false)
    };
    if has_summary {
        tracing::debug!(session_id = %session_id, "Daily summary for yesterday already in long-term memory, skipping");
        return;
    }
    let blob = ShortTermStore::turns_to_context(&turns);
    let blob_capped = crate::llm_prompt_cap::truncate_utf8_bytes(
        &blob,
        crate::llm_prompt_cap::SYSTEM_PROMPT_FIELD_MAX_BYTES,
    );
    let summary_prompt = format!(
        "Summarize in a short synthetic paragraph (5 to 10 lines) the day of {}: topics covered, decisions, projects or important information. \
Factual response in English.\n\n{}",
        session_id.trim_start_matches("day-"),
        blob_capped
    );
    let summary_max_tokens = std::env::var("AKASHA_SYSTEM_TASK_MAX_TOKENS")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(1024);
    let req = CompletionRequest {
        prompt: summary_prompt,
        max_tokens: Some(summary_max_tokens),
        temperature: Some(0.2),
        preferred_task_type: Some("utility".to_string()),
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
            let summary = resp.text.trim();
            if !summary.is_empty() {
                let content = format!(
                    "Summary for {}: {}",
                    session_id.trim_start_matches("day-"),
                    summary
                );
                let client = client.clone();
                match tokio::task::spawn_blocking(move || {
                    let res = client.promote(
                        content.clone(),
                        "daily_summary".to_string(),
                        None,
                        None,
                        None,
                        Some(1),
                        None,
                        None,
                        None,
                    );
                    if res.is_ok() {
                        let _ = client.emit_event(
                            "memory_promoted".to_string(),
                            content,
                            None,
                            None,
                            None,
                            None,
                            Some(1),
                            None,
                            Some("daily_summary".to_string()),
                        );
                    }
                    res
                })
                .await
                {
                    Ok(Ok(_)) => {
                        tracing::info!(session_id = %session_id, "Yesterday summarized and stored in long-term memory")
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(error = %e, "Daily summary promote to long-term failed")
                    }
                    Err(e) => tracing::warn!(error = %e, "Daily summary task join failed"),
                }
            }
        }
        Err(e) => tracing::debug!(error = %e, "Daily summary LLM call failed"),
    }
}

/// Returns a short, user-friendly progress message for a tool invocation (for key steps).
pub(crate) fn progress_message_for_tool(tool: &str, args: &[String]) -> String {
    let first_arg = args.first().map(|s| s.as_str()).unwrap_or("");
    let second_arg = args.get(1).map(|s| s.as_str()).unwrap_or("");
    let lower = tool.to_lowercase();
    if lower.contains("web_search") || lower == "search" {
        let query_preview = first_arg.chars().take(40).collect::<String>();
        if query_preview.is_empty() {
            "Searching the web…".to_string()
        } else {
            format!("Searching the web for “{}”…", query_preview.trim())
        }
    } else if lower.contains("web_fetch") || lower == "fetch" {
        "Fetching the page…".to_string()
    } else if lower == "bankr" || first_arg.eq_ignore_ascii_case("bankr") {
        if second_arg.eq_ignore_ascii_case("portfolio") {
            "Checking your portfolio…".to_string()
        } else {
            "Running Bankr…".to_string()
        }
    } else if lower.contains("run_command") && first_arg.eq_ignore_ascii_case("bankr") {
        if second_arg.eq_ignore_ascii_case("portfolio") {
            "Checking your portfolio…".to_string()
        } else {
            "Running Bankr…".to_string()
        }
    } else if lower.contains("write_file")
        || lower.contains("search_replace")
        || lower.contains("edit_file")
    {
        "Writing the file…".to_string()
    } else if lower.contains("read_file") {
        "Reading the file…".to_string()
    } else if lower.contains("generate_image") {
        "Generating the image…".to_string()
    } else if lower.contains("speech_synthesize") {
        "Synthesizing speech…".to_string()
    } else if lower.contains("speech_transcribe") {
        "Transcribing audio…".to_string()
    } else if lower.contains("device_invoke") && first_arg.to_lowercase().contains("camera") {
        "Capturing with camera…".to_string()
    } else if lower == "browser" && first_arg.eq_ignore_ascii_case("navigate") {
        "Opening in browser…".to_string()
    } else if lower == "install_playwright" {
        "Installing Playwright Chromium…".to_string()
    } else if lower.contains("run_command") {
        "Running the command…".to_string()
    } else if lower == "ask_user" {
        "Waiting for your input…".to_string()
    } else {
        format!("Running {}…", tool)
    }
}

/// Extract a short name (how to call the user) from a message during onboarding.
/// Simple heuristic: first word, or first two words if the message is very short. Skips common prefixes.
pub(crate) fn extract_how_to_call_from_message(msg: &str) -> Option<String> {
    let msg = msg.trim();
    if msg.is_empty() || msg.len() > 80 {
        return None;
    }
    if classify_small_talk_message(msg).is_some() {
        return None;
    }
    let lower = msg.to_lowercase();
    let skip_prefixes = [
        "je m'appelle",
        "je suis",
        "c'est",
        "my name is",
        "i'm",
        "i am",
        "call me",
        "moi c'est",
    ];
    let mut text = msg;
    for prefix in skip_prefixes {
        if lower.starts_with(prefix) {
            text = msg[prefix.len()..].trim();
            if text.is_empty() {
                return None;
            }
            break;
        }
    }
    let words: Vec<&str> = text.split_whitespace().take(2).collect();
    let name = if words.len() == 1 {
        words[0].trim().to_string()
    } else if words.len() == 2 && text.len() <= 30 {
        format!("{} {}", words[0].trim(), words[1].trim())
    } else {
        words[0].trim().to_string()
    };
    let name = name
        .trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '\'')
        .to_string();
    if name.is_empty() || name.len() > 50 {
        None
    } else {
        Some(name)
    }
}

/// Builds a short, task-specific acknowledgment message so the user sees a real take-over instead of a generic placeholder.
/// Uses the start of the user message for context; one coherent sentence, same tone (formal "you").
pub(crate) fn build_ack_message(user_message: &str) -> String {
    let trimmed = user_message.trim();
    let preview = if trimmed.is_empty() {
        "your request".to_string()
    } else {
        let max_len = 50;
        let truncated: String = trimmed.chars().take(max_len).collect();
        if truncated.chars().count() >= max_len {
            format!("« {}… »", truncated.trim_end())
        } else {
            format!("« {} »", truncated)
        }
    };
    format!(
        "On it — looking into {}. You can follow progress in the Tasks tab.",
        preview
    )
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct MemoryProfile {
    pub(crate) recent_turns_limit: usize,
    pub(crate) recent_context_max_chars: usize,
    pub(crate) semantic_top_k: usize,
    pub(crate) episodic_limit: usize,
    pub(crate) facts_limit: usize,
    pub(crate) user_rag_top_k: usize,
    pub(crate) workspace_graph_top_k: usize,
    pub(crate) expand_by_graph: bool,
    pub(crate) graph_expand_hops: u8,
    pub(crate) compact_before_prompt: bool,
    pub(crate) allow_project_recall: bool,
    pub(crate) allow_identity_lookup: bool,
}

fn memory_fast_path_enabled() -> bool {
    std::env::var("AKASHA_MEMORY_FAST_PATH")
        .ok()
        .map(|s| s != "0" && !s.eq_ignore_ascii_case("false"))
        .unwrap_or(true)
}

pub(crate) fn memory_profile_for_task(
    message: &str,
    assigned_agent: &str,
    is_subagent: bool,
    orch_disk_deliverables: bool,
) -> MemoryProfile {
    fn env_usize(name: &str) -> Option<usize> {
        std::env::var(name).ok().and_then(|s| s.parse::<usize>().ok())
    }
    fn env_u8(name: &str) -> Option<u8> {
        std::env::var(name).ok().and_then(|s| s.parse::<u8>().ok())
    }
    let apply_env_overrides = |mut profile: MemoryProfile| -> MemoryProfile {
        if let Some(v) = env_usize("AKASHA_MEMORY_SEMANTIC_TOP_K") {
            profile.semantic_top_k = v.min(50);
        }
        if let Some(v) = env_u8("AKASHA_MEMORY_GRAPH_EXPAND_HOPS") {
            profile.graph_expand_hops = v.min(4);
        }
        if let Some(v) = env_usize("AKASHA_MEMORY_USER_RAG_TOP_K") {
            profile.user_rag_top_k = v.min(50);
        }
        if let Some(v) = env_usize("AKASHA_MEMORY_WORKSPACE_GRAPH_TOP_K") {
            profile.workspace_graph_top_k = v.min(50);
        }
        profile
    };
    if is_subagent {
        return apply_env_overrides(MemoryProfile {
            recent_turns_limit: 0,
            recent_context_max_chars: 0,
            semantic_top_k: 0,
            episodic_limit: 0,
            facts_limit: 0,
            user_rag_top_k: 0,
            workspace_graph_top_k: 0,
            expand_by_graph: false,
            graph_expand_hops: 0,
            compact_before_prompt: false,
            allow_project_recall: false,
            allow_identity_lookup: false,
        });
    }

    let enriched = !memory_fast_path_enabled()
        || orch_disk_deliverables
        || message_suggests_project(message)
        || assigned_agent != "conversation"
        || message.chars().count() > 280;

    if enriched {
        apply_env_overrides(MemoryProfile {
            recent_turns_limit: 15,
            recent_context_max_chars: 2_000,
            semantic_top_k: 5,
            episodic_limit: 5,
            facts_limit: 10,
            user_rag_top_k: 5,
            workspace_graph_top_k: 5,
            expand_by_graph: std::env::var("AKASHA_GRAPH_EXPAND").ok().as_deref() == Some("1"),
            graph_expand_hops: if std::env::var("AKASHA_GRAPH_EXPAND").ok().as_deref() == Some("1") {
                1
            } else {
                1
            },
            compact_before_prompt: true,
            allow_project_recall: true,
            allow_identity_lookup: true,
        })
    } else {
        apply_env_overrides(MemoryProfile {
            recent_turns_limit: 6,
            recent_context_max_chars: 800,
            semantic_top_k: 2,
            episodic_limit: 1,
            facts_limit: 0,
            user_rag_top_k: 0,
            workspace_graph_top_k: 0,
            expand_by_graph: false,
            graph_expand_hops: 0,
            compact_before_prompt: false,
            allow_project_recall: false,
            allow_identity_lookup: true,
        })
    }
}

fn task_stall_timeout_secs() -> u64 {
    std::env::var("AKASHA_TASK_STALL_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&s| s >= 30)
        .unwrap_or(180)
}

pub(crate) fn memory_recall_timeout_secs() -> u64 {
    std::env::var("AKASHA_MEMORY_RECALL_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&s| s >= 5)
        .unwrap_or(45)
}

/// Fails the task when no meaningful progress occurred within the stall timeout (worker hang, LLM never responds).
pub(crate) fn spawn_task_stall_watchdog(
    bus: EventBus,
    store_path: std::path::PathBuf,
    correlation_id: Uuid,
    task_id: Uuid,
    meaningful_progress: std::sync::Arc<std::sync::atomic::AtomicBool>,
    task_completion_registry: Option<TaskCompletionRegistry>,
    steering_queue: Option<SteeringQueueStore>,
    mut cancel_rx: oneshot::Receiver<()>,
) {
    tokio::spawn(async move {
        let timeout_secs = task_stall_timeout_secs();
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(timeout_secs)) => {
                if meaningful_progress.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                let still_running = TaskStore::open(&store_path)
                    .ok()
                    .and_then(|s| s.get(task_id).ok().flatten())
                    .map(|t| t.status == TaskStatus::Running)
                    .unwrap_or(false);
                if !still_running {
                    return;
                }
                let reason = format!(
                    "La tâche n'a produit aucune progression utile après {} secondes. \
                     Le worker a peut‑être bloqué ou le fournisseur LLM ne répond pas. \
                     Réessayez ou redémarrez le daemon.",
                    timeout_secs
                );
                if let Ok(store) = TaskStore::open(&store_path) {
                    let _ = store.update_status(task_id, TaskStatus::Failed);
                }
                let _ = bus.send(
                    EventEnvelope::new(
                        EventType::ProgressUpdate,
                        Some(serde_json::json!({
                            "task_id": task_id.to_string(),
                            "progress_pct": 100,
                            "message": reason
                        })),
                    )
                    .with_correlation(correlation_id),
                );
                let _ = bus.send(
                    EventEnvelope::new(
                        EventType::TaskFailed,
                        Some(serde_json::json!({
                            "task_id": task_id.to_string(),
                            "status": "failed",
                            "reason": reason
                        })),
                    )
                    .with_correlation(correlation_id),
                );
                if let Some(ref sq) = steering_queue {
                    sq.unregister_active(task_id).await;
                }
                notify_task_completion(&task_completion_registry, task_id).await;
                tracing::warn!(
                    task_id = %task_id,
                    timeout_secs,
                    "task stall watchdog: marked failed (no meaningful progress)"
                );
            }
            _ = &mut cancel_rx => {}
        }
    });
}

pub(crate) fn spawn_progress_watchdog(
    bus: EventBus,
    store_path: std::path::PathBuf,
    correlation_id: Uuid,
    task_id: Uuid,
    mut cancel_rx: oneshot::Receiver<()>,
) {
    tokio::spawn(async move {
        let checkpoints = [
            (2_u64, 12_u8, "pipeline_warmup", "Initialisation du worker…"),
            (
                5_u64,
                18_u8,
                "pipeline_context",
                "Assemblage du contexte et routage des outils…",
            ),
            (
                10_u64,
                24_u8,
                "pipeline_llm_pending",
                "Appel au modèle LLM en cours (fallback possible)…",
            ),
        ];
        for (secs, pct, stage, message) in checkpoints {
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(secs)) => {
                    let _ = bus.send(
                        EventEnvelope::new(
                            EventType::ProgressUpdate,
                            Some(serde_json::json!({
                                "task_id": task_id.to_string(),
                                "progress_pct": pct,
                                "message": message,
                            })),
                        )
                        .with_correlation(correlation_id),
                    );
                    insert_task_tracking_event(
                        store_path.as_path(),
                        task_id,
                        "pipeline_checkpoint",
                        serde_json::json!({
                            "stage": stage,
                            "progress_pct": pct,
                            "message": message,
                        }),
                    );
                }
                _ = &mut cancel_rx => return,
            }
        }
    });
}

fn cancel_progress_watchdog(cancel_tx: &mut Option<oneshot::Sender<()>>) {
    if let Some(tx) = cancel_tx.take() {
        let _ = tx.send(());
    }
}

pub(crate) fn cancel_task_watchdogs(
    progress_cancel: &mut Option<oneshot::Sender<()>>,
    stall_cancel: &mut Option<oneshot::Sender<()>>,
) {
    cancel_progress_watchdog(progress_cancel);
    cancel_progress_watchdog(stall_cancel);
}

/// Reload `tools_policy.yaml` from disk into the in-memory executor (hot reload).
pub(crate) async fn reload_tools_executor_policy(
    executor: &std::sync::Arc<tokio::sync::RwLock<std::sync::Arc<akasha_tools::ToolExecutor>>>,
    policy_path: &std::path::Path,
    data_dir: &std::path::Path,
) -> anyhow::Result<()> {
    let mut reloaded = akasha_tools::ToolsPolicy::load_from_path(policy_path)?;
    if let Ok(v) = akasha_vault::open_vault(data_dir) {
        reloaded.apply_vault_api_keys(|k| v.get(k).ok());
    }
    reloaded.workspace_root = Some(data_dir.to_path_buf());
    *executor.write().await = std::sync::Arc::new(akasha_tools::ToolExecutor::new(reloaded));
    Ok(())
}

async fn wait_for_task_activity(
    progress: &ProgressCache,
    task_id: Uuid,
    timeout: std::time::Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        {
            let guard = progress.read().await;
            let has_activity = guard
                .get(&task_id)
                .and_then(|q| q.back())
                .map(|entry| !entry.message.trim().is_empty())
                .unwrap_or(false);
            if has_activity {
                return true;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

/// Returns true if the text looks like a placeholder / promise ("I'll do it", "one second") rather than an actual answer.
/// Used after tool calls to avoid completing the task with "I will fetch…" instead of the real result.
pub(crate) fn looks_like_placeholder_after_tools(text: &str) -> bool {
    let lower = text.trim().to_lowercase();
    if lower.is_empty() {
        return false;
    }
    let placeholder_phrases = [
        "une seconde",
        "one second",
        "one moment",
        "un instant",
        "je vais ",
        "i'll ",
        "i will ",
        "i'm fetching",
        "i'm retrieving",
        "i'm getting",
        "i'm checking",
        "let me fetch",
        "let me get",
        "let me check",
        "let me find",
        "je vais récupérer",
        "je vais chercher",
        "je vais consulter",
        "génération",
        "récupération",
        "attendez",
        "wait ",
        "action en cours",
        "en cours",
        "puis te les résumer",
        "puis vous les",
        "then i'll ",
        "then i will ",
        "and then give you",
        "and then summarize",
        "will summarize",
        "will give you",
    ];
    placeholder_phrases.iter().any(|p| lower.contains(p))
}

/// Returns true when the model asks the user to manually apply file edits
/// ("replace this file with...") instead of using write tools.
pub(crate) fn looks_like_manual_file_patch_reply(text: &str) -> bool {
    let lower = text.trim().to_lowercase();
    if lower.is_empty() {
        return false;
    }
    let cues = [
        "voici le contenu corrigé",
        "à remplacer dans",
        "copie ces blocs",
        "replace directly in",
        "here is the corrected content",
        "paste this into",
        "replace this file with",
        "manually apply",
        "je fournis le contenu corrigé",
        "je fournis les corrections",
    ];
    cues.iter().any(|c| lower.contains(c))
}

pub(crate) fn looks_like_meta_agent_response(text: &str) -> bool {
    let lower = text.trim().to_lowercase();
    if lower.is_empty() {
        return false;
    }
    [
        "i am ready to assist",
        "i'm ready to assist",
        "based on the instructions provided",
        "according to the instructions provided",
        "following the instructions provided",
        "i am configured as",
        "i'm configured as",
        "as an ai assistant",
        "as an ai language model",
        "today, i will execute the phase 2 task",
        "phase 2 task",
        "click, fill, screenshot, and wait",
    ]
    .iter()
    .any(|p| lower.contains(p))
}

/// Returns true when the model output looks like generic greeting/small-talk
/// instead of an answer to a concrete tool-backed request.
pub(crate) fn looks_like_off_topic_greeting(text: &str) -> bool {
    let lower = text.trim().to_lowercase();
    if lower.is_empty() {
        return false;
    }
    [
        "bonjour ! je suis prêt à vous aider",
        "bonjour ! je suis pret a vous aider",
        "que souhaitez-vous faire aujourd'hui",
        "que souhaitez vous faire aujourd'hui",
        "hi! i'm ready to help",
        "what would you like to do today",
    ]
    .iter()
    .any(|p| lower.contains(p))
}

/// Outils autorisés pendant la correction automatique post-échec `npm run build` / `cargo check` (Code Studio).
pub(crate) const STUDIO_VERIFY_AUTOFIX_TOOLS: &[&str] = &[
    "read_file",
    "grep_content",
    "search_files",
    "search_replace",
    "edit_file",
    "write_file",
    "write_code",
    "apply_patch",
    "file_diff",
    "delete_file",
    "rename_path",
    "move_tree",
];

/// Injecté quand deux tours consécutifs produisent le même résumé d’outils (risque de boucle).
pub(crate) const STUDIO_VERIFY_AUTOFIX_STALL_WARNING: &str = "\n\n[STALL / ANTI-LOOP — READ CAREFULLY]\n\
The previous round’s tool results fingerprint matches this round’s: you are likely repeating a failing strategy.\n\
Rules for this sandbox:\n\
- There is NO `run_command`, NO shell, NO `mv`/`mkdir`/`git mv` here.\n\
- To rename/move a file or a single directory atomically: `rename_path <from> <to>` (destination must not exist).\n\
- To move a directory tree across volumes or when rename fails: `move_tree <from_dir> <to_dir>`.\n\
- If those are not applicable: `read_file` each source under the old path, then `write_file workspace:/new/path/...` with the same contents (write_file creates parent directories). Then `grep_content` + `search_replace` across the repo to fix imports. Optionally `delete_file` obsolete duplicates only if policy allows and you are sure.\n\
- If TS2305 says a symbol is not exported: `read_file` the module that the import resolves to on disk (path shown in the error) before editing; add the export OR change the import to a file that actually exists (`search_files workspace:/. <basename> true`).\n\
- If TS2554 wrong arity: fix the call site or the callee — read both sides.\n\
- Do NOT repeat the same import-only edit without ensuring the target file exists at that path.\n";

fn studio_verify_extract_ts_error_paths(verify_log: &str, cap: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in verify_log.lines() {
        if !line.contains(": error TS") {
            continue;
        }
        let Some((left, _)) = line.split_once('(') else {
            continue;
        };
        let p = left.trim();
        if p.is_empty() {
            continue;
        }
        if !(p.ends_with(".ts") || p.ends_with(".tsx") || p.ends_with(".js") || p.ends_with(".jsx"))
        {
            continue;
        }
        if out.iter().any(|x| x == p) {
            continue;
        }
        out.push(p.to_string());
        if out.len() >= cap {
            break;
        }
    }
    out
}

fn studio_verify_short_hash(input: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    input.hash(&mut h);
    format!("{:08x}", (h.finish() & 0xffff_ffff) as u32)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StudioVerifyUserLanguage {
    French,
    English,
    /// Spanish, German, etc. — LLM must follow the user excerpt language.
    MatchUserExcerpt,
}

pub(crate) fn studio_verify_detect_user_language(clean_user_message: &str) -> StudioVerifyUserLanguage {
    let t: String = clean_user_message.chars().take(6_000).collect();
    if t.trim().is_empty() {
        return StudioVerifyUserLanguage::MatchUserExcerpt;
    }
    let mut fr = 0i32;
    let mut en = 0i32;
    for ch in t.chars() {
        if matches!(
            ch,
            'à' | 'â' | 'é' | 'è' | 'ê' | 'ë' | 'î' | 'ï' | 'ô' | 'ù' | 'û' | 'ü' | 'ç' | 'œ' | 'æ'
        ) {
            fr += 3;
        }
    }
    let lower = t.to_lowercase();
    let pad = format!(" {lower} ");
    const FR_MARKERS: &[&str] = &[
        " tâche ",
        " créer ",
        " merci ",
        " échec ",
        " fichier ",
        " utilisateur ",
        " réinitialisation ",
        " veuillez ",
        " c'est ",
        " être ",
        " dans le ",
        " pas de ",
        " pour ",
        " avec ",
        " depuis ",
        " régénérer ",
        " mettre à jour ",
        " code studio ",
        " consigne ",
    ];
    const EN_MARKERS: &[&str] = &[
        " the ",
        " and ",
        " create ",
        " task ",
        " failed ",
        " please ",
        " update ",
        " build ",
        " error ",
        " file ",
        " user ",
        " request ",
        " with ",
        " from ",
        " regenerate ",
        " design ",
        " studio ",
        " workspace ",
    ];
    for m in FR_MARKERS {
        if pad.contains(m) {
            fr += 2;
        }
    }
    for m in EN_MARKERS {
        if pad.contains(m) {
            en += 2;
        }
    }
    if fr > en + 2 {
        StudioVerifyUserLanguage::French
    } else if en > fr + 2 {
        StudioVerifyUserLanguage::English
    } else {
        StudioVerifyUserLanguage::MatchUserExcerpt
    }
}

pub(crate) fn studio_verify_failure_banner(lang: StudioVerifyUserLanguage) -> &'static str {
    match lang {
        StudioVerifyUserLanguage::French => {
            "Échec vérification automatique (build/check) — la tâche est marquée en échec."
        }
        StudioVerifyUserLanguage::English => {
            "Automatic build/check verification failed — the task was marked as failed."
        }
        StudioVerifyUserLanguage::MatchUserExcerpt => {
            "Build/check verification failed — the task was marked as failed."
        }
    }
}

pub(crate) fn studio_verify_analyzing_progress_line(lang: StudioVerifyUserLanguage) -> &'static str {
    match lang {
        StudioVerifyUserLanguage::French => {
            "Échec de la vérification build — rédaction d'une synthèse lisible…"
        }
        StudioVerifyUserLanguage::English => {
            "Build verification failed — drafting a readable summary…"
        }
        StudioVerifyUserLanguage::MatchUserExcerpt => {
            "Verification failed — drafting a readable summary…"
        }
    }
}

pub(crate) fn studio_verify_build_excerpt_label(lang: StudioVerifyUserLanguage) -> &'static str {
    match lang {
        StudioVerifyUserLanguage::French => "--- Sortie build (extrait) ---",
        StudioVerifyUserLanguage::English => "--- Build output (excerpt) ---",
        StudioVerifyUserLanguage::MatchUserExcerpt => "--- Build output (excerpt) ---",
    }
}

pub(crate) fn studio_verify_summary_heading_markdown(lang: StudioVerifyUserLanguage) -> &'static str {
    match lang {
        StudioVerifyUserLanguage::French => "## Synthèse\n\n",
        StudioVerifyUserLanguage::English => "## Summary\n\n",
        StudioVerifyUserLanguage::MatchUserExcerpt => "",
    }
}

/// Revue légère post-build : critères **manuel** vs résumé des changements (1 appel LLM, sans outils). `None` = rien à signaler.
pub(crate) async fn studio_semantic_acceptance_review(
    llm_router: &std::sync::Arc<akasha_llm::LLMRouter>,
    manual_lines: &[String],
    diff_summary: &str,
    user_excerpt: &str,
    assistant_excerpt: &str,
) -> Option<Vec<String>> {
    if manual_lines.is_empty() {
        return None;
    }
    if std::env::var("AKASHA_STUDIO_SEMANTIC_VERIFY").ok().as_deref() == Some("0") {
        return None;
    }
    let crit = manual_lines.join("\n- ");
    let system = "You are a strict checklist reviewer for a software agent task. \
You ONLY compare the manual acceptance criteria list to the evidence (file change summary + short user goal + short assistant reply). \
Reply with a single JSON object and no markdown fences, no other text. Schema: {\"satisfied\":[\"...\"],\"missing\":[\"...\"]}. \
Put in \"missing\" any manual criterion that is clearly not addressed or contradicted by the evidence. If uncertain, prefer putting a short note in \"missing\" rather than assuming success. \
Use the same language as the criteria when writing strings.";
    let user = format!(
        "Manual criteria (each must be satisfied unless impossible and stated):\n- {crit}\n\n\
Summarized file diffs / changes (truncated):\n{}\n\nUser goal (excerpt):\n{}\n\nAssistant reply (excerpt):\n{}\n\n\
Output JSON only.",
        diff_summary.chars().take(10_000).collect::<String>(),
        user_excerpt.chars().take(1_800).collect::<String>(),
        assistant_excerpt.chars().take(1_800).collect::<String>()
    );
    let req = CompletionRequest {
        prompt: user,
        max_tokens: Some(500),
        temperature: Some(0.05),
        top_p: None,
        top_k: None,
        frequency_penalty: None,
        presence_penalty: None,
        repeat_penalty: None,
        num_ctx: None,
        num_gpu: None,
        thinking_level: None,
        preferred_task_type: Some("conversation".to_string()),
        system_prompt: Some(system.to_string()),
        image_data_urls: None,
    };
    let raw = match tokio::time::timeout(
        std::time::Duration::from_secs(45),
        llm_router.complete(&req),
    )
    .await
    {
        Ok(Ok(r)) => r.text,
        _ => return None,
    };
    let trimmed = raw.trim();
    let trimmed = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed)
        .trim()
        .trim_end_matches('`')
        .trim();
    let v: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let missing = v
        .get("missing")
        .and_then(|m| m.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if missing.is_empty() {
        None
    } else {
        Some(missing)
    }
}

/// Synthèse courte (LLM, sans outils) après échec de `studio_verify_after_agent_task`, pour l’utilisateur final.
pub(crate) async fn studio_verify_explain_failure_to_user(
    llm_router: &std::sync::Arc<akasha_llm::LLMRouter>,
    user_message: &str,
    assistant_reply: &str,
    verify_log: &str,
    lang: StudioVerifyUserLanguage,
) -> Option<String> {
    const MAX_USER: usize = 900;
    const MAX_ASSIST: usize = 1_400;
    const MAX_LOG: usize = 14_000;
    let um: String = user_message.chars().take(MAX_USER).collect();
    let ar: String = assistant_reply.chars().take(MAX_ASSIST).collect();
    let log: String = verify_log.chars().take(MAX_LOG).collect();
    let system = match lang {
        StudioVerifyUserLanguage::French => {
            "Tu es un assistant Code Studio. Une tâche agent vient d'échouer à l'étape de vérification automatique (souvent `npm run build`, `tsc`, `vite build`, ou `cargo check`). \
Rédige une synthèse entièrement en français, claire pour quelqu'un qui n'est pas familier avec les logs npm. \
Ne pas inventer de chemins ou numéros de ligne absents du log. Si l'extrait est incomplet, le dire. \
Structure (titres ## ou **gras** courts autorisés) :\n\
1) **Contexte** — en 1–2 phrases : objectif apparent (consigne + extrait de la réponse agent).\n\
2) **Cause principale** — ce qui bloque le build en langage simple.\n\
3) **Fichiers / lignes** — ceux visibles dans le log, sinon \"non précisé dans l'extrait\".\n\
4) **Prochaines étapes** — 2 à 5 actions concrètes.\n\
Pas de copier-coller massif du log ; pas de blocs de code de plus de 6 lignes. Réponse max ~1600 caractères."
                .to_string()
        }
        StudioVerifyUserLanguage::English => {
            "You are a Code Studio assistant. An agent task just failed automatic verification (often `npm run build`, `tsc`, `vite build`, or `cargo check`). \
Write the entire summary in clear English for someone who is not used to npm logs. \
Do not invent file paths or line numbers that are not in the log. If the excerpt is incomplete, say so. \
Structure (short ## or **bold** headings allowed):\n\
1) **Context** — 1–2 sentences: apparent goal (user request + agent reply excerpt).\n\
2) **Root cause** — what broke the build in plain language.\n\
3) **Files / lines** — those visible in the log, otherwise \"not specified in the excerpt\".\n\
4) **Next steps** — 2–5 concrete actions.\n\
No huge copy-paste of the log; no code blocks longer than 6 lines. Max ~1600 characters."
                .to_string()
        }
        StudioVerifyUserLanguage::MatchUserExcerpt => {
            "You are a Code Studio assistant. An agent task just failed automatic verification (often `npm run build`, `tsc`, `vite build`, or `cargo check`). \
**Language (mandatory):** Write your entire summary in the **same natural language** as the \"User message (excerpt)\" block below (French if French, English if English, Spanish if Spanish, etc.). Use headings and bullets in that same language. \
If that excerpt is too short to infer the language, default to **French**. Do not mix two languages. \
Do not invent file paths or line numbers absent from the log; if the excerpt is incomplete, say so in that language. \
Use a short structure: context (1–2 sentences), root cause, files/lines from the log (or \"not in excerpt\"), next steps (2–5 bullets). No huge log paste; max ~1600 characters."
                .to_string()
        }
    };
    let user = match lang {
        StudioVerifyUserLanguage::French => format!(
            "Consigne utilisateur (extrait) :\n{um}\n\nTravail / réponse agent (extrait) :\n{ar}\n\nSortie vérification (build/check) :\n{log}\n\nRédige la synthèse demandée."
        ),
        StudioVerifyUserLanguage::English => format!(
            "User message (excerpt):\n{um}\n\nAgent work / reply (excerpt):\n{ar}\n\nVerification output (build/check):\n{log}\n\nWrite the requested summary."
        ),
        StudioVerifyUserLanguage::MatchUserExcerpt => format!(
            "User message (excerpt):\n{um}\n\nAgent work / reply (excerpt):\n{ar}\n\nVerification output (build/check):\n{log}\n\nWrite the summary in the same language as the user message excerpt."
        ),
    };
    let req = CompletionRequest {
        prompt: user,
        max_tokens: Some(800),
        temperature: Some(0.15),
        top_p: None,
        top_k: None,
        frequency_penalty: None,
        presence_penalty: None,
        repeat_penalty: None,
        num_ctx: None,
        num_gpu: None,
        thinking_level: None,
        preferred_task_type: Some("conversation".to_string()),
        system_prompt: Some(system.to_string()),
        image_data_urls: None,
    };
    match tokio::time::timeout(
        std::time::Duration::from_secs(55),
        llm_router.complete(&req),
    )
    .await
    {
        Ok(Ok(r)) => {
            let t = r.text.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.chars().take(2_400).collect())
            }
        }
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "studio verify explain: LLM complete failed");
            None
        }
        Err(_) => {
            tracing::warn!("studio verify explain: LLM timeout");
            None
        }
    }
}

/// Path argument for `read_file` (before optional line/offset window), normalized like `execute_tool_call`.
fn parse_read_file_tool_path(args: &[String]) -> Option<String> {
    let line_window = if args.len() >= 3 {
        let maybe_limit = args.last().and_then(|s| s.parse::<usize>().ok());
        let maybe_offset = args
            .get(args.len().saturating_sub(2))
            .and_then(|s| s.parse::<usize>().ok());
        match (maybe_offset, maybe_limit) {
            (Some(off), Some(lim)) if lim > 0 => Some((off, lim)),
            _ => None,
        }
    } else {
        None
    };
    let path_parts_len = if line_window.is_some() && args.len() >= 3 {
        args.len() - 2
    } else {
        args.len()
    };
    if path_parts_len == 0 {
        return None;
    }
    let path_input = args[..path_parts_len].join(" ");
    let path_str = normalize_tool_path_hint(path_input.trim());
    if path_str.is_empty() || path_str.contains("..") {
        return None;
    }
    Some(path_str)
}

/// Remove model reasoning wrappers that occasionally leak in plain text responses.
fn strip_reasoning_wrappers(input: &str) -> String {
    let mut out = input.to_string();
    // Fast path: nothing to strip.
    if !out.contains("<think>") && !out.contains("</think>") {
        return out;
    }
    loop {
        let Some(start) = out.find("<think>") else {
            break;
        };
        if let Some(end_rel) = out[start..].find("</think>") {
            let end = start + end_rel + "</think>".len();
            out.replace_range(start..end, "");
        } else {
            // Unclosed tag: remove tail from opening marker.
            out.truncate(start);
            break;
        }
    }
    out.replace("</think>", "").replace("<think>", "")
}

fn extract_ts2306_not_module_paths(verify_log: &str, cap: usize) -> Vec<String> {
    let mut out = Vec::new();
    for line in verify_log.lines() {
        if !line.contains("error TS2306") || !line.contains("is not a module") {
            continue;
        }
        let Some((_, after)) = line.split_once("File '") else {
            continue;
        };
        let Some((path, _)) = after.split_once("' is not a module") else {
            continue;
        };
        let p = path.trim().replace('\\', "/");
        if p.is_empty() || out.iter().any(|x| x == &p) {
            continue;
        }
        out.push(p);
        if out.len() >= cap {
            break;
        }
    }
    out
}

/// Après un log d’échec de compilation, enchaîne quelques tours LLM + exécution d’outils pour corriger les sources.
/// Retourne `true` si au moins un outil d’écriture a réussi au moins une fois.
pub(crate) async fn studio_verify_run_llm_autofix_rounds(
    bus: &EventBus,
    llm_router: &std::sync::Arc<akasha_llm::LLMRouter>,
    tools_executor: &std::sync::Arc<RwLock<std::sync::Arc<akasha_tools::ToolExecutor>>>,
    skill_registry: Option<&std::sync::Arc<crate::skills::SkillRegistry>>,
    plugin_registry: Option<&std::sync::Arc<crate::plugins::PluginRegistry>>,
    process_registry: Option<&ProcessRegistry>,
    conv_tx: Option<mpsc::Sender<OrchestratorTask>>,
    long_term_client: Option<&LongTermMemoryClient>,
    workspace_store: Option<&TaskWorkspaceStore>,
    browser_registry: Option<&crate::browser::BrowserSessionRegistry>,
    device_bridge: Option<&std::sync::Arc<crate::device_bridge::DeviceBridge>>,
    task_id: Uuid,
    store_path: &Path,
    data_dir: &Path,
    tool_disk_root: &Path,
    verify_log: &str,
    max_llm_rounds: u32,
) -> bool {
    const VERIFY_SNIP: usize = 16_000;
    const LLM_TIMEOUT_SECS: u64 = 180;
    let timeline_correlation = resolve_root_task_id(store_path, task_id).unwrap_or(task_id);
    let message_webhook_url = std::env::var("AKASHA_MESSAGE_WEBHOOK_URL").ok();
    let system = "You repair a Code Studio project after `npm run build` or `cargo check` failed. \
Workspace paths are relative to the project root shown in the user message. \
Emit only lines starting with TOOL: using these tools: read_file, grep_content, search_files, search_replace, edit_file, write_file, apply_patch, file_diff, delete_file, rename_path, move_tree. \
STRICT tool-call format: `TOOL: <tool_name> <arg1> <arg2> ...` (space-separated args only). \
For ALL file/dir path arguments, ALWAYS use `workspace:/...` paths (including `workspace:/.` for project root scans). \
Never use bare relative paths like `src/...` or `.`; use `workspace:/src/...` and `workspace:/.`. \
Do NOT output JSON function-call style (invalid: `TOOL: read_file({\"path\":\"src/App.tsx\"})`). \
Do NOT output pipe syntax (invalid: `TOOL: search_files|pattern|*|path|.`). \
Valid examples: `TOOL: read_file workspace:/src/App.tsx 320 20`, `TOOL: search_files workspace:/. package.json true`. \
Do NOT use ask_user, delegate_to_agent, run_command, browser_*, install_skill, or any tool not in that list. \
IMPORTANT — this autofix sandbox has NO shell: you cannot `mv`, `git mv`, `mkdir` via run_command (it is disabled). \
To rename/move a file or directory when the destination does not exist: `TOOL: rename_path workspace:/old/path workspace:/new/path` (second path may contain spaces). \
To move a whole directory tree (e.g. cross-volume): `TOOL: move_tree workspace:/old_dir workspace:/new_dir`. \
If that is not enough: read each file under the old path, then write_file to `workspace:/new/dir/...` (parent dirs are created automatically), then search_replace / grep_content to fix imports across the project. \
If the build says a module has no exported member: read_file that module on disk at the path the error shows; either export the symbol or change imports to a module that actually exists (use search_files to locate the real file). \
Do not paste markdown fences or assistant prose into source files — only valid source code. \
Fix every compiler error; prefer minimal search_replace / edit_file over rewriting whole files. \
If a [Code index — …] block appears in the user message, use it as orientation only — you must still apply edits with search_replace / edit_file / write_file / apply_patch. \
When FILES_WITH_ERRORS or KNOWN_TS_ERRORS already pin down locations, prefer a concrete fix (search_replace / edit_file) over broad exploration; avoid read_file loops on the same path without editing. \
Large projects may require many reads across different files — that is fine. \
Do not deliver the fix only as prose or a full-file markdown block for the user to paste: emit write tools (`search_replace`, `edit_file`, `write_file`, `apply_patch`) unless a tool error or policy block forces you to explain why you cannot apply it yourself, then give a manual fallback. \
If tool results repeat without progress, change strategy (see any [STALL / ANTI-LOOP] block in the user message).";
    let root_disp = tool_disk_root.display().to_string();
    let log_snip: String = verify_log.chars().take(VERIFY_SNIP).collect();
    let focus_files = studio_verify_extract_ts_error_paths(verify_log, 10);
    let error_lines = verify_log
        .lines()
        .filter(|l| l.contains(": error TS"))
        .take(18)
        .collect::<Vec<_>>()
        .join("\n");
    let known_ts_errors_hash = studio_verify_short_hash(&error_lines);
    let sticky_context = format!(
        "CAVEMAN CONTEXT (reuse every round):\n\
ROOT: {}\n\
FILES_WITH_ERRORS: {}\n\
KNOWN_TS_ERRORS:\n{}\n",
        root_disp,
        if focus_files.is_empty() {
            "(none parsed)".to_string()
        } else {
            focus_files.join(", ")
        },
        if error_lines.trim().is_empty() {
            "(none captured)"
        } else {
            error_lines.as_str()
        }
    );
    let mut first_user = format!(
        "{}\nProject root (all relative paths are under this directory):\n{}\n\nBuild output:\n{}\n\nFix the project.",
        sticky_context, root_disp, log_snip
    );
    if studio_code_rag_enabled() {
        if let Some(pid) = crate::studio::studio_project_id_from_disk_root(data_dir, tool_disk_root) {
            let query = format!(
                "{}\n{}\n{}",
                error_lines,
                focus_files.join(" "),
                log_snip.chars().take(3_000).collect::<String>()
            );
            let data_dir_owned = data_dir.to_path_buf();
            let root_owned = tool_disk_root.to_path_buf();
            let code_rag_block = tokio::task::spawn_blocking(move || {
                let store = crate::code_rag::CodeRagStore::new(&data_dir_owned);
                let chunks = store.retrieve(
                    &pid,
                    &root_owned,
                    &query,
                    crate::code_rag::RetrieveOptions {
                        top_k: 8,
                        max_chars: 6_000,
                    },
                );
                let chunks = chunks.ok()?;
                if chunks.is_empty() {
                    return None;
                }
                crate::code_rag::format_retrieved_chunks(&chunks, 6_000)
            })
            .await
            .ok()
            .flatten();
            if let Some(block) = code_rag_block {
                first_user = format!(
                    "[Code index — extraits locaux (RAG Code Studio) pour guider la correction. Ce n’est pas une lecture complète des fichiers : vous devez quand même appliquer search_replace / edit_file / write_file / apply_patch sur les chemins en erreur.]\n\n{block}\n\n{first_user}"
                );
            }
        }
    }
    let exec = tools_executor.read().await.clone();
    let mut any_write_success = false;
    let mut follow_up = String::new();
    let mut short_or_no_tool_retries = 0u32;
    let mut writes_ok_count = 0u32;
    /// Nudge only when the same file is read repeatedly without an intervening write (exploration stays allowed across many files).
    const SAME_FILE_READ_THRESHOLD: u32 = 3;
    let mut last_read_file_path: Option<String> = None;
    let mut consecutive_same_file_reads: u32 = 0;
    let mut last_tool = "(none)".to_string();
    let mut last_failed_files = if focus_files.is_empty() {
        "(none parsed)".to_string()
    } else {
        focus_files.join(", ")
    };
    let mut prev_round_tool_fp: Option<String> = None;
    let mut pending_stall: Option<String> = None;
    let mut read_only_round_streak: u32 = 0;

    // Deterministic first-aid: TS2306 "is not a module" often comes from an empty .ts file.
    // Patch those files immediately so subsequent LLM rounds can focus on remaining errors.
    // Gated on tools_policy write permission; uses non-blocking I/O.
    if policy_allows_primary_disk_write(&exec.policy) {
        let not_module_paths = extract_ts2306_not_module_paths(verify_log, 12);
        for p in &not_module_paths {
            let as_path = std::path::Path::new(p);
            if !as_path.starts_with(tool_disk_root) {
                continue;
            }
            if !as_path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("ts"))
            {
                continue;
            }
            let content = tokio::fs::read_to_string(as_path).await.unwrap_or_default();
            if content.trim().is_empty() {
                let new_content = "export {}\n";
                // Pollution check — consistent with the write_file path
                if crate::api_studio::studio_reject_polluted_code_content(as_path, new_content)
                    .is_some()
                {
                    continue;
                }
                if tokio::fs::write(as_path, new_content.as_bytes()).await.is_ok() {
                    any_write_success = true;
                    writes_ok_count = writes_ok_count.saturating_add(1);
                    let _ = bus.send(
                        EventEnvelope::new(
                            EventType::ProgressUpdate,
                            Some(serde_json::json!({
                                "task_id": task_id.to_string(),
                                "progress_pct": 57,
                                "message": format!("Correctif auto TS2306: `{}` était vide, ajout de `export {{}}`.", p)
                            })),
                        )
                        .with_correlation(timeline_correlation),
                    );
                }
            }
        }
    }

    for round in 0..max_llm_rounds {
        let state_snapshot = format!(
            "STATE SNAPSHOT:\n\
ROUND_INDEX: {}\n\
LAST_TOOL: {}\n\
WRITES_OK_COUNT: {}\n\
NO_PARSEABLE_TOOL_RETRIES: {}\n\
KNOWN_TS_ERRORS_HASH: {}\n\
LAST_FAILED_FILES: {}\n",
            round + 1,
            last_tool,
            writes_ok_count,
            short_or_no_tool_retries,
            known_ts_errors_hash,
            last_failed_files
        );
        let user_block = if round == 0 {
            format!("{}\n{}", first_user, state_snapshot)
        } else {
            format!(
                "{}{}\n{}\nRound {} — previous tool results:\n{}\n\nEmit more TOOL: lines to fix remaining errors, or a single line AUTOFIX_DONE if the project should compile.",
                pending_stall.take().unwrap_or_default(),
                sticky_context,
                state_snapshot,
                round + 1,
                follow_up
            )
        };
        let req = CompletionRequest {
            prompt: user_block,
            max_tokens: Some(8192),
            temperature: Some(0.15),
            top_p: None,
            top_k: None,
            frequency_penalty: None,
            presence_penalty: None,
            repeat_penalty: None,
            num_ctx: None,
            num_gpu: None,
            thinking_level: None,
            preferred_task_type: Some("code_generation".to_string()),
            system_prompt: Some(system.to_string()),
            image_data_urls: None,
        };
        let resp_text = match tokio::time::timeout(
            std::time::Duration::from_secs(LLM_TIMEOUT_SECS),
            llm_router.complete(&req),
        )
        .await
        {
            Ok(Ok(r)) => strip_reasoning_wrappers(&r.text),
            Ok(Err(e)) => {
                tracing::warn!(task_id = %task_id, error = %e, "studio verify autofix: LLM complete failed");
                break;
            }
            Err(_) => {
                tracing::warn!(task_id = %task_id, "studio verify autofix: LLM timeout");
                break;
            }
        };
        let trimmed = resp_text.trim();
        if trimmed.contains("AUTOFIX_DONE") && parse_tool_calls(trimmed).is_empty() {
            tracing::info!(task_id = %task_id, round, "studio verify autofix: model signalled AUTOFIX_DONE");
            break;
        }
        let calls = parse_tool_calls(trimmed);
        if calls.is_empty() {
            let text_len = trimmed.chars().count();
            if short_or_no_tool_retries < 2 && round + 1 < max_llm_rounds {
                short_or_no_tool_retries += 1;
                let _ = bus.send(
                    EventEnvelope::new(
                        EventType::ProgressUpdate,
                        Some(serde_json::json!({
                            "task_id": task_id.to_string(),
                            "progress_pct": 56,
                            "message": "Réponse modèle trop courte / sans TOOL, nouvelle tentative de correction auto…"
                        })),
                    )
                    .with_correlation(timeline_correlation),
                );
                follow_up = format!(
                    "Model returned no parseable TOOL lines (len={text_len}). \
Retry now. Return only TOOL: lines; if FILES_WITH_ERRORS is set, prefer one read_file on the primary error file then immediately search_replace or edit_file to fix it."
                );
                tracing::warn!(
                    task_id = %task_id,
                    round,
                    text_len,
                    retry = short_or_no_tool_retries,
                    "studio verify autofix: no parseable TOOL lines, retrying"
                );
                continue;
            }
            tracing::debug!(task_id = %task_id, round, text_len, "studio verify autofix: no parseable TOOL lines");
            break;
        }
        short_or_no_tool_retries = 0;
        let mut round_results: Vec<String> = Vec::new();
        let mut round_read_success = false;
        let mut round_write_like_success = false;
        let lanes = akasha_tools::schedule_tool_calls(&calls);
        for (_lane, lane_calls) in lanes {
            for (name, args) in &lane_calls {
                let actual_tool = match skill_registry {
                    Some(reg) => reg
                        .get(name)
                        .await
                        .map(|s| s.tool_ref)
                        .unwrap_or_else(|| name.clone()),
                    None => name.clone(),
                };
                let actual_tool = canonicalize_tool_name(&actual_tool);
                last_tool = actual_tool.clone();
                if !STUDIO_VERIFY_AUTOFIX_TOOLS
                    .iter()
                    .any(|t| t.eq_ignore_ascii_case(&actual_tool))
                {
                    round_results.push(format!(
                        "[{}] skipped in studio autofix (not an allowed repair tool)",
                        actual_tool
                    ));
                    continue;
                }
                if exec.policy.requires_approval(&actual_tool) {
                    round_results.push(format!(
                        "[{}] skipped in studio autofix (requires human approval)",
                        actual_tool
                    ));
                    continue;
                }
                let _ = bus.send(
                    EventEnvelope::new(
                        EventType::ProgressUpdate,
                        Some(serde_json::json!({
                            "task_id": task_id.to_string(),
                            "progress_pct": 58,
                            "message": format!("Correction auto compilation : {} …", actual_tool)
                        })),
                    )
                    .with_correlation(timeline_correlation),
                );
                let (success, res, _cap) = execute_tool_call(
                    &exec,
                    &actual_tool,
                    args,
                    process_registry,
                    long_term_client,
                    task_id,
                    Some(store_path),
                    conv_tx.clone(),
                    message_webhook_url.as_deref(),
                    plugin_registry,
                    device_bridge,
                    workspace_store,
                    browser_registry,
                    Some(tool_disk_root),
                    None,
                )
                .await;
                let write_like = matches!(
                    actual_tool.as_str(),
                    "write_file" | "write_code" | "search_replace" | "edit_file" | "apply_patch"
                        | "delete_file" | "rename_path" | "move_tree"
                );
                if success && write_like {
                    any_write_success = true;
                    round_write_like_success = true;
                    writes_ok_count = writes_ok_count.saturating_add(1);
                    last_read_file_path = None;
                    consecutive_same_file_reads = 0;
                } else if success && actual_tool.as_str() == "read_file" {
                    round_read_success = true;
                    if let Some(path_key) = parse_read_file_tool_path(args) {
                        if last_read_file_path.as_ref() == Some(&path_key) {
                            consecutive_same_file_reads =
                                consecutive_same_file_reads.saturating_add(1);
                        } else {
                            last_read_file_path = Some(path_key);
                            consecutive_same_file_reads = 1;
                        }
                    }
                }
                round_results.push(format!("[{}] success={} {}", actual_tool, success, res));
            }
        }
        follow_up = round_results.join("\n");
        if round_read_success && !round_write_like_success {
            read_only_round_streak = read_only_round_streak.saturating_add(1);
        } else {
            read_only_round_streak = 0;
        }
        if read_only_round_streak >= 2 {
            let _ = bus.send(
                EventEnvelope::new(
                    EventType::ProgressUpdate,
                    Some(serde_json::json!({
                        "task_id": task_id.to_string(),
                        "progress_pct": 59,
                        "message": format!(
                            "[Étape: garde-fou autofix] {} round(s) lecture-only détecté(s) — correction d'écriture forcée (search_replace/edit_file/write_code/write_file).",
                            read_only_round_streak
                        )
                    })),
                )
                .with_correlation(timeline_correlation),
            );
            let not_module_paths = extract_ts2306_not_module_paths(verify_log, 6);
            let focus = if not_module_paths.is_empty() {
                "No TS2306 path parsed from compiler output.".to_string()
            } else {
                format!("TS2306 focus files: {}", not_module_paths.join(", "))
            };
            follow_up.push_str(&format!(
                "\n\n[HARD GUARD — READ-ONLY ROUNDS]\n\
You just completed {read_only_round_streak} round(s) with successful reads but no successful write tool.\n\
Stop broad exploration. Apply one concrete fix NOW with search_replace/edit_file/write_file.\n\
If a file is empty and TS2306 says 'is not a module', add at least `export {{}}` first, then continue with proper exported types.\n\
{focus}\n"
            ));
        }
        if read_only_round_streak >= 4 {
            let _ = bus.send(
                EventEnvelope::new(
                    EventType::ProgressUpdate,
                    Some(serde_json::json!({
                        "task_id": task_id.to_string(),
                        "progress_pct": 60,
                        "message": "[Étape: arrêt anti-boucle] Trop de rounds lecture-only successifs en autofix — arrêt de la boucle pour éviter l'exploration infinie."
                    })),
                )
                .with_correlation(timeline_correlation),
            );
            tracing::warn!(
                task_id = %task_id,
                round = round + 1,
                "studio verify autofix: aborting due to repeated read-only rounds"
            );
            break;
        }
        if consecutive_same_file_reads >= SAME_FILE_READ_THRESHOLD {
            let path_disp = last_read_file_path
                .as_deref()
                .unwrap_or("(unknown path)");
            follow_up.push_str(&format!(
                "\n\n[LOOP GUARD — REPEATED read_file]\n\
You successfully read_file `{path_disp}` {SAME_FILE_READ_THRESHOLD} times in a row without an intervening successful write tool.\n\
The compiler output already signals what is wrong: apply a minimal fix with `TOOL: search_replace … | …` or `TOOL: edit_file …` on this file (or switch to another path from FILES_WITH_ERRORS / KNOWN_TS_ERRORS instead of re-opening the same file).\n"
            ));
            last_read_file_path = None;
            consecutive_same_file_reads = 0;
        }
        let files_after_round = studio_verify_extract_ts_error_paths(&follow_up, 10);
        if !files_after_round.is_empty() {
            last_failed_files = files_after_round.join(", ");
        }
        if follow_up.trim().is_empty() {
            break;
        }
        let fp = studio_verify_short_hash(&follow_up.chars().take(4_500).collect::<String>());
        if let Some(prev) = prev_round_tool_fp.as_ref() {
            if *prev == fp && round >= 1 {
                pending_stall = Some(STUDIO_VERIFY_AUTOFIX_STALL_WARNING.to_string());
                tracing::warn!(
                    task_id = %task_id,
                    round = round + 1,
                    "studio verify autofix: repeated tool-output fingerprint (stall guard)"
                );
            }
        }
        prev_round_tool_fp = Some(fp);
    }
    if !any_write_success {
        let focus_files = studio_verify_extract_ts_error_paths(verify_log, 8);
        if !focus_files.is_empty() {
            let _ = bus.send(
                EventEnvelope::new(
                    EventType::ProgressUpdate,
                    Some(serde_json::json!({
                        "task_id": task_id.to_string(),
                        "progress_pct": 57,
                        "message": "Fallback auto-correction : lecture ciblée des fichiers en erreur TS…"
                    })),
                )
                .with_correlation(timeline_correlation),
            );
            let mut focused_reads: Vec<String> = Vec::new();
            for p in &focus_files {
                let read_args = vec![p.clone()];
                let (ok, res, _cap) = execute_tool_call(
                    &exec,
                    "read_file",
                    &read_args,
                    process_registry,
                    long_term_client,
                    task_id,
                    Some(store_path),
                    conv_tx.clone(),
                    message_webhook_url.as_deref(),
                    plugin_registry,
                    device_bridge,
                    workspace_store,
                    browser_registry,
                    Some(tool_disk_root),
                    None,
                )
                .await;
                if ok {
                    focused_reads.push(format!(
                        "FILE: {}\n{}",
                        p,
                        res.chars().take(5000).collect::<String>()
                    ));
                }
            }
            if !focused_reads.is_empty() {
                let fallback_req = CompletionRequest {
                    prompt: format!(
                        "Build output:\n{}\n\nFocused file reads:\n{}\n\nReturn ONLY TOOL lines to fix the TS syntax/build errors now. \
Use only: search_replace, edit_file, write_file, apply_patch, file_diff, delete_file, rename_path, move_tree. \
No prose, no ask_user, no run_command. Prefer rename_path / move_tree for renames; otherwise use write_file under new paths (parents auto-created) and fix imports.",
                        verify_log.chars().take(12_000).collect::<String>(),
                        focused_reads.join("\n\n---\n\n")
                    ),
                    max_tokens: Some(8192),
                    temperature: Some(0.1),
                    top_p: None,
                    top_k: None,
                    frequency_penalty: None,
                    presence_penalty: None,
                    repeat_penalty: None,
                    num_ctx: None,
                    num_gpu: None,
                    thinking_level: None,
                    preferred_task_type: Some("code_generation".to_string()),
                    system_prompt: Some(
                        "You are in final deterministic Code Studio autofix fallback. Emit only TOOL lines. \
Use STRICT format: `TOOL: <tool_name> <arg1> <arg2> ...` with plain space-separated arguments. \
For ALL file/dir paths, ALWAYS use `workspace:/...` arguments (for root scans, use `workspace:/.`). \
Do not use bare relative paths (`src/...`, `.`) and do not use `tool(...)` JSON-call style or `|key|value|` syntax."
                            .to_string(),
                    ),
                    image_data_urls: None,
                };
                if let Ok(Ok(resp)) = tokio::time::timeout(
                    std::time::Duration::from_secs(LLM_TIMEOUT_SECS),
                    llm_router.complete(&fallback_req),
                )
                .await
                {
                    let calls = parse_tool_calls(resp.text.trim());
                    if !calls.is_empty() {
                        let lanes = akasha_tools::schedule_tool_calls(&calls);
                        for (_lane, lane_calls) in lanes {
                            for (name, args) in &lane_calls {
                                let actual_tool = canonicalize_tool_name(name);
                                if !matches!(
                                    actual_tool.as_str(),
                                    "search_replace" | "edit_file" | "write_file" | "write_code" | "apply_patch"
                                        | "file_diff" | "delete_file" | "rename_path" | "move_tree"
                                ) {
                                    continue;
                                }
                                let (success, _res, _cap) = execute_tool_call(
                                    &exec,
                                    &actual_tool,
                                    args,
                                    process_registry,
                                    long_term_client,
                                    task_id,
                                    Some(store_path),
                                    conv_tx.clone(),
                                    message_webhook_url.as_deref(),
                                    plugin_registry,
                                    device_bridge,
                                    workspace_store,
                                    browser_registry,
                                    Some(tool_disk_root),
                                    None,
                                )
                                .await;
                                if success
                                    && matches!(
                                        actual_tool.as_str(),
                                        "search_replace" | "edit_file" | "write_file" | "write_code" | "apply_patch"
                                            | "delete_file" | "rename_path" | "move_tree"
                                    )
                                {
                                    any_write_success = true;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    any_write_success
}

pub(crate) fn insert_task_tracking_event(
    store_path: &std::path::Path,
    task_id: Uuid,
    event_type: &str,
    payload: serde_json::Value,
) {
    if let Ok(store) = TaskStore::open(store_path) {
        let _ = store.insert_event(
            task_id,
            event_type,
            Some(&payload),
            &chrono::Utc::now().to_rfc3339(),
        );
    }
}

pub(crate) use crate::api_llm_loop::run_message_via_llm;

/// Notify any waiter in the TaskCompletionRegistry that `task_id` has finished.
pub(crate) async fn notify_task_completion(registry: &Option<TaskCompletionRegistry>, task_id: Uuid) {
    if let Some(reg) = registry {
        if let Some(notify) = reg.write().await.remove(&task_id) {
            notify.notify_one();
        }
    }
}

/// Learn step: emit a `task_outcome` episodic event after a task completes (conversation or orchestrator).
/// Payload: task_id, initial_message_preview (200 chars), status, summary_preview (300 chars), optional intent.
/// Synchronous; must not be called from a tokio runtime thread (use learn_from_task_outcome_async from async code).
pub(crate) fn learn_from_task_outcome(
    client: Option<&LongTermMemoryClient>,
    task_id: Uuid,
    initial_message: &str,
    status: &str,
    summary: &str,
    session_id: Option<&str>,
    intent: Option<&str>,
) {
    let Some(client) = client else { return };
    let initial_preview: String = initial_message.chars().take(200).collect();
    let summary_preview: String = summary.chars().take(300).collect();
    let payload = serde_json::json!({
        "task_id": task_id.to_string(),
        "initial_message_preview": initial_preview,
        "status": status,
        "summary_preview": summary_preview,
        "intent": intent,
    });
    if let Err(e) = client.emit_event(
        "task_outcome".to_string(),
        payload.to_string(),
        None,
        None,
        session_id.map(String::from),
        Some(task_id.to_string()),
        None,
        None,
        None,
    ) {
        tracing::debug!(task_id = %task_id, error = %e, "task_outcome emit failed");
    }
}

/// Async wrapper: runs learn_from_task_outcome in spawn_blocking so it is safe to call from async (avoids blocking the runtime).
pub(crate) async fn learn_from_task_outcome_async(
    client: Option<LongTermMemoryClient>,
    task_id: Uuid,
    initial_message: String,
    status: String,
    summary: String,
    session_id: Option<String>,
    intent: Option<String>,
) {
    let Some(client) = client else { return };
    let _ = tokio::task::spawn_blocking(move || {
        learn_from_task_outcome(
            Some(&client),
            task_id,
            &initial_message,
            &status,
            &summary,
            session_id.as_deref(),
            intent.as_deref(),
        );
    })
    .await;
}

/// Optional channel to trigger daemon shutdown (for POST /api/restart).
pub type RestartTx = Option<tokio::sync::mpsc::Sender<()>>;

/// Optional filters for GET /api/events (`?task_id=` and/or `?types=` comma-separated).
#[derive(Debug, Clone, Default)]
pub struct SseEventFilter {
    pub task_id: Option<uuid::Uuid>,
    pub event_types: Option<std::collections::HashSet<String>>,
}

/// Parse `task_id` and `types` / `event_types` from a query string (without leading `?`).
pub fn parse_sse_event_filter(query: &str) -> SseEventFilter {
    let mut filter = SseEventFilter::default();
    for pair in query.split('&') {
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        let key = k.trim().to_lowercase();
        let val = urlencoding::decode(v.trim()).unwrap_or_else(|_| v.trim().into());
        match key.as_str() {
            "task_id" | "correlation_id" => {
                if let Ok(id) = uuid::Uuid::parse_str(val.trim()) {
                    filter.task_id = Some(id);
                }
            }
            "types" | "event_types" => {
                let set: std::collections::HashSet<String> = val
                    .split(',')
                    .map(|s| s.trim().to_lowercase())
                    .filter(|s| !s.is_empty())
                    .collect();
                if !set.is_empty() {
                    filter.event_types = Some(set);
                }
            }
            _ => {}
        }
    }
    filter
}

fn sse_event_matches_filter(envelope: &EventEnvelope, filter: &SseEventFilter) -> bool {
    if let Some(tid) = filter.task_id {
        let corr = envelope.correlation_id == Some(tid);
        let payload_tid = envelope
            .payload
            .as_ref()
            .and_then(|p| p.get("task_id"))
            .and_then(|v| v.as_str())
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
            == Some(tid);
        if !corr && !payload_tid {
            return false;
        }
    }
    if let Some(ref types) = filter.event_types {
        if !types.contains(&envelope.event_type.as_str().to_lowercase()) {
            return false;
        }
    }
    true
}

/// Stream event-bus events as Server-Sent Events (GET /api/events). Keeps connection open until client disconnects.
pub async fn stream_sse_events<W>(
    bus: &EventBus,
    stream: &mut W,
    filter: SseEventFilter,
) -> std::io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut rx = crate::agents::subscribe(bus);
    let headers = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\nAccess-Control-Allow-Origin: *\r\n\r\n";
    stream.write_all(headers.as_bytes()).await?;
    stream.flush().await?;
    loop {
        match rx.recv().await {
            Ok(envelope) => {
                if !sse_event_matches_filter(&envelope, &filter) {
                    continue;
                }
                let payload = serde_json::json!({
                    "id": envelope.id.to_string(),
                    "event_type": envelope.event_type.as_str(),
                    "payload": envelope.payload,
                    "timestamp": envelope.timestamp.to_rfc3339(),
                    "correlation_id": envelope.correlation_id.map(|u| u.to_string()),
                });
                let line = format!("data: {}\n\n", payload.to_string());
                if stream.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if stream.flush().await.is_err() {
                    break;
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
    Ok(())
}


pub(crate) fn decode_url_component(s: &str) -> String {
    urlencoding::decode(s)
        .unwrap_or_else(|_| s.to_string().into())
        .into_owned()
}

pub async fn handle_api(
    method: &str,
    path: &str,
    body: Option<Vec<u8>>,
    headers: &std::collections::HashMap<String, String>,
    store_path: &Path,
    progress: &ProgressCache,
    events: &EventsCache,
    main_agent: &crate::agents::MainAgent,
    channel_config: &crate::channels::ChannelConfig,
    plugin_registry: &std::sync::Arc<crate::plugins::PluginRegistry>,
    llm_router: &std::sync::Arc<akasha_llm::LLMRouter>,
    rag_pack: &std::sync::Arc<akasha_rag::RagPack>,
    ollama_base_url: Option<&str>,
    spec_dir: &Path,
    restart_tx: RestartTx,
    tools_executor: Option<
        &std::sync::Arc<tokio::sync::RwLock<std::sync::Arc<akasha_tools::ToolExecutor>>>,
    >,
    skill_registry: &std::sync::Arc<crate::skills::SkillRegistry>,
    short_term: Option<std::sync::Arc<ShortTermStore>>,
    long_term_client: Option<LongTermMemoryClient>,
    human_input_store: Option<HumanInputStore>,
    steering_queue: Option<SteeringQueueStore>,
    user_rag_store: &crate::user_rag::SharedUserRagStore,
    notes_store: &crate::notes::SharedNotesStore,
    agent_profile_cache: &AgentProfileCache,
    update_cache: &UpdateCheckCache,
    task_usage_store: &TaskUsageStore,
    device_bridge: Option<&std::sync::Arc<crate::device_bridge::DeviceBridge>>,
    autonomous_mission: Option<Arc<RwLock<AutonomousMissionConfig>>>,
) -> String {
    let data_dir = store_path.parent().unwrap_or_else(|| store_path.as_ref());

    if let Some(resp) = crate::api_security::csrf_reject_response(method, path, headers) {
        return resp;
    }

    let (path_only, query_str) = crate::api_security::split_path_query(path);
    let _http_post_lifecycle = crate::lifecycle_hooks::HttpPostLifecycleHooks {
        data_dir: data_dir.to_path_buf(),
        method: method.to_string(),
        path: path_only.to_string(),
    };
    crate::lifecycle_hooks::run_http_request_pre_hooks(data_dir, method, path_only).await;

    if let Some(resp) = crate::api_studio::handle_studio_route(
        method,
        path_only,
        query_str,
        body.as_deref(),
        data_dir,
    )
    .await
    {
        return resp;
    }

    if let Some(resp) = crate::api_workspace_graph::handle_workspace_graph(
        method,
        path_only,
        body.as_deref(),
        data_dir,
        store_path,
    )
    .await
    {
        return resp;
    }

    if let Some(resp) = crate::api_routes_automation::handle_automation_routes(
        method,
        path_only,
        body.as_deref(),
        headers,
        data_dir,
    )
    .await
    {
        return resp;
    }

    if let Some(resp) = crate::api_routes_event_triggers::handle_event_trigger_routes(
        method,
        path_only,
        body.as_deref(),
        store_path,
    )
    .await
    {
        return resp;
    }

    if let Some(resp) = crate::api_routes_notes::try_handle(
        method,
        path_only,
        Some(query_str),
        body.as_deref(),
        notes_store,
    )
    .await
    {
        return resp;
    }

    let header_pairs: Vec<(String, String)> = headers
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if let Some(resp) = crate::api_routes_calendar::try_handle(
        method,
        path_only,
        Some(query_str),
        body.as_deref(),
        &header_pairs,
        &crate::api_routes_calendar::CalendarRouteCtx {
            store_path,
            data_dir,
        },
    )
    .await
    {
        return resp;
    }

    if let Some(resp) = crate::api_routes_kinbot::try_handle(
        method,
        path_only,
        Some(query_str),
        body.as_deref(),
        &crate::api_routes_kinbot::KinbotRouteCtx {
            store_path,
            data_dir,
            user_rag_store,
            plugin_registry: Some(plugin_registry),
        },
    )
    .await
    {
        return resp;
    }

    if let Some(resp) = crate::api_routes_life::try_handle(
        method,
        path_only,
        body.as_deref(),
        &crate::api_routes_life::LifeRouteCtx {
            store_path,
            data_dir,
        },
    )
    .await
    {
        return resp;
    }

    if let Some(resp) = crate::api_routes_terminal::handle_terminal_routes(
        method,
        path_only,
        query_str,
        body.as_deref(),
    )
    .await
    {
        return resp;
    }

    if let Some(resp) = crate::api_routes_mcp::handle_mcp_lifecycle_routes(
        method,
        path_only,
        body.as_deref(),
        data_dir,
    )
    .await
    {
        return resp;
    }

    if let Some(tools_exec) = tools_executor {
        let research_ctx = crate::deep_research::build_research_context(
            llm_router.clone(),
            tools_exec.clone(),
            data_dir,
        );
        if let Some(resp) = crate::deep_research::handle_deep_research_routes(
            method,
            path_only,
            body.as_deref(),
            &research_ctx,
        )
        .await
        {
            return resp;
        }
    }

    if let Some(resp) = crate::api_routes_workspace::handle_workspace_routes(
        method,
        path_only,
        Some(query_str),
        body.as_deref(),
        &llm_router,
    )
    .await
    {
        return resp;
    }

    if let Some(resp) = crate::api_routes_mission::handle_mission_routes(
        method,
        path_only,
        query_str,
        body.as_deref(),
        data_dir,
        store_path,
        autonomous_mission.clone(),
    )
    .await
    {
        return resp;
    }


    // GET /api/update/status — cached result of latest.json from Akasha_app (for UI update banner)


    // GET /api/device/pending — oldest pending device request (for UI to fulfill: camera, mic, etc.)

    // GET /api/voice/status — whether TTS/STT are configured (voice_router.yaml)

    if let Some(resp) = crate::api_routes_profiles::handle_profiles_routes(
        method,
        path_only,
        query_str,
        body.as_deref(),
        data_dir,
        agent_profile_cache,
        short_term.clone(),
        long_term_client.clone(),
    )
    .await
    {
        return resp;
    }

    if let Some(resp) = crate::api_routes_docs::try_handle(
        method,
        path_only,
        Some(query_str),
        body.as_deref(),
        &crate::api_routes_docs::RouteCtx {
            data_dir,
            spec_dir,
            update_cache,
        },
    )
    .await
    {
        return resp;
    }

    if let Some(resp) = crate::api_routes_companion::try_handle(
        method,
        path_only,
        Some(query_str),
        body.as_deref(),
        &crate::api_routes_companion::RouteCtx {
            data_dir,
            store_path,
            llm_router,
            headers,
        },
    )
    .await
    {
        return resp;
    }

    if let Some(resp) = crate::api_routes_config::try_handle(
        method,
        path_only,
        Some(query_str),
        body.as_deref(),
        &crate::api_routes_config::RouteCtx {
            data_dir,
            store_path,
            main_agent,
            short_term: short_term.clone(),
            tools_executor,
        },
    )
    .await
    {
        return resp;
    }

    if let Some(resp) = crate::api_routes_router::try_handle(
        method,
        path_only,
        Some(query_str),
        body.as_deref(),
        &crate::api_routes_router::RouteCtx {
            data_dir,
            store_path,
            llm_router,
            ollama_base_url,
            task_usage_store,
        },
    )
    .await
    {
        return resp;
    }

    if let Some(resp) = crate::api_routes_plugins::try_handle(
        method,
        path_only,
        Some(query_str),
        body.as_deref(),
        &crate::api_routes_plugins::RouteCtx {
            data_dir,
            spec_dir,
            plugin_registry,
            skill_registry,
            device_bridge,
            tools_executor,
        },
    )
    .await
    {
        return resp;
    }

    if let Some(resp) = crate::api_routes_memory::try_handle(
        method,
        path_only,
        Some(query_str),
        body.as_deref(),
        &crate::api_routes_memory::RouteCtx {
            data_dir,
            short_term: short_term.clone(),
            long_term_client: long_term_client.clone(),
        },
    )
    .await
    {
        return resp;
    }

    if let Some(resp) = crate::api_routes_tasks::try_handle(
        method,
        path_only,
        Some(query_str),
        body.as_deref(),
        &crate::api_routes_tasks::RouteCtx {
            store_path,
            data_dir,
            progress,
            events,
            main_agent,
            short_term: short_term.clone(),
            long_term_client: long_term_client.clone(),
            human_input_store: human_input_store.clone(),
            steering_queue: steering_queue.clone(),
            task_usage_store,
        },
    )
    .await
    {
        return resp;
    }


    if let Some(resp) = crate::api_routes_ops::try_handle(
        method,
        path,
        body.as_deref(),
        &crate::api_routes_ops::RouteCtx {
            data_dir,
            store_path,
            spec_dir,
            llm_router,
            rag_pack,
            ollama_base_url,
            restart_tx: &restart_tx,
            user_rag_store,
        },
    )
    .await
    {
        return resp;
    }

    if let Some(resp) = crate::api_routes_channels::try_handle(
        method,
        path,
        path_only,
        body.clone(),
        &crate::api_routes_channels::RouteCtx {
            data_dir,
            store_path,
            channel_config,
            main_agent,
            headers,
        },
    )
    .await
    {
        return resp;
    }

    if let Some(resp) = crate::api_routes_permissions::try_handle(
        method,
        path_only,
        body.as_deref(),
        &crate::api_routes_permissions::RouteCtx {
            data_dir,
            path,
            human_input_store: human_input_store.as_ref(),
        },
    )
    .await
    {
        return resp;
    }

    json_response("404 Not Found", r#"{"error":"not_found"}"#)
}





pub(crate) fn packaged_spec_check_ok(spec_dir: &Path) -> bool {
    spec_dir.exists() || std::env::var_os("AKASHA_SPEC_DIR").is_none()
}

#[cfg(test)]
mod tests {
    use crate::api_routes_tasks::{is_pausable, is_resumable};
    use super::{
        agent_role_system_prompt, build_image_markdown, build_session_recap_reply,
        canonicalize_tool_name, classify_small_talk_message, detect_session_recall_intent,
        should_use_compact_local_prompt,
        ensure_no_open_code_block, extract_how_to_call_from_message,
        looks_like_meta_agent_response, memory_profile_for_task, message_suggests_tool_only_action,
        normalize_tool_path_hint, packaged_spec_check_ok, parse_content_length,
        parse_device_invoke_params, parse_generate_image_tool_args, parse_sse_event_filter,
        parse_memory_store_explicit_links, parse_plugin_reputation_reset_body, parse_run_command_args,
        parse_skill_install_url, parse_tool_calls, parse_write_file_request,
        resolve_run_command_working_dir, strip_markdown_fences_from_write_content,
        code_studio_tools_for_prompt,
        response_looks_off_topic_for_small_talk, rewrite_workspace_plan_key_to_lineage_root,
        rewrite_workspace_plan_path_str, small_talk_fast_lane, PluginReputationResetBody,
        SessionRecallIntent, SessionRecallRange, SmallTalkLanguage,
    };
    use akasha_store::TaskStatus;
    use uuid::Uuid;

    #[test]
    fn parse_sse_event_filter_task_id_and_types() {
        let tid = Uuid::new_v4();
        let q = format!(
            "task_id={}&types=progress_update,task_completed",
            tid
        );
        let f = parse_sse_event_filter(&q);
        assert_eq!(f.task_id, Some(tid));
        assert!(f.event_types.as_ref().unwrap().contains("progress_update"));
    }

    #[test]
    fn parse_content_length_returns_header_end_and_content_length() {
        let buf = b"POST /api/message HTTP/1.1\r\nContent-Length: 5\r\n\r\nhello";
        let r = parse_content_length(buf);
        assert!(r.is_some());
        let (header_end, content_length) = r.unwrap();
        assert_eq!(header_end, 45); // start of "\r\n\r\n" after "Content-Length: 5\r\n"
        assert_eq!(content_length, 5);
    }

    #[test]
    fn parse_content_length_no_separator_returns_none() {
        let buf = b"POST /api/message HTTP/1.1";
        assert!(parse_content_length(buf).is_none());
    }

    #[test]
    fn parse_content_length_empty_returns_none() {
        assert!(parse_content_length(&[]).is_none());
    }

    // --- parse_device_invoke_params ---

    fn s(v: &str) -> String {
        v.to_string()
    }

    #[test]
    fn device_invoke_params_no_extra_args_returns_empty_object() {
        let args: Vec<String> = vec![s("local_media"), s("camera"), s("capture")];
        let p = parse_device_invoke_params(&args);
        assert_eq!(p, serde_json::json!({}));
    }

    #[test]
    fn device_invoke_params_single_valid_json_arg() {
        let args = vec![
            s("synthetic_input"),
            s("keyboard"),
            s("shortcut"),
            s(r#"{"keys":["Control","C"]}"#),
        ];
        let p = parse_device_invoke_params(&args);
        assert_eq!(p, serde_json::json!({"keys": ["Control", "C"]}));
    }

    #[test]
    fn device_invoke_params_single_invalid_json_falls_back_to_empty_object() {
        let args = vec![
            s("local_media"),
            s("microphone"),
            s("record"),
            s("not-json"),
        ];
        let p = parse_device_invoke_params(&args);
        assert_eq!(p, serde_json::json!({}));
    }

    #[test]
    fn device_invoke_params_multiple_args_become_json_array() {
        let args = vec![
            s("synthetic_input"),
            s("keyboard"),
            s("type"),
            s("hello"),
            s("world"),
        ];
        let p = parse_device_invoke_params(&args);
        assert_eq!(p, serde_json::json!(["hello", "world"]));
    }

    // --- parse_generate_image_tool_args (whitespace-split TOOL: lines) ---

    #[test]
    fn generate_image_args_join_words_into_prompt() {
        let args = vec![s("A"), s("cute"), s("cat"), s("playing"), s("guitar")];
        let (prompt, size) = parse_generate_image_tool_args(&args);
        assert_eq!(prompt, "A cute cat playing guitar");
        assert!(size.is_none());
    }

    #[test]
    fn generate_image_args_trailing_size_token() {
        let args = vec![s("cat"), s("on"), s("sofa"), s("1024x1024")];
        let (prompt, size) = parse_generate_image_tool_args(&args);
        assert_eq!(prompt, "cat on sofa");
        assert_eq!(size.as_deref(), Some("1024x1024"));
    }

    // --- message_suggests_tool_only_action ---

    #[test]
    fn tool_only_action_true_for_camera() {
        assert!(message_suggests_tool_only_action(
            "Prends une photo avec la caméra"
        ));
        assert!(message_suggests_tool_only_action(
            "Take a photo from the webcam"
        ));
    }

    #[test]
    fn tool_only_action_true_for_weather() {
        assert!(message_suggests_tool_only_action(
            "Quelle est la météo à Paris ?"
        ));
    }

    #[test]
    fn tool_only_action_true_for_save_file() {
        assert!(message_suggests_tool_only_action(
            "Sauvegarde ce code dans /tmp/foo.py"
        ));
    }

    #[test]
    fn tool_only_action_true_for_image_generation() {
        assert!(message_suggests_tool_only_action(
            "Génère une image d'un lapin"
        ));
    }

    #[test]
    fn tool_only_action_false_for_code_generation() {
        assert!(!message_suggests_tool_only_action(
            "Écris un script Python qui lit un fichier"
        ));
        assert!(!message_suggests_tool_only_action(
            "Génère du code pour trier une liste"
        ));
    }

    #[test]
    fn tool_only_action_false_when_code_intent_dominates() {
        // Explicit code request even if it mentions photo → do not override to conversation
        assert!(!message_suggests_tool_only_action(
            "écris un script qui prend une photo"
        ));
    }

    #[test]
    fn memory_profile_uses_fast_path_for_simple_root_requests() {
        let profile = memory_profile_for_task("Bonjour, ça va ?", "conversation", false, false);
        assert_eq!(profile.semantic_top_k, 2);
        assert_eq!(profile.user_rag_top_k, 0);
        assert_eq!(profile.workspace_graph_top_k, 0);
        assert!(!profile.expand_by_graph);
        assert!(!profile.compact_before_prompt);
    }

    #[test]
    fn memory_profile_is_lean_for_subagents() {
        let profile = memory_profile_for_task(
            "Implémente la route demandée dans le plan partagé",
            "backend",
            true,
            true,
        );
        assert_eq!(profile.semantic_top_k, 0);
        assert_eq!(profile.episodic_limit, 0);
        assert_eq!(profile.user_rag_top_k, 0);
        assert_eq!(profile.workspace_graph_top_k, 0);
        assert!(!profile.compact_before_prompt);
    }

    #[test]
    fn compact_local_prompt_for_embedded_and_small_ollama() {
        assert!(should_use_compact_local_prompt("akasha_embedded", "qwen3.5-0.8b-q4"));
        assert!(should_use_compact_local_prompt("akasha_core", "any"));
        assert!(should_use_compact_local_prompt("ollama", "tinyllama"));
        assert!(should_use_compact_local_prompt("ollama", "qwen2.5:1.5b"));
        assert!(should_use_compact_local_prompt("ollama", "llama3.2:3b"));
        assert!(should_use_compact_local_prompt("ollama", "qwen2.5:7b"));
        assert!(should_use_compact_local_prompt("ollama", "qwen3.5:9b"));
        assert!(should_use_compact_local_prompt("ollama", "mistral"));
        assert!(!should_use_compact_local_prompt("ollama", "llama3.1:70b"));
        assert!(!should_use_compact_local_prompt("openai", "gpt-4o-mini"));
    }

    #[test]
    fn small_talk_fast_lane_detects_simple_greeting() {
        let intent = small_talk_fast_lane("Salut, ça va ?").expect("small-talk should be detected");
        assert_eq!(intent.language, SmallTalkLanguage::French);
        assert!(intent.asks_status);
    }

    #[test]
    fn small_talk_fast_lane_rejects_real_request_after_greeting() {
        assert!(small_talk_fast_lane("Bonjour, peux-tu lire ce fichier ?").is_none());
    }

    #[test]
    fn extract_how_to_call_ignores_greetings() {
        assert!(extract_how_to_call_from_message("salut").is_none());
        assert!(extract_how_to_call_from_message("bonjour").is_none());
    }

    #[test]
    fn classify_small_talk_rejects_small_talk_with_real_request() {
        // "ça va merci, tu peux me rappeler ce qu'on a fait hier ?"
        // Should be rejected because it contains "peux me" + "rappeler" + "hier"
        assert!(classify_small_talk_message(
            "ça va merci, tu peux me rappeler ce qu'on a fait hier ?"
        )
        .is_none());

        // "ça va, peux-tu lire ce fichier ?" should be rejected
        assert!(classify_small_talk_message("ça va, peux-tu lire ce fichier ?").is_none());

        // But "salut, ça va ?" should still pass
        assert!(classify_small_talk_message("salut, ça va ?").is_some());
    }

    #[test]
    fn small_talk_guardrail_flags_tool_leaks() {
        assert!(response_looks_off_topic_for_small_talk(
            "TOOL: write_file c:/tmp/x.txt\nJe vais d'abord modifier tools_policy.yaml"
        ));
        assert!(!response_looks_off_topic_for_small_talk("Salut ! 👋"));
    }

    #[test]
    fn detect_session_recall_intent_for_yesterday() {
        let intent = detect_session_recall_intent("tu peux me rappeler ce qu'on a fait hier ?")
            .expect("intent should be detected");
        assert_eq!(intent.range, SessionRecallRange::Yesterday);
        assert_eq!(intent.language, SmallTalkLanguage::French);
    }

    #[test]
    fn detect_session_recall_ignores_task_recap_in_design_spec() {
        assert!(
            detect_session_recall_intent(
                "Nothing else (no task recap, no npm commands, no non-English text)."
            )
            .is_none(),
            "DESIGN.md/Code Studio boilerplate must not trigger session-recall fast path"
        );
    }

    #[test]
    fn detect_session_recall_still_detects_recap_verb() {
        let intent = detect_session_recall_intent("Can you recap what we shipped today?")
            .expect("recap request");
        assert_eq!(intent.range, SessionRecallRange::CurrentDay);
        assert_eq!(intent.language, SmallTalkLanguage::English);
    }

    #[test]
    fn detect_session_recall_no_false_positive_rappellent() {
        assert!(
            detect_session_recall_intent(
                "[Délégation obligatoire] Les sous-agents ne rappellent pas delegate_to_agent.\n\nmet à jour CODE_STUDIO_PLAN.md"
            )
            .is_none(),
            "« rappellent » must not false-positive via substring « rappel »"
        );
    }

    #[test]
    fn build_session_recap_reply_filters_noise() {
        let turns = vec![
            crate::memory::ConversationTurn {
                role: "assistant".to_string(),
                content: "tools_policy.yaml blocked write_file".to_string(),
            },
            crate::memory::ConversationTurn {
                role: "assistant".to_string(),
                content: "On a mis en place le fast-path small-talk et ajouté des garde-fous de pertinence.".to_string(),
            },
        ];
        let intent = SessionRecallIntent {
            range: SessionRecallRange::Yesterday,
            language: SmallTalkLanguage::French,
        };
        let recap = build_session_recap_reply(&turns, intent).expect("recap should exist");
        assert!(recap.contains("fast-path small-talk"));
        assert!(!recap.to_lowercase().contains("tools_policy.yaml"));
    }

    // --- parse_tool_calls (normalized markdown / list TOOL lines) ---

    #[test]
    fn parse_tool_calls_markdown_list_and_title_case_tool() {
        let s = "- Tool: write_file workspace:/out.md hello world";
        let c = parse_tool_calls(s);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].0, "write_file");
        assert!(c[0].1[0].contains("workspace:"), "{:?}", c[0].1);
    }

    #[test]
    fn parse_tool_calls_bold_wrapped_tool_keyword() {
        let s = "**TOOL:** write_file workspace:/x.md\nhello block";
        let c = parse_tool_calls(s);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].0, "write_file");
        assert_eq!(c[0].1[0], "workspace:/x.md");
        assert_eq!(c[0].1[1], "hello block");
    }

    #[test]
    fn parse_tool_calls_xml_tool_call_tags() {
        let s = "<tool_call>read_file workspace:/src/App.tsx 1 10</tool_call>\n<tool_call>read_file workspace:/src/b.tsx 1 5</tool_call>";
        let c = parse_tool_calls(s);
        assert_eq!(c.len(), 2, "{:?}", c);
        assert_eq!(c[0].0, "read_file");
        assert_eq!(c[1].0, "read_file");
    }

    #[test]
    fn parse_tool_calls_xml_tool_call_chained_without_close() {
        let s = "<tool_call>read_file workspace:/a.tsx 1 2<tool_call>read_file workspace:/b.tsx 3 4";
        let c = parse_tool_calls(s);
        assert_eq!(c.len(), 2, "{:?}", c);
        assert_eq!(c[0].0, "read_file");
        assert_eq!(c[1].0, "read_file");
    }

    /// OpenRouter / modèles qui émettent `<longcat_tool_call>…</longcat_tool_call>` au lieu de `TOOL:`.
    #[test]
    fn parse_tool_calls_longcat_read_file_inline() {
        let s = "Intro prose.<longcat_tool_call>read_file <longcat_arg_key>path <longcat_arg_value>workspace:/CODE_STUDIO_PLAN.md </longcat_tool_call>";
        let c = parse_tool_calls(s);
        assert_eq!(c.len(), 1, "{:?}", c);
        assert_eq!(c[0].0, "read_file");
        assert_eq!(c[0].1[0], "workspace:/CODE_STUDIO_PLAN.md");
    }

    #[test]
    fn parse_tool_calls_longcat_read_file_multiline() {
        let s = r#"Salut !
<longcat_tool_call>read_file
<longcat_arg_key>path
<longcat_arg_value>workspace:/package.json
</longcat_tool_call>"#;
        let c = parse_tool_calls(s);
        assert_eq!(c.len(), 1, "{:?}", c);
        assert_eq!(c[0].0, "read_file");
        assert_eq!(c[0].1[0], "workspace:/package.json");
    }

    #[test]
    fn parse_tool_calls_longcat_three_parallel_reads() {
        let s = r#"Plan.<longcat_tool_call>read_file <longcat_arg_key>path <longcat_arg_value>workspace:/CODE_STUDIO_PLAN.md </longcat_tool_call> <longcat_tool_call>read_file <longcat_arg_key>path <longcat_arg_value>workspace:/DESIGN.md </longcat_tool_call> <longcat_tool_call>read_file <longcat_arg_key>path <longcat_arg_value>workspace:/package.json </longcat_tool_call>"#;
        let c = parse_tool_calls(s);
        assert_eq!(c.len(), 3, "{:?}", c);
        assert_eq!(c[0].1[0], "workspace:/CODE_STUDIO_PLAN.md");
        assert_eq!(c[1].1[0], "workspace:/DESIGN.md");
        assert_eq!(c[2].1[0], "workspace:/package.json");
    }

    /// Kimi / modèles qui mettent toute la phrase + plusieurs `TOOL:` sur **une** ligne.
    #[test]
    fn parse_tool_calls_prose_inline_multiple_tool_on_one_line() {
        let s = "Je vérifie. TOOL: search_files workspace:/ * --no-ignore TOOL: read_file workspace:/package.json TOOL: read_file workspace:/vite.config.ts";
        let c = parse_tool_calls(s);
        assert_eq!(c.len(), 3, "{:?}", c);
        assert_eq!(c[0].0, "search_files");
        assert_eq!(
            c[0].1,
            vec![
                "workspace:/".to_string(),
                "*".to_string(),
                "--no-ignore".to_string()
            ]
        );
        assert_eq!(c[1].0, "read_file");
        assert_eq!(c[1].1, vec!["workspace:/package.json"]);
        assert_eq!(c[2].0, "read_file");
        assert_eq!(c[2].1, vec!["workspace:/vite.config.ts"]);
    }

    #[test]
    fn parse_tool_calls_search_replace_multiline_body_after_path_only() {
        let s = r#"TOOL: search_replace workspace:/CODE_STUDIO_PLAN.md
## Old section
line two | ## New section
line two new"#;
        let c = parse_tool_calls(s);
        assert_eq!(c.len(), 1, "{:?}", c);
        assert_eq!(c[0].0, "search_replace");
        assert_eq!(c[0].1.len(), 2);
        assert_eq!(c[0].1[0], "workspace:/CODE_STUDIO_PLAN.md");
        assert!(c[0].1[1].contains(" | "), "{:?}", c[0].1[1]);
    }

    #[test]
    fn parse_tool_calls_strips_redacted_thinking_then_finds_tools() {
        let s = "<think>plan</think>OK. TOOL: read_file workspace:/a.ts";
        let c = parse_tool_calls(s);
        assert_eq!(c.len(), 1, "{:?}", c);
        assert_eq!(c[0].0, "read_file");
    }

    /// DeepSeek-style `<｜DSML｜…>` (U+FF5C fullwidth vertical line).
    #[test]
    fn parse_tool_calls_dsml_search_files_fullwidth_delimiters() {
        let d = "\u{ff5c}";
        let s = format!(
            "<{d}DSML{d}tool_calls>\n\
             <{d}DSML{d}invoke name=\"search_files\">\n\
             <{d}DSML{d}parameter name=\"dir\" string=\"true\">workspace:/</{d}DSML{d}parameter>\n\
             <{d}DSML{d}parameter name=\"pattern\" string=\"true\">**/*</{d}DSML{d}parameter>\n\
             <{d}DSML{d}parameter name=\"--no-ignore\" string=\"false\">true</{d}DSML{d}parameter>\n\
             <{d}DSML{d}parameter name=\"limit\" string=\"false\">200</{d}DSML{d}parameter>\n\
             </{d}DSML{d}invoke>\n\
             </{d}DSML{d}tool_calls>",
            d = d
        );
        let c = parse_tool_calls(&s);
        assert_eq!(c.len(), 1, "{:?}", c);
        assert_eq!(c[0].0, "search_files");
        assert_eq!(c[0].1[0], "workspace:/");
        assert_eq!(c[0].1[1], "**/*");
        assert!(c[0].1.contains(&"--no-ignore".to_string()));
        assert!(
            !c[0].1.iter().any(|a| a == "200"),
            "limit must be ignored: {:?}",
            c[0].1
        );
    }

    #[test]
    fn parse_tool_calls_table_cell_with_leading_pipe() {
        let s = "| TOOL: read_file workspace:/plan.md |";
        let c = parse_tool_calls(s);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].0, "read_file");
    }

    #[test]
    fn parse_tool_calls_rejects_tools_plain_word() {
        let s = "tools: hammer and nail";
        let c = parse_tool_calls(s);
        assert!(c.is_empty());
    }

    #[test]
    fn parse_tool_calls_rejects_nested_tool_keyword_as_name() {
        let s = "TOOL: TOOL: install_skill https://github.com/bankr/cli";
        let c = parse_tool_calls(s);
        assert!(
            c.is_empty(),
            "nested TOOL: should not become a tool named TOOL:"
        );
    }

    #[test]
    fn parse_tool_calls_rejects_non_ascii_tool_name() {
        let s = "TOOL: prévision météo Paris";
        let c = parse_tool_calls(s);
        assert!(c.is_empty(), "non-ASCII pseudo tool names must be ignored");
    }

    #[test]
    fn normalize_tool_path_hint_accepts_workspace_slash_form() {
        assert_eq!(
            normalize_tool_path_hint("workspace/analyze/comparatif.md"),
            "workspace:/analyze/comparatif.md"
        );
        assert_eq!(
            normalize_tool_path_hint("workspace\\analyze\\comparatif.md"),
            "workspace:/analyze\\comparatif.md"
        );
    }

    #[test]
    fn parse_write_file_request_accepts_json_payload() {
        let args = vec![
            "{".to_string(),
            "\"path\": \"workspace/project_plan.md\",".to_string(),
            "\"content\": \"# Plan\\n- item\"".to_string(),
            "}".to_string(),
        ];
        let (path, content) = parse_write_file_request(&args).expect("json payload should parse");
        assert_eq!(path, "workspace:/project_plan.md");
        assert!(content.contains("# Plan"));
    }

    #[test]
    fn parse_write_file_request_preserves_same_line_content_spacing() {
        // All tokens come from the TOOL header (no collected body). The last token is joined
        // with a newline to be consistent with the single-line-body case — the resulting JSON
        // (with a newline before the final `}`) is still valid and parseable.
        let args = vec![
            "workspace:/tsconfig.json".to_string(),
            "{".to_string(),
            "\"compilerOptions\":".to_string(),
            "{".to_string(),
            "\"strict\":".to_string(),
            "true".to_string(),
            "}".to_string(),
            "}".to_string(),
        ];
        let (path, content) = parse_write_file_request(&args).expect("write_file should parse");
        assert_eq!(path, "workspace:/tsconfig.json");
        assert_eq!(content, "{ \"compilerOptions\": { \"strict\": true }\n}");
    }

    #[test]
    fn parse_write_file_request_combines_inline_prefix_and_single_line_body() {
        // Inline token on the TOOL header line + exactly one body line collected by the parser.
        let args = vec![
            "workspace:/src/index.js".to_string(),
            "const".to_string(),
            "x = 1;".to_string(),
            "return x;".to_string(),
        ];
        let (_, content) = parse_write_file_request(&args).expect("write_file should parse");
        assert_eq!(content, "const x = 1;\nreturn x;");
    }

    #[test]
    fn parse_write_file_request_combines_inline_prefix_and_multiline_body() {
        let args = vec![
            "workspace:/index.html".to_string(),
            "<!doctype".to_string(),
            "html>".to_string(),
            "<html>\n<body></body>\n</html>".to_string(),
        ];
        let (_, content) = parse_write_file_request(&args).expect("write_file should parse");
        assert_eq!(content, "<!doctype html>\n<html>\n<body></body>\n</html>");
    }

    #[test]
    fn strip_markdown_fences_removes_wrapping_fence() {
        let raw = "```tsx\nconst x = 1;\n```";
        assert_eq!(strip_markdown_fences_from_write_content(raw), "const x = 1;");
        let raw2 = "```\nhello\n```\n";
        assert_eq!(strip_markdown_fences_from_write_content(raw2), "hello");
        assert_eq!(
            strip_markdown_fences_from_write_content("no fence here"),
            "no fence here"
        );
    }

    #[test]
    fn parse_memory_store_explicit_links_uuid_plus_kind() {
        let u = "550e8400-e29b-41d4-a716-446655440000";
        let args = vec![format!("link_to:{u}+excludes")];
        let p = parse_memory_store_explicit_links(&args).expect("links");
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].0, u);
        assert_eq!(p[0].1, "excludes");
    }

    #[test]
    fn parse_memory_store_explicit_links_plain_uuids_and_global_kind() {
        let a = "550e8400-e29b-41d4-a716-446655440001";
        let b = "550e8400-e29b-41d4-a716-446655440002";
        let args = vec![
            format!("link_to:{a},{b}"),
            "link_kind:relates_to".to_string(),
        ];
        let p = parse_memory_store_explicit_links(&args).expect("links");
        assert_eq!(p, vec![
            (a.to_string(), "relates_to".to_string()),
            (b.to_string(), "relates_to".to_string()),
        ]);
    }

    #[test]
    fn parse_plugin_reputation_reset_body_rejects_invalid_json() {
        let parsed = parse_plugin_reputation_reset_body(Some(br#"{"plugin_id":"maps""#));
        assert!(matches!(parsed, PluginReputationResetBody::Invalid(_)));
    }

    #[test]
    fn parse_plugin_reputation_reset_body_accepts_empty_body_as_reset_all() {
        let parsed = parse_plugin_reputation_reset_body(None);
        assert!(matches!(parsed, PluginReputationResetBody::Empty));
    }

    #[test]
    fn canonicalize_tool_name_supports_create_todos_alias() {
        assert_eq!(canonicalize_tool_name("create_todos"), "write_todos");
        assert_eq!(canonicalize_tool_name("append_todos"), "merge_todos");
    }

    #[test]
    fn looks_like_meta_agent_response_flags_instruction_recitation() {
        assert!(looks_like_meta_agent_response(
            "Based on the instructions provided, I am ready to assist with this Phase 2 task."
        ));
        assert!(!looks_like_meta_agent_response(
            "Voici le rapport demandé et le fichier a été écrit dans workspace:/analyze/comparatif.md."
        ));
    }

    #[test]
    fn promise_before_tools_detects_diagnostic_preamble_fr() {
        let s = "Salut Loïc ! Je vais d'abord examiner l'état actuel du projet pour comprendre ce qui est déjà en place et ce qui fait échouer le build, puis je planifierai et implémenterai les fonctionnalités manquantes. Commençons par un diagnostic.";
        assert!(crate::api_studio::looks_like_code_studio_promise_before_any_tools(s));
    }

    #[test]
    fn promise_before_tools_detects_inspect_and_delegate_wording() {
        let s = "Je vais d'abord inspecter l'état actuel du projet pour identifier précisément ce qui manque, puis planifier et déléguer l'implémentation.";
        assert!(crate::api_studio::looks_like_code_studio_promise_before_any_tools(s));
    }

    #[test]
    fn promise_before_tools_false_when_long_prose() {
        let s = "Je vais ".to_string() + &"x".repeat(1700);
        assert!(!crate::api_studio::looks_like_code_studio_promise_before_any_tools(&s));
    }

    #[test]
    fn skip_zero_tool_retry_for_planner_only() {
        assert!(crate::api_studio::code_studio_skip_zero_tool_mandatory_retry(
            "studio_planner"
        ));
        assert!(!crate::api_studio::code_studio_skip_zero_tool_mandatory_retry(
            "studio_project_manager"
        ));
    }

    // Headings with textual content before `TOOL:` are not treated as tool calls.
    // Only markdown "junk" (e.g. `#` heading markers, list bullets) may precede `TOOL:`.
    #[test]
    fn parse_tool_calls_heading_then_tool_on_same_line() {
        let s = "### Step 4 — TOOL: write_file workspace:/out.md hello";
        let c = parse_tool_calls(s);
        assert!(
            c.is_empty(),
            "alphabetic heading text before `TOOL:` must not be parsed as a tool call"
        );
    }

    #[test]
    fn parse_tool_calls_rejects_tool_in_long_prose_line() {
        let s = "According to the documentation and the specification we note that TOOL: write_file workspace:/x y";
        let c = parse_tool_calls(s);
        assert!(
            c.is_empty(),
            "long alphabetic prefix before TOOL: must not become a false tool call"
        );
    }

    // After the fix, sloppy "- Tool: …" headers DO terminate multiline bodies.
    // A line like "- Tool: read_file …" inside a body is treated as the start of a new tool call.
    #[test]
    fn parse_tool_calls_sloppy_header_terminates_multiline_body() {
        // write_file body is ended when "- Tool: read_file …" appears — new tool starts.
        let s = "TOOL: write_file workspace:/docs/tools.md\n# Available tools\n\n- Tool: read_file — reads a file\n- Tool: write_file — writes a file\n\nEnd of list";
        let c = parse_tool_calls(s);
        // The "- Tool: read_file" line terminates the write_file body and starts a new call.
        assert!(c.len() >= 2, "expected at least 2 tool calls, got {:?}", c);
        assert_eq!(c[0].0, "write_file");
        // The write_file body ends just before "- Tool: read_file …".
        let body = &c[0].1[1];
        assert!(
            !body.contains("End of list"),
            "write_file body should be truncated at the sloppy header, got: {:?}",
            body
        );
        // The second call should be read_file (started by the sloppy header).
        assert_eq!(c[1].0, "read_file");
    }

    #[test]
    fn parse_tool_calls_body_with_heading_tool_mention_not_truncated() {
        // edit_file body contains "### Step — TOOL: read_file …" — must NOT split here.
        let s = "- Tool: edit_file workspace:/spec.md\n### Step — TOOL: read_file some args\nmore content\nTOOL: read_file workspace:/next.md";
        let c = parse_tool_calls(s);
        assert_eq!(c.len(), 2, "expected edit_file + read_file, got {:?}", c);
        assert_eq!(c[0].0, "edit_file");
        let body = &c[0].1[1];
        assert!(
            body.contains("### Step — TOOL: read_file"),
            "body should contain the heading tool mention verbatim, got: {:?}",
            body
        );
        assert_eq!(c[1].0, "read_file");
    }

    #[test]
    fn parse_tool_calls_body_terminated_by_canonical_tool_line() {
        // After a body-supporting tool, a canonical TOOL: line correctly ends the body.
        let s = "TOOL: write_file workspace:/a.txt\nsome content\nTOOL: read_file workspace:/b.txt";
        let c = parse_tool_calls(s);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].0, "write_file");
        assert_eq!(c[1].0, "read_file");
    }

    #[test]
    fn parse_tool_calls_sloppy_provider_all_headers_after_multiline_tool() {
        // Providers that always emit sloppy "- Tool: …" headers must not lose tool calls
        // that come after a multiline-body tool (write_file / edit_file / apply_patch).
        let s = concat!(
            "- Tool: write_file workspace:/out.txt\n",
            "hello content\n",
            "- Tool: read_file workspace:/in.txt\n",
            "- Tool: list_dir workspace:/\n",
        );
        let c = parse_tool_calls(s);
        assert_eq!(
            c.len(),
            3,
            "expected write_file + read_file + list_dir, got {:?}",
            c
        );
        assert_eq!(c[0].0, "write_file");
        assert_eq!(c[1].0, "read_file");
        assert_eq!(c[2].0, "list_dir");
    }

    #[test]
    fn parse_tool_calls_bold_sloppy_provider_after_multiline_tool() {
        // "**TOOL:** …" style also terminates a multiline body.
        let s = concat!(
            "**TOOL:** write_file workspace:/out.txt\n",
            "some generated content\n",
            "**TOOL:** read_file workspace:/plan.md\n",
        );
        let c = parse_tool_calls(s);
        assert_eq!(c.len(), 2, "expected write_file + read_file, got {:?}", c);
        assert_eq!(c[0].0, "write_file");
        assert_eq!(c[1].0, "read_file");
    }

    // --- merged_for_tools selection logic ---
    // These tests mirror the merge branch `a_has_tools && !r_has_tools`
    // introduced to fix the sloppy-prefix streaming case.

    /// Helper that reproduces the merge logic from `run_message_via_llm`.
    fn merged_for_tools(response_text: &str, accumulated: &str) -> String {
        let r = response_text.trim();
        let a = accumulated.trim();
        let a_has_tools = !a.is_empty() && !parse_tool_calls(a).is_empty();
        let r_has_tools = !r.is_empty() && !parse_tool_calls(r).is_empty();
        if r.is_empty() && !a.is_empty() {
            a.to_string()
        } else if a_has_tools && !r_has_tools {
            a.to_string()
        } else if !r.is_empty() {
            r.to_string()
        } else {
            a.to_string()
        }
    }

    #[test]
    fn merge_prefers_accumulated_when_streamed_has_title_case_tool_and_resp_does_not() {
        // Provider streams `- Tool: write_file …` but the final body omits tool lines.
        let accumulated = "- Tool: write_file workspace:/out.md hello";
        let resp_text = "I will write the file for you.";
        let merged = merged_for_tools(resp_text, accumulated);
        assert_eq!(merged, accumulated);
        // The merged string must still parse as a tool call.
        let calls = parse_tool_calls(&merged);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "write_file");
    }

    #[test]
    fn merge_prefers_accumulated_when_streamed_has_bold_tool_and_resp_does_not() {
        // Provider streams `**TOOL:** write_file …` but the final body is plain text.
        let accumulated = "**TOOL:** write_file workspace:/x.md\nhello block";
        let resp_text = "Here is your file.";
        let merged = merged_for_tools(resp_text, accumulated);
        assert_eq!(merged, accumulated);
        let calls = parse_tool_calls(&merged);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "write_file");
    }

    #[test]
    fn merge_uses_resp_text_when_both_have_tool_calls() {
        // When both accumulated and resp_text have parseable tool calls, prefer resp_text (the
        // final body is authoritative as long as it contains tools).
        let accumulated = "TOOL: write_file workspace:/a.txt\nfoo";
        let resp_text = "TOOL: write_file workspace:/b.txt\nbar";
        let merged = merged_for_tools(resp_text, accumulated);
        assert_eq!(merged, resp_text);
    }

    #[test]
    fn merge_uses_resp_text_when_accumulated_has_no_tool_calls() {
        // If the streamed chunk contains no parseable tools at all, use resp_text.
        let accumulated = "Thinking about what to do…";
        let resp_text = "TOOL: read_file workspace:/plan.md";
        let merged = merged_for_tools(resp_text, accumulated);
        assert_eq!(merged, resp_text);
    }

    // --- rewrite_workspace_plan_key_to_lineage_root ---

    #[test]
    fn rewrite_plan_key_stale_uuid_rewritten_to_lineage_root() {
        let root = Uuid::parse_str("a80fec14-d91a-4d91-a09e-77ea286c21fd").unwrap();
        let stale = Uuid::parse_str("216364b2-9308-47a7-9c2b-1c3b84c55c8c").unwrap();
        let (k, bad) =
            rewrite_workspace_plan_key_to_lineage_root(&format!(".akasha/plan_{stale}.md"), root);
        assert_eq!(bad, Some(stale));
        assert_eq!(k, format!(".akasha/plan_{root}.md"));
    }

    #[test]
    fn rewrite_plan_key_matching_root_unchanged() {
        let root = Uuid::parse_str("a80fec14-d91a-4d91-a09e-77ea286c21fd").unwrap();
        let (k, bad) =
            rewrite_workspace_plan_key_to_lineage_root(&format!(".akasha/plan_{root}.md"), root);
        assert!(bad.is_none());
        assert_eq!(k, format!(".akasha/plan_{root}.md"));
    }

    #[test]
    fn rewrite_plan_key_non_plan_paths_passthrough() {
        let root = Uuid::nil();
        let (k, bad) =
            rewrite_workspace_plan_key_to_lineage_root("certification_ai/exo/x.md", root);
        assert!(bad.is_none());
        assert_eq!(k, "certification_ai/exo/x.md");
    }

    // --- rewrite_workspace_plan_path_str ---

    #[test]
    fn rewrite_workspace_plan_path_str_stale_uuid_rewritten() {
        let root = Uuid::parse_str("a80fec14-d91a-4d91-a09e-77ea286c21fd").unwrap();
        let stale = Uuid::parse_str("216364b2-9308-47a7-9c2b-1c3b84c55c8c").unwrap();
        // rewrite_workspace_plan_path_str calls workspace_lineage_root_task_id which needs a real
        // store, so we test the path-string wrapper via a nil store_path (lineage = task_id = root).
        let result = rewrite_workspace_plan_path_str(
            &format!("workspace:/.akasha/plan_{stale}.md"),
            root,
            None,
        );
        assert_eq!(result, format!("workspace:/.akasha/plan_{root}.md"));
    }

    #[test]
    fn rewrite_workspace_plan_path_str_non_workspace_unchanged() {
        let root = Uuid::parse_str("a80fec14-d91a-4d91-a09e-77ea286c21fd").unwrap();
        let stale = Uuid::parse_str("216364b2-9308-47a7-9c2b-1c3b84c55c8c").unwrap();
        let input = format!("/abs/path/.akasha/plan_{stale}.md");
        let result = rewrite_workspace_plan_path_str(&input, root, None);
        assert_eq!(result, input);
    }

    #[test]
    fn rewrite_workspace_plan_path_str_non_plan_workspace_path_unchanged() {
        let root = Uuid::parse_str("a80fec14-d91a-4d91-a09e-77ea286c21fd").unwrap();
        let result =
            rewrite_workspace_plan_path_str("workspace:/certification_ai/exo/x.md", root, None);
        assert_eq!(result, "workspace:/certification_ai/exo/x.md");
    }

    // --- agent_role_system_prompt ---

    #[test]
    fn agent_role_prompt_some_for_recognized_types() {
        let with_role = [
            "code",
            "search",
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
            "studio_reviewer",
        ];
        for t in &with_role {
            let s = agent_role_system_prompt(t);
            assert!(s.is_some(), "agent_type {:?} should have a role prompt", t);
            assert!(
                !s.unwrap().is_empty(),
                "agent_type {:?} role prompt must be non-empty",
                t
            );
        }
    }

    #[test]
    fn agent_role_prompt_none_for_conversation_and_unknown() {
        assert!(agent_role_system_prompt("conversation").is_none());
        assert!(agent_role_system_prompt("").is_none());
        // schedule is handled specially (recurring task), no role prompt
        assert!(agent_role_system_prompt("schedule").is_none());
    }

    #[test]
    fn code_studio_tools_for_reviewer_are_read_only_plus_ticket_updates() {
        let tools = code_studio_tools_for_prompt(None, "studio_reviewer");
        assert!(tools.iter().any(|t| t == "read_file"));
        assert!(tools.iter().any(|t| t == "studio_list_tickets"));
        assert!(tools.iter().any(|t| t == "studio_update_ticket"));
        for forbidden in [
            "write_file",
            "write_code",
            "delete_file",
            "rename_path",
            "move_tree",
            "edit_file",
            "apply_patch",
            "search_replace",
            "delegate_to_agent",
        ] {
            assert!(
                !tools.iter().any(|t| t == forbidden),
                "tool `{forbidden}` must not be available for studio_reviewer"
            );
        }
    }

    // --- ensure_no_open_code_block ---

    #[test]
    fn ensure_no_open_code_block_zero_backticks() {
        let s = "hello";
        assert_eq!(ensure_no_open_code_block(s), "hello");
    }

    #[test]
    fn ensure_no_open_code_block_one_backtick_open() {
        let s = "code:\n```";
        assert_eq!(ensure_no_open_code_block(s), "code:\n```\n```\n");
    }

    #[test]
    fn ensure_no_open_code_block_two_backticks_closed() {
        let s = "```\nfn x() {}\n```";
        assert_eq!(ensure_no_open_code_block(s), "```\nfn x() {}\n```");
    }

    // --- build_image_markdown ---

    #[test]
    fn build_image_markdown_data_url() {
        let url = "data:image/jpeg;base64,ABC";
        let out = build_image_markdown("Photo", url);
        assert!(
            out.contains("](<data:image/"),
            "output should contain ](<data:image/: {:?}",
            out
        );
        assert!(out.ends_with(">)"), "output should end with >): {:?}", out);
        assert_eq!(out, "\n\n![Photo](<data:image/jpeg;base64,ABC>)");
    }

    // --- is_pausable / is_resumable state transitions ---

    #[test]
    fn pausable_includes_pending_queued_running_and_waiting_input() {
        assert!(is_pausable(&TaskStatus::Pending));
        assert!(is_pausable(&TaskStatus::Queued));
        assert!(is_pausable(&TaskStatus::Running));
        assert!(is_pausable(&TaskStatus::WaitingUserInput));
        assert!(!is_pausable(&TaskStatus::Paused));
        assert!(!is_pausable(&TaskStatus::Completed));
        assert!(!is_pausable(&TaskStatus::Failed));
        assert!(!is_pausable(&TaskStatus::Cancelled));
        assert!(!is_pausable(&TaskStatus::Interrupted));
    }

    #[test]
    fn resumable_only_paused_and_interrupted() {
        assert!(is_resumable(&TaskStatus::Paused));
        assert!(is_resumable(&TaskStatus::Interrupted));
        // All other statuses are not resumable
        assert!(!is_resumable(&TaskStatus::Pending));
        assert!(!is_resumable(&TaskStatus::Queued));
        assert!(!is_resumable(&TaskStatus::Running));
        assert!(!is_resumable(&TaskStatus::Completed));
        assert!(is_resumable(&TaskStatus::Failed));
        assert!(!is_resumable(&TaskStatus::Cancelled));
        assert!(!is_resumable(&TaskStatus::WaitingUserInput));
    }

    #[test]
    fn interrupted_status_is_resumable_but_not_pausable() {
        // Interrupted tasks (daemon-restart survivors) must be resumable, not pausable
        assert!(is_resumable(&TaskStatus::Interrupted));
        assert!(!is_pausable(&TaskStatus::Interrupted));
    }

    #[test]
    fn task_status_filter_strings_are_canonical() {
        // Verify that status as_str() values match the strings used in query-param filtering
        assert_eq!(TaskStatus::Pending.as_str(), "pending");
        assert_eq!(TaskStatus::Queued.as_str(), "queued");
        assert_eq!(TaskStatus::Running.as_str(), "running");
        assert_eq!(TaskStatus::Completed.as_str(), "completed");
        assert_eq!(TaskStatus::Failed.as_str(), "failed");
        assert_eq!(TaskStatus::Paused.as_str(), "paused");
        assert_eq!(TaskStatus::Cancelled.as_str(), "cancelled");
        assert_eq!(TaskStatus::Interrupted.as_str(), "interrupted");
        assert_eq!(TaskStatus::WaitingUserInput.as_str(), "waiting_user_input");
    }

    #[test]
    fn packaged_spec_check_is_ok_without_override_for_installed_daemon() {
        let dir = tempfile::tempdir().unwrap();
        std::env::remove_var("AKASHA_SPEC_DIR");

        assert!(packaged_spec_check_ok(dir.path()));
    }

    #[test]
    fn parse_skill_install_url_accepts_github_tree_url() {
        // Standard GitHub tree URL for a skill directory
        let allowed = vec!["github.com".into(), "raw.githubusercontent.com".into()];
        let r = parse_skill_install_url(
            "https://github.com/BankrBot/skills/tree/main/bankr",
            &allowed,
        );
        assert!(r.is_some(), "GitHub tree URL must be accepted");
        let parsed = r.unwrap();
        assert_eq!(parsed.skill_name, "bankr");
        assert!(
            parsed.raw_skill_url.contains("raw.githubusercontent.com"),
            "raw URL must point to raw.githubusercontent.com"
        );
    }

    #[test]
    fn parse_skill_install_url_rejects_disallowed_host() {
        // Host not in the allowed list must be rejected to prevent SSRF
        let allowed = vec!["github.com".into(), "raw.githubusercontent.com".into()];
        let r = parse_skill_install_url("https://evil.example.com/bad.md", &allowed);
        assert!(
            r.is_none(),
            "URL from a non-allowed host must be rejected by parse_skill_install_url"
        );
    }

    #[test]
    fn parse_skill_install_url_wildcard_allows_any_https() {
        // When allowed_hosts contains "*", any HTTPS URL is allowed
        let allowed = vec!["*".into()];
        let r = parse_skill_install_url("https://example.com/my-skill/SKILL.md", &allowed);
        assert!(
            r.is_some(),
            "Wildcard allowed_hosts must accept any HTTPS URL"
        );
    }

    #[test]
    fn parse_skill_install_url_rejects_http_scheme() {
        // Only HTTPS is allowed; plain HTTP must be rejected
        let allowed = vec!["*".into()];
        let r = parse_skill_install_url("http://github.com/user/skills/tree/main/bankr", &allowed);
        assert!(
            r.is_none(),
            "HTTP scheme must be rejected (only HTTPS is allowed)"
        );
    }

    // --- parse_run_command_args / resolve_run_command_working_dir ---

    #[test]
    fn parse_run_command_args_extracts_cwd_after_vault() {
        let args = vec![
            s("VAULT:foo=BAR"),
            s("--cwd"),
            s("workspace:/"),
            s("git"),
            s("status"),
        ];
        let (vault, cwd, cmd, a) = parse_run_command_args(&args);
        assert_eq!(vault.len(), 1);
        assert_eq!(cwd.as_deref(), Some("workspace:/"));
        assert_eq!(cmd, "git");
        assert_eq!(a, vec![s("status")]);
    }

    #[test]
    fn parse_run_command_args_no_cwd() {
        let args = vec![s("cargo"), s("build")];
        let (_, cwd, cmd, a) = parse_run_command_args(&args);
        assert!(cwd.is_none());
        assert_eq!(cmd, "cargo");
        assert_eq!(a, vec![s("build")]);
    }

    #[test]
    fn resolve_run_command_working_dir_default_off_returns_none() {
        let mut p = akasha_tools::ToolsPolicy::default();
        p.run_command_default_cwd_workspace = false;
        let tmp = tempfile::tempdir().unwrap();
        p.allowed_read_paths = vec![tmp.path().to_string_lossy().to_string()];
        let r = resolve_run_command_working_dir(None, Some(tmp.path()), &p).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn resolve_run_command_working_dir_workspace_when_policy_on() {
        let mut p = akasha_tools::ToolsPolicy::default();
        p.run_command_default_cwd_workspace = true;
        let tmp = tempfile::tempdir().unwrap();
        p.allowed_read_paths = vec![tmp.path().to_string_lossy().to_string()];
        let r = resolve_run_command_working_dir(None, Some(tmp.path()), &p).unwrap();
        let got = r.expect("expected workspace cwd");
        let exp = std::fs::canonicalize(tmp.path()).unwrap();
        let got_c = std::fs::canonicalize(&got).unwrap();
        assert_eq!(got_c, exp);
    }

    // --- tool_scope_key ---

    #[test]
    fn tool_scope_key_run_command_joins_first_two_args() {
        let args = vec![s("git"), s("status"), s("--short")];
        let key = super::tool_scope_key("run_command", &args);
        assert_eq!(key, "git status");
    }

    #[test]
    fn tool_scope_key_delete_file_joins_all_args_for_spaced_paths() {
        let args = vec![s("Cas"), s("d'usage.pdf")];
        let key = super::tool_scope_key("delete_file", &args);
        assert_eq!(key, "Cas d'usage.pdf");
    }

    #[test]
    fn tool_scope_key_delete_file_single_arg() {
        let args = vec![s("/tmp/test.txt")];
        let key = super::tool_scope_key("delete_file", &args);
        assert_eq!(key, "/tmp/test.txt");
    }

    #[test]
    fn tool_scope_key_write_file_uses_plain_path_from_first_arg() {
        let args = vec![s("/tmp/hello.txt"), s("content here")];
        let key = super::tool_scope_key("write_file", &args);
        assert_eq!(key, "/tmp/hello.txt");
    }

    #[test]
    fn tool_scope_key_write_file_parses_json_path() {
        let args = vec![s(r#"{"path":"/tmp/from_json.txt","content":"hello"}"#)];
        let key = super::tool_scope_key("write_file", &args);
        assert_eq!(key, "/tmp/from_json.txt");
    }

    #[test]
    fn tool_scope_key_edit_file_uses_first_arg() {
        let args = vec![s("/some/file.rs"), s("1"), s("5"), s("new content")];
        let key = super::tool_scope_key("edit_file", &args);
        assert_eq!(key, "/some/file.rs");
    }

    #[test]
    fn tool_scope_key_unknown_tool_returns_global() {
        let args = vec![s("whatever")];
        let key = super::tool_scope_key("unknown_tool", &args);
        assert_eq!(key, "global");
    }

    #[test]
    fn tool_scope_key_empty_args_returns_global_for_write_file() {
        let args: Vec<String> = vec![];
        let key = super::tool_scope_key("write_file", &args);
        assert_eq!(key, "global");
    }

    // --- permissions_center (load/save/lookup) ---

    #[test]
    fn permissions_center_save_load_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let state = crate::permissions_center::PermissionCenterState {
            decisions: vec![crate::permissions_center::PermissionDecision {
                id: "abc".to_string(),
                tool: "delete_file".to_string(),
                scope: "/tmp/foo.txt".to_string(),
                mode: crate::permissions_center::DecisionMode::AllowPersistent,
                created_at: "2024-01-01T00:00:00Z".to_string(),
                expires_at: None,
            }],
        };
        crate::permissions_center::save(tmp.path(), &state).unwrap();
        let loaded = crate::permissions_center::load(tmp.path());
        assert_eq!(loaded.decisions.len(), 1);
        assert_eq!(loaded.decisions[0].tool, "delete_file");
        assert_eq!(loaded.decisions[0].scope, "/tmp/foo.txt");
        assert_eq!(loaded.decisions[0].mode, crate::permissions_center::DecisionMode::AllowPersistent);
    }

    #[test]
    fn permissions_center_lookup_returns_matching_decision() {
        let state = crate::permissions_center::PermissionCenterState {
            decisions: vec![
                crate::permissions_center::PermissionDecision {
                    id: "d1".to_string(),
                    tool: "write_file".to_string(),
                    scope: "/tmp/a.txt".to_string(),
                    mode: crate::permissions_center::DecisionMode::DenyPersistent,
                    created_at: "2024-01-01T00:00:00Z".to_string(),
                    expires_at: None,
                },
            ],
        };
        let found = crate::permissions_center::lookup("write_file", "/tmp/a.txt", &state);
        assert!(found.is_some());
        assert_eq!(found.unwrap().mode, crate::permissions_center::DecisionMode::DenyPersistent);
    }

    #[test]
    fn permissions_center_lookup_returns_none_for_different_scope() {
        let state = crate::permissions_center::PermissionCenterState {
            decisions: vec![crate::permissions_center::PermissionDecision {
                id: "d1".to_string(),
                tool: "write_file".to_string(),
                scope: "/tmp/a.txt".to_string(),
                mode: crate::permissions_center::DecisionMode::AllowPersistent,
                created_at: "2024-01-01T00:00:00Z".to_string(),
                expires_at: None,
            }],
        };
        let not_found = crate::permissions_center::lookup("write_file", "/tmp/b.txt", &state);
        assert!(not_found.is_none());
    }

    #[test]
    fn permissions_center_lookup_returns_none_for_different_tool() {
        let state = crate::permissions_center::PermissionCenterState {
            decisions: vec![crate::permissions_center::PermissionDecision {
                id: "d1".to_string(),
                tool: "write_file".to_string(),
                scope: "global".to_string(),
                mode: crate::permissions_center::DecisionMode::AllowPersistent,
                created_at: "2024-01-01T00:00:00Z".to_string(),
                expires_at: None,
            }],
        };
        let not_found = crate::permissions_center::lookup("delete_file", "global", &state);
        assert!(not_found.is_none());
    }

    #[test]
    fn permissions_center_save_is_atomic_via_tmp_rename() {
        // Verify no .tmp file is left behind after a successful save.
        let tmp = tempfile::tempdir().unwrap();
        let state = crate::permissions_center::PermissionCenterState::default();
        crate::permissions_center::save(tmp.path(), &state).unwrap();
        let tmp_file = tmp.path().join("permissions_center.json.tmp");
        assert!(!tmp_file.exists(), ".tmp file should not remain after save");
    }
}
