//! Akasha TUI — Interface terminal (ratatui + crossterm).
//! Onglets Chat et Routeur (métriques), statut daemon, envoi de messages avec polling tâche.
//! Thèmes (cycle F2) et rendu markdown (tables, listes, blocs de code).

mod markdown;
mod theme;

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, Tabs, Wrap},
};
use theme::{Theme, ThemeName};
use std::io::{self, Stdout};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const DAEMON_PORT: u16 = 3876;
const TASK_POLL_INTERVAL_MS: u64 = 1500;
const TASK_POLL_TIMEOUT_SECS: u64 = 600;

/// Label in French for task/event types (Task Center, spec 09_event_model).
fn activity_event_label(typ: &str) -> String {
    match typ {
        "user_request_received" => "Demande reçue".into(),
        "acknowledgment_sent" => "Accusé de réception envoyé".into(),
        "task_created" => "Tâche créée".into(),
        "task_started" => "Tâche démarrée".into(),
        "task_decomposed" => "Tâche décomposée (délégation à des sous-agents)".into(),
        "sub_agent_spawned" => "Délégué à un agent spécialisé".into(),
        "progress_update" => "Progression".into(),
        "task_progress_updated" => "Progression mise à jour".into(),
        "task_step_completed" => "Étape terminée".into(),
        "task_completed" => "Tâche terminée".into(),
        "task_failed" => "Tâche en échec".into(),
        "task_run_created" => "Run planifié créé".into(),
        "schedule_created" => "Récurrence créée".into(),
        "schedule_updated" => "Récurrence mise à jour".into(),
        "schedule_deleted" => "Récurrence supprimée".into(),
        _ => typ.to_string(),
    }
}

fn daemon_base_url(port: u16) -> String {
    format!("http://127.0.0.1:{}", port)
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Chat,
    Router,
    Doc,
    Tasks,
    Calendar,
    Memory,
}

#[derive(Clone)]
struct ChatMessage {
    role: String,
    text: String,
    is_error: bool,
}

/// One row in the Activity task list (from GET /api/tasks).
#[derive(Clone)]
struct ActivityTaskRow {
    id: String,
    status: String,
    created_at: String,
    assigned_agent: String,
}

/// Full detail for selected task (from GET /api/tasks/:id + user message from events).
#[derive(Clone, Default)]
struct ActivityTaskDetail {
    created_at: String,
    updated_at: String,
    status: String,
    assigned_agent: String,
    /// Progress entries (e.g. reply text); last one is usually the full reply.
    progress: Vec<(u8, String)>,
    /// User message that started this task (from first user_request_received event).
    user_message: Option<String>,
}

#[derive(Clone, Default, serde::Deserialize)]
struct ModelMetrics {
    total_requests: u64,
    successful_requests: u64,
    failed_requests: u64,
    total_latency_ms: u64,
    total_tokens: u64,
    fallback_triggered: u64,
    fallback_success: u64,
}

struct App {
    mode: Mode,
    messages: Vec<ChatMessage>,
    input: String,
    daemon_ok: bool,
    loading: bool,
    metrics: std::collections::HashMap<String, ModelMetrics>,
    scroll: usize,
    /// Set by ui() in Chat/Doc mode: total line count and content area height for scroll clamping.
    last_content_lines: usize,
    last_content_area_height: u16,
    /// Rendered row count (wrap-aware) for Chat; 0 = use last_content_lines (e.g. Doc).
    last_content_rendered_rows: usize,
    /// User guide markdown (fetched from GET /api/docs when opening Doc tab).
    doc_content: String,
    /// Activity tab: list of tasks (id, status, created_at, agent); selected index; events + detail for selected.
    activity_tasks: Vec<ActivityTaskRow>,
    activity_selected: usize,
    activity_events: Vec<String>,
    /// Detail of selected task (progress/reply, metadata). Fetched with GET /api/tasks/:id.
    activity_task_detail: Option<ActivityTaskDetail>,
    /// User message that started the selected task (from events).
    activity_user_message: Option<String>,
    /// Activity tab: vertical scroll offset for the detail block (PgUp/PgDn).
    activity_detail_scroll: usize,
    /// Calendar tab: schedules and task_runs (FR-029).
    calendar_schedules: Vec<(String, String, bool, Option<u64>)>,
    calendar_task_runs: Vec<(String, String, String, String)>,
    #[allow(dead_code)]
    calendar_scroll: usize,
    /// Pending task id after ack (show "En cours: Task #xxx" in chat).
    pending_reply_task_id: Option<String>,
    /// Session id for short-term memory (returned by daemon, send back on next message).
    session_id: Option<String>,
    /// If true, next message will request a new session (context reset).
    force_new_session: bool,
    /// Memory tab: short-term turns (role, content) for current session.
    memory_short_term: Vec<(String, String)>,
    /// Memory tab: long-term entries (content, created_at, source).
    memory_long_term: Vec<(String, String, String)>,
    /// Whether long-term memory is available (daemon has embeddings).
    memory_long_term_available: bool,
    /// Current theme (cycle with F2).
    theme: ThemeName,
    port: u16,
    /// (content, session_id, pending_task_id). When pending_task_id is Some, reply will follow later.
    tx: mpsc::Sender<Result<(String, String, Option<String>), String>>,
    /// Scroll offset for input area when text wraps to more lines than visible (Ctrl+↑/↓).
    input_scroll: usize,
    /// Set by ui(): inner height of input area for clamping input_scroll.
    input_inner_height: usize,
    /// Set by ui(): wrapped line count of input text.
    input_wrapped_lines: usize,
}

impl App {
    fn new(port: u16, tx: mpsc::Sender<Result<(String, String, Option<String>), String>>) -> Self {
        Self {
            mode: Mode::Chat,
            messages: Vec::new(),
            input: String::new(),
            daemon_ok: false,
            loading: false,
            metrics: std::collections::HashMap::new(),
            scroll: 0,
            last_content_lines: 0,
            last_content_area_height: 0,
            last_content_rendered_rows: 0,
            doc_content: String::new(),
            activity_tasks: Vec::new(),
            activity_selected: 0,
            activity_events: Vec::new(),
            activity_task_detail: None,
            activity_user_message: None,
            activity_detail_scroll: 0,
            calendar_schedules: Vec::new(),
            calendar_task_runs: Vec::new(),
            calendar_scroll: 0,
            pending_reply_task_id: None,
            session_id: None,
            force_new_session: false,
            memory_short_term: Vec::new(),
            memory_long_term: Vec::new(),
            memory_long_term_available: false,
            theme: ThemeName::default(),
            port,
            tx,
            input_scroll: 0,
            input_inner_height: 3,
            input_wrapped_lines: 0,
        }
    }

    /// Number of wrapped lines for a string given line width (chars).
    fn wrapped_line_count(text: &str, width: usize) -> usize {
        if width == 0 {
            return 1;
        }
        text.lines()
            .map(|line| {
                let w = unicode_width::UnicodeWidthStr::width(line).max(1);
                (w + width - 1) / width
            })
            .sum::<usize>()
            .max(1)
    }

    fn input_scroll_up(&mut self) {
        self.input_scroll = self.input_scroll.saturating_sub(1);
    }

    fn input_scroll_down(&mut self) {
        let max = self.input_wrapped_lines.saturating_sub(self.input_inner_height);
        if self.input_scroll < max {
            self.input_scroll += 1;
        }
    }

    fn fetch_activity_tasks(&mut self) {
        let base = daemon_base_url(self.port);
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        if let Ok(resp) = client.get(format!("{}/api/tasks", base)).send() {
            if resp.status().is_success() {
                if let Ok(json) = resp.json::<serde_json::Value>() {
                    let list = json.get("tasks").and_then(|t| t.as_array()).cloned().unwrap_or_default();
                    self.activity_tasks = list
                        .iter()
                        .filter_map(|t| {
                            let id = t.get("id").and_then(|v| v.as_str()).map(String::from)?;
                            let status = t.get("status").and_then(|v| v.as_str()).unwrap_or("?").to_string();
                            let created_at = t.get("created_at").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let assigned_agent = t.get("assigned_agent").and_then(|v| v.as_str()).unwrap_or("—").to_string();
                            Some(ActivityTaskRow { id, status, created_at, assigned_agent })
                        })
                        .collect();
                    if self.activity_selected >= self.activity_tasks.len() && !self.activity_tasks.is_empty() {
                        self.activity_selected = self.activity_tasks.len() - 1;
                    }
                    if !self.activity_tasks.is_empty() {
                        self.fetch_activity_events_for_selected();
                        self.fetch_activity_task_detail();
                    } else {
                        self.activity_events.clear();
                        self.activity_task_detail = None;
                    }
                }
            }
        }
    }

    fn fetch_activity_events_for_selected(&mut self) {
        let id = match self.activity_tasks.get(self.activity_selected) {
            Some(row) => row.id.clone(),
            None => {
                self.activity_events.clear();
                self.activity_task_detail = None;
                self.activity_user_message = None;
                return;
            }
        };
        let base = daemon_base_url(self.port);
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        if let Ok(resp) = client.get(format!("{}/api/tasks/{}/events", base, id)).send() {
            if resp.status().is_success() {
                if let Ok(json) = resp.json::<serde_json::Value>() {
                    let list = json.get("events").and_then(|e| e.as_array()).cloned().unwrap_or_default();
                    self.activity_user_message = list
                        .iter()
                        .find(|e| e.get("event_type").and_then(|v| v.as_str()) == Some("user_request_received"))
                        .and_then(|e| e.get("payload")).and_then(|p| p.get("message")).and_then(|v| v.as_str())
                        .map(String::from);
                    self.activity_events = list
                        .iter()
                        .filter_map(|e| {
                            let typ = e.get("event_type").and_then(|v| v.as_str()).unwrap_or("?");
                            let label = activity_event_label(typ);
                            let at = e.get("at").and_then(|v| v.as_str()).unwrap_or("");
                            let payload = e.get("payload").cloned();
                            let line = if let Some(p) = payload {
                                format!("{} @ {} — {}", label, at, serde_json::to_string(&p).unwrap_or_default())
                            } else {
                                format!("{} @ {}", label, at)
                            };
                            Some(line)
                        })
                        .collect();
                    return;
                }
            }
        }
        self.activity_events.clear();
        self.activity_user_message = None;
    }

    fn fetch_activity_task_detail(&mut self) {
        let id = match self.activity_tasks.get(self.activity_selected) {
            Some(row) => row.id.clone(),
            None => return,
        };
        let base = daemon_base_url(self.port);
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        if let Ok(resp) = client.get(format!("{}/api/tasks/{}", base, id)).send() {
            if resp.status().is_success() {
                if let Ok(json) = resp.json::<serde_json::Value>() {
                    let progress: Vec<(u8, String)> = json.get("progress")
                        .and_then(|p| p.as_array())
                        .map(|arr| arr.iter().filter_map(|e| {
                            let pct = e.get("progress_pct").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
                            let msg = e.get("message").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            Some((pct, msg))
                        }).collect())
                        .unwrap_or_default();
                    self.activity_task_detail = Some(ActivityTaskDetail {
                        created_at: json.get("created_at").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                        updated_at: json.get("updated_at").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                        status: json.get("status").and_then(|v| v.as_str()).unwrap_or("?").to_string(),
                        assigned_agent: json.get("assigned_agent").and_then(|v| v.as_str()).unwrap_or("—").to_string(),
                        progress,
                        user_message: self.activity_user_message.clone(),
                    });
                    return;
                }
            }
        }
        self.activity_task_detail = None;
    }

    fn fetch_calendar(&mut self) {
        let base = daemon_base_url(self.port);
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        self.calendar_schedules.clear();
        self.calendar_task_runs.clear();
        if let Ok(resp) = client.get(format!("{}/api/schedules", base)).send() {
            if resp.status().is_success() {
                if let Ok(json) = resp.json::<serde_json::Value>() {
                    if let Some(arr) = json.get("schedules").and_then(|a| a.as_array()) {
                        for s in arr {
                            let id = s.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let name = s.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let enabled = s.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
                            let interval_seconds = s.get("interval_seconds").and_then(|v| v.as_u64());
                            self.calendar_schedules.push((id, name, enabled, interval_seconds));
                        }
                    }
                }
            }
        }
        if let Ok(resp) = client.get(format!("{}/api/task_runs", base)).send() {
            if resp.status().is_success() {
                if let Ok(json) = resp.json::<serde_json::Value>() {
                    if let Some(arr) = json.get("task_runs").and_then(|a| a.as_array()) {
                        for r in arr.iter().take(50) {
                            let id = r.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let status = r.get("status").and_then(|v| v.as_str()).unwrap_or("?").to_string();
                            let planned_for = r.get("planned_for").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let task_id = r.get("task_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            self.calendar_task_runs.push((id, status, planned_for, task_id));
                        }
                    }
                }
            }
        }
    }

    fn fetch_memory(&mut self) {
        let base = daemon_base_url(self.port);
        let session_param = self.session_id.as_deref().map(|s| format!("?session_id={}", urlencoding::encode(s)));
        let short_url = match &session_param {
            Some(p) => format!("{}/api/memory/short-term{}", base, p),
            None => format!("{}/api/memory/short-term", base),
        };
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        if let Ok(resp) = client.get(&short_url).send() {
            if resp.status().is_success() {
                if let Ok(json) = resp.json::<serde_json::Value>() {
                    let turns = json.get("turns").and_then(|t| t.as_array()).cloned().unwrap_or_default();
                    self.memory_short_term = turns
                        .iter()
                        .filter_map(|t| {
                            let role = t.get("role")?.as_str()?.to_string();
                            let content = t.get("content")?.as_str()?.to_string();
                            Some((role, content))
                        })
                        .collect();
                }
            } else {
                self.memory_short_term.clear();
            }
        } else {
            self.memory_short_term.clear();
        }
        let long_url = format!("{}/api/memory/long-term?limit=50", base);
        if let Ok(resp) = client.get(&long_url).send() {
            if resp.status().is_success() {
                if let Ok(json) = resp.json::<serde_json::Value>() {
                    self.memory_long_term_available = json.get("long_term_available").and_then(|v| v.as_bool()).unwrap_or(false);
                    let entries = json.get("entries").and_then(|e| e.as_array()).cloned().unwrap_or_default();
                    self.memory_long_term = entries
                        .iter()
                        .filter_map(|e| {
                            let content = e.get("content")?.as_str()?.to_string();
                            let created_at = e.get("created_at")?.as_str()?.to_string();
                            let source = e.get("source")?.as_str()?.to_string();
                            Some((content, created_at, source))
                        })
                        .collect();
                }
            } else {
                self.memory_long_term.clear();
                self.memory_long_term_available = false;
            }
        } else {
            self.memory_long_term.clear();
            self.memory_long_term_available = false;
        }
    }

    fn fetch_doc(&mut self) {
        let url = format!("{}/api/docs", daemon_base_url(self.port));
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        if let Ok(resp) = client.get(&url).send() {
            if resp.status().is_success() {
                if let Ok(json) = resp.json::<serde_json::Value>() {
                    if let Some(s) = json.get("content").and_then(|c| c.as_str()) {
                        self.doc_content = s.to_string();
                        return;
                    }
                }
            }
        }
        self.doc_content = "Documentation non disponible (daemon requis : akasha start).".to_string();
    }

    /// Max scroll offset (0 if content fits in area). Uses rendered rows when wrap-aware (Chat).
    fn max_scroll(&self) -> usize {
        let h = self.last_content_area_height as usize;
        let content_len = if self.last_content_rendered_rows > 0 {
            self.last_content_rendered_rows
        } else {
            self.last_content_lines
        };
        content_len.saturating_sub(h)
    }

    fn scroll_down(&mut self) {
        let max = self.max_scroll();
        if self.scroll < max {
            self.scroll += 1;
        }
    }

    fn scroll_up(&mut self) {
        self.scroll = self.scroll.saturating_sub(1);
    }

    fn scroll_page_down(&mut self) {
        let page = self.last_content_area_height as usize;
        self.scroll = (self.scroll + page).min(self.max_scroll());
    }

    fn scroll_page_up(&mut self) {
        let page = self.last_content_area_height as usize;
        self.scroll = self.scroll.saturating_sub(page);
    }

    fn scroll_to_bottom(&mut self) {
        self.scroll = self.max_scroll();
    }

    fn check_health(&mut self) {
        let url = format!("{}/", daemon_base_url(self.port));
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap_or_default();
        self.daemon_ok = client
            .get(&url)
            .send()
            .map(|r| r.status().is_success())
            .unwrap_or(false);
    }

    fn fetch_metrics(&mut self) {
        let url = format!("{}/api/router/metrics", daemon_base_url(self.port));
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        if let Ok(resp) = client.get(&url).send() {
            if resp.status().is_success() {
                if let Ok(json) =
                    resp.json::<std::collections::HashMap<String, ModelMetrics>>()
                {
                    self.metrics = json;
                }
            }
        }
    }

    /// Non-blocking: POST /api/message, send ack via tx, then poll and send final reply (FR-025).
    fn send_message_non_blocking(
        tx: mpsc::Sender<Result<(String, String, Option<String>), String>>,
        message: String,
        port: u16,
        session_id: Option<String>,
        new_session: bool,
    ) {
        let base = daemon_base_url(port);
        let url = format!("{}/api/message", base);
        let client = match reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(Err(format!("Client: {}", e)));
                return;
            }
        };
        let body = if new_session {
            serde_json::json!({ "message": message, "new_session": true })
        } else if let Some(ref s) = session_id {
            serde_json::json!({ "message": message, "session_id": s })
        } else {
            serde_json::json!({ "message": message })
        };
        let resp = match client.post(&url).json(&body).send() {
            Ok(r) => r,
            Err(e) => {
                let _ = tx.send(Err(format!("Daemon unreachable: {}", e)));
                return;
            }
        };
        if !resp.status().is_success() {
            let _ = tx.send(Err(format!("Daemon returned {}", resp.status())));
            return;
        }
        let json: serde_json::Value = match resp.json() {
            Ok(j) => j,
            Err(e) => {
                let _ = tx.send(Err(e.to_string()));
                return;
            }
        };
        let task_id = json.get("task_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let session_id = json.get("session_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let ack_msg = json.get("message").and_then(|v| v.as_str()).unwrap_or("Je prends en compte votre demande.");
        let ack_text = if task_id.is_empty() {
            ack_msg.to_string()
        } else {
            let short = if task_id.len() > 8 { &task_id[task_id.len()-8..] } else { &task_id[..] };
            format!("{}\n\nTu peux suivre l'avancement dans l'onglet Tâches. Task #{}", ack_msg, short)
        };
        let _ = tx.send(Ok((ack_text, session_id.clone(), if task_id.is_empty() { None } else { Some(task_id.clone()) })));
        if task_id.is_empty() {
            return;
        }
        let task_url = format!("{}/api/tasks/{}", base, task_id);
        let deadline = std::time::Instant::now() + Duration::from_secs(TASK_POLL_TIMEOUT_SECS);
        let mut last_message = String::new();
        loop {
            if std::time::Instant::now() > deadline {
                let _ = tx.send(Ok((
                    if last_message.is_empty() { "Délai dépassé. Consultez l'onglet Tâches.".to_string() } else { last_message },
                    session_id,
                    None,
                )));
                return;
            }
            thread::sleep(Duration::from_millis(TASK_POLL_INTERVAL_MS));
            let poll = match client.get(&task_url).timeout(Duration::from_secs(5)).send() {
                Ok(r) => r,
                Err(_) => continue,
            };
            if !poll.status().is_success() {
                continue;
            }
            let task_json: serde_json::Value = match poll.json() {
                Ok(j) => j,
                Err(_) => continue,
            };
            if let Some(progress) = task_json.get("progress").and_then(|p| p.as_array()) {
                if let Some(last) = progress.last() {
                    if let Some(msg) = last.get("message").and_then(|m| m.as_str()) {
                        last_message = msg.to_string();
                    }
                }
            }
            let status = task_json.get("status").and_then(|v| v.as_str()).unwrap_or("");
            if status == "completed" {
                let _ = tx.send(Ok((
                    if last_message.is_empty() { "Terminé.".to_string() } else { last_message },
                    session_id,
                    None,
                )));
                return;
            }
            if status == "failed" {
                let _ = tx.send(Ok((
                    if last_message.is_empty() { "Tâche en échec.".to_string() } else { last_message },
                    session_id,
                    None,
                )));
                return;
            }
        }
    }

    /// Blocking send (kept for tests or fallback). Returns (reply_text, session_id).
    #[allow(dead_code)]
    fn send_message_blocking(message: String, port: u16, session_id: Option<&str>, new_session: bool) -> Result<(String, String), String> {
        let base = daemon_base_url(port);
        let url = format!("{}/api/message", base);
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| e.to_string())?;
        let body = if new_session {
            serde_json::json!({ "message": message, "new_session": true })
        } else if let Some(s) = session_id {
            serde_json::json!({ "message": message, "session_id": s })
        } else {
            serde_json::json!({ "message": message })
        };
        let resp = client
            .post(&url)
            .json(&body)
            .send()
            .map_err(|e| format!("Daemon unreachable: {}", e))?;
        if !resp.status().is_success() {
            return Err(format!("Daemon returned {}", resp.status()));
        }
        let json: serde_json::Value = resp.json().map_err(|e| e.to_string())?;
        let task_id = json.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
        let session_id = json.get("session_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if task_id.is_empty() {
            return Ok(("Message received.".to_string(), session_id));
        }
        let task_url = format!("{}/api/tasks/{}", base, task_id);
        let deadline =
            std::time::Instant::now() + Duration::from_secs(TASK_POLL_TIMEOUT_SECS);
        let mut last_message = String::new();
        loop {
            if std::time::Instant::now() > deadline {
                return Ok((
                    if last_message.is_empty() { "Request timed out.".to_string() } else { last_message },
                    session_id.clone(),
                ));
            }
            thread::sleep(Duration::from_millis(TASK_POLL_INTERVAL_MS));
            let poll = client
                .get(&task_url)
                .timeout(Duration::from_secs(5))
                .send();
            let resp = match poll {
                Ok(r) => r,
                Err(_) => continue,
            };
            if !resp.status().is_success() {
                continue;
            }
            let task_json: serde_json::Value = match resp.json() {
                Ok(j) => j,
                Err(_) => continue,
            };
            if let Some(progress) = task_json.get("progress").and_then(|p| p.as_array()) {
                if let Some(last) = progress.last() {
                    if let Some(msg) = last.get("message").and_then(|m| m.as_str()) {
                        last_message = msg.to_string();
                    }
                }
            }
            let status = task_json.get("status").and_then(|v| v.as_str()).unwrap_or("");
            if status == "completed" {
                return Ok((
                    if last_message.is_empty() { "Done.".to_string() } else { last_message },
                    session_id.clone(),
                ));
            }
            if status == "failed" {
                return Ok((
                    if last_message.is_empty() { "Task failed.".to_string() } else { last_message },
                    session_id.clone(),
                ));
            }
        }
    }

    /// Run a slash command (e.g. /status, /metrics), return result text.
    fn run_slash_command_blocking(port: u16, input: &str) -> String {
        let input = input.trim().trim_start_matches('/').trim();
        let parts: Vec<&str> = input.split_whitespace().collect();
        let cmd = parts.get(0).map(|s| *s).unwrap_or("").to_lowercase();
        let base = daemon_base_url(port);
        let client = match reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
        {
            Ok(c) => c,
            Err(e) => return format!("Erreur client: {}", e),
        };

        match cmd.as_str() {
            "help" | "?" => {
                return r#"Commandes disponibles:
  /help, /?         — cette aide
  /newsession       — repartir de zéro (nouvelle session, contexte court terme effacé)
  /status           — état du daemon
  /doctor           — diagnostic (daemon, ollama, vault, spec)
  /advice           — conseil diagnostic (RAG + modèle)
  /embedded         — statut du modèle local embarqué
  /embedded reload  — décharger le modèle (rechargé au prochain appel)
  /metrics          — métriques du routeur LLM
  /models           — liste des modèles (tous les providers)
  /models list      — modèles par catégorie (primary + fallback)
  /models set CAT PROV MODÈLE — définir le modèle pour une catégorie (ex. conversation ollama llama3.2)
  /routes           — modèles par catégorie (primary + fallback)
  /config list      — variables (akasha.env)
  /config get KEY   — valeur d'une variable
  /config set K V   — définir variable (K=V dans akasha.env)
  /vault list       — clés du vault (noms uniquement)
  /plugins          — liste des plugins
  /reload           — recharger les plugins
  /restart          — redémarrer le daemon (superviseur)
  /vault set        — utiliser le CLI : akasha vault set KEY [value]"#.to_string();
            }
            "status" => {
                let url = format!("{}/api/status", base);
                match client.get(&url).send() {
                    Ok(r) if r.status().is_success() => return "Daemon : OK".to_string(),
                    _ => return "Daemon : déconnecté ou erreur".to_string(),
                }
            }
            "doctor" => {
                let url = format!("{}/api/doctor", base);
                match client.get(&url).send() {
                    Ok(r) if r.status().is_success() => {
                        if let Ok(json) = r.json::<serde_json::Value>() {
                            let empty: Vec<serde_json::Value> = vec![];
                            let checks = json.get("checks").and_then(|c| c.as_array()).unwrap_or(&empty);
                            let mut out = String::from("Doctor — diagnostic\n");
                            for c in checks {
                                let id = c.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                                let ok = c.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                                let desc = c.get("description").and_then(|v| v.as_str()).unwrap_or("");
                                out.push_str(&format!("  [{}] {} — {}\n", if ok { "OK" } else { "KO" }, id, desc));
                            }
                            let all_ok = json.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                            out.push_str(if all_ok { "\nTous les checks sont OK." } else { "\nCertains checks ont échoué." });
                            return out;
                        }
                    }
                    _ => {}
                }
                return "Impossible de récupérer le diagnostic.".to_string();
            }
            "advice" => {
                let doctor_url = format!("{}/api/doctor", base);
                let advice_url = format!("{}/api/diagnostic/advice", base);
                if let Ok(r) = client.get(&doctor_url).send() {
                    if r.status().is_success() {
                        if let Ok(doctor_json) = r.json::<serde_json::Value>() {
                            let body = serde_json::json!({ "health": doctor_json });
                            if let Ok(adv_resp) = client.post(&advice_url).json(&body).timeout(Duration::from_secs(180)).send() {
                                let ok = adv_resp.status().is_success();
                                if let Ok(adv_json) = adv_resp.json::<serde_json::Value>() {
                                    if ok {
                                        let advice = adv_json.get("advice").and_then(|v| v.as_str()).unwrap_or("").trim();
                                        let model = adv_json.get("model_used").and_then(|v| v.as_str()).unwrap_or("?");
                                        if advice.is_empty() {
                                            return format!("(Aucun conseil retourné. Modèle utilisé : {}.)", model);
                                        }
                                        return format!("Conseil diagnostic (modèle: {})\n\n{}", model, advice);
                                    }
                                    if let Some(detail) = adv_json.get("detail").and_then(|v| v.as_str()) {
                                        return format!("Conseil indisponible : {}", detail);
                                    }
                                }
                            }
                        }
                    }
                }
                return "Impossible de récupérer le conseil (daemon + LLM requis). Tapez /embedded pour vérifier le modèle local.".to_string();
            }
            "embedded" => {
                let sub = parts.get(1).map(|s| s.to_lowercase()).unwrap_or_default();
                if sub == "reload" {
                    let url = format!("{}/api/router/embedded/reload", base);
                    match client.post(&url).send() {
                        Ok(r) if r.status().is_success() => {
                            if let Ok(json) = r.json::<serde_json::Value>() {
                                let msg = json.get("message").and_then(|v| v.as_str()).unwrap_or("Modèle déchargé.");
                                return msg.to_string();
                            }
                        }
                        _ => {}
                    }
                    return "Impossible de recharger (daemon ou routeur).".to_string();
                }
                let url = format!("{}/api/router/embedded-status", base);
                match client.get(&url).send() {
                    Ok(r) if r.status().is_success() => {
                        if let Ok(json) = r.json::<serde_json::Value>() {
                            let available = json.get("embedded_available").and_then(|v| v.as_bool()).unwrap_or(false);
                            let loaded = json.get("embedded_loaded").and_then(|v| v.as_bool()).unwrap_or(false);
                            let hint = json.get("hint").and_then(|v| v.as_str()).unwrap_or("");
                            let status = if !available {
                                "non disponible"
                            } else if loaded {
                                "disponible et chargé (prêt)"
                            } else {
                                "disponible (chargement au 1ᵉʳ appel, 5–15 min possibles)"
                            };
                            return format!("Modèle embarqué : {}\n{}", status, hint);
                        }
                    }
                    _ => {}
                }
                return "Impossible de joindre le daemon ou routeur.".to_string();
            }
            "plugins" => {
                let url = format!("{}/api/plugins", base);
                match client.get(&url).send() {
                    Ok(r) if r.status().is_success() => {
                        if let Ok(list) = r.json::<Vec<serde_json::Value>>() {
                            if list.is_empty() {
                                return "Aucun plugin installé.".to_string();
                            }
                            let mut out = String::new();
                            for p in list {
                                let id = p.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                                let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                                let version = p.get("version").and_then(|v| v.as_str()).unwrap_or("?");
                                out.push_str(&format!("{} — {} ({})\n", id, name, version));
                            }
                            return out;
                        }
                    }
                    _ => {}
                }
                return "Impossible de lister les plugins.".to_string();
            }
            "reload" => {
                let url = format!("{}/api/plugins/reload", base);
                match client.post(&url).send() {
                    Ok(r) if r.status().is_success() => return "Plugins rechargés.".to_string(),
                    Ok(r) => return format!("Erreur: {}", r.status()),
                    Err(e) => return format!("Erreur: {}", e),
                }
            }
            "metrics" => {
                let url = format!("{}/api/router/metrics", base);
                match client.get(&url).send() {
                    Ok(r) if r.status().is_success() => {
                        if let Ok(json) = r.json::<std::collections::HashMap<String, serde_json::Value>>() {
                            if json.is_empty() {
                                return "Aucune métrique (envoyez un message pour en générer).".to_string();
                            }
                            let mut out = String::new();
                            for (model, m) in &json {
                                let req = m.get("total_requests").and_then(|v| v.as_u64()).unwrap_or(0);
                                let ok = m.get("successful_requests").and_then(|v| v.as_u64()).unwrap_or(0);
                                let fail = m.get("failed_requests").and_then(|v| v.as_u64()).unwrap_or(0);
                                let lat = m.get("total_latency_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                                out.push_str(&format!("{} : {} requêtes ({} ok, {} échecs), {} ms\n", model, req, ok, fail, lat));
                            }
                            return out;
                        }
                    }
                    _ => {}
                }
                return "Impossible de récupérer les métriques.".to_string();
            }
            "models" => {
                let sub = parts.get(1).map(|s| s.to_lowercase()).unwrap_or_default();
                if sub == "list" {
                    let url = format!("{}/api/router/routes", base);
                    match client.get(&url).send() {
                        Ok(r) if r.status().is_success() => {
                            if let Ok(routes) = r.json::<serde_json::Value>() {
                                let obj = match routes.as_object() {
                                    Some(o) => o,
                                    None => return "Aucune route configurée.".to_string(),
                                };
                                let mut cats: Vec<_> = obj.keys().collect();
                                cats.sort();
                                let mut out = String::from("Modèles par catégorie (primary + fallback)\n\n");
                                for cat in cats {
                                    let tt = match routes.get(cat).and_then(|v| v.as_object()) {
                                        Some(t) => t,
                                        None => continue,
                                    };
                                    let primary = tt
                                        .get("primary")
                                        .and_then(|p| p.as_object())
                                        .map(|p| format!("{} / {}", p.get("provider").and_then(|v| v.as_str()).unwrap_or("?"), p.get("model").and_then(|v| v.as_str()).unwrap_or("?")))
                                        .unwrap_or_else(|| "(aucun)".to_string());
                                    out.push_str(&format!("  {}:\n    primary: {}\n", cat, primary));
                                    let empty: Vec<serde_json::Value> = vec![];
                                    let fallback = tt.get("fallback").and_then(|f| f.as_array()).unwrap_or(&empty);
                                    if fallback.is_empty() {
                                        out.push_str("    fallback: (aucun)\n");
                                    } else {
                                        for (i, e) in fallback.iter().enumerate() {
                                            let obj = e.as_object();
                                            let line = obj.map(|o| {
                                                let p = o.get("provider").and_then(|v| v.as_str()).unwrap_or("?");
                                                let m = o.get("model").and_then(|v| v.as_str()).unwrap_or("?");
                                                format!("{} / {}", p, m)
                                            }).unwrap_or_else(|| "?".to_string());
                                            out.push_str(&format!("    fallback[{}]: {}\n", i, line));
                                        }
                                    }
                                }
                                return out;
                            }
                        }
                        _ => {}
                    }
                    return "Impossible de récupérer les routes.".to_string();
                }
                if sub == "set" {
                    let category = parts.get(2).map(|s| (*s).to_string());
                    let provider = parts.get(3).map(|s| (*s).to_string());
                    let model = if parts.len() > 4 {
                        parts[4..].join(" ").trim().to_string()
                    } else {
                        parts.get(4).map(|s| (*s).to_string()).unwrap_or_default()
                    };
                    match (category, provider, model) {
                        (Some(cat), Some(prov), modl) if !cat.is_empty() && !prov.is_empty() && !modl.is_empty() => {
                            let url = format!("{}/api/router/route", base);
                            let body = serde_json::json!({ "category": cat, "provider": prov, "model": modl });
                            match client.post(&url).json(&body).send() {
                                Ok(r) if r.status().is_success() => {
                                    if let Ok(json) = r.json::<serde_json::Value>() {
                                        let msg = json.get("message").and_then(|v| v.as_str()).unwrap_or("Route mise à jour.");
                                        return format!("{} — {}", json.get("category").and_then(|v| v.as_str()).unwrap_or(""), msg);
                                    }
                                }
                                Ok(r) => {
                                    let err = r.text().unwrap_or_default();
                                    let detail: String = serde_json::from_str::<serde_json::Value>(&err)
                                        .ok()
                                        .and_then(|j| j.get("error").and_then(|v| v.as_str().map(String::from)))
                                        .unwrap_or(err);
                                    return format!("Erreur: {}", detail);
                                }
                                Err(e) => return format!("Erreur: {}", e),
                            }
                        }
                        _ => return "Usage: /models set CATÉGORIE PROVIDER MODÈLE (ex. /models set conversation ollama llama3.2)".to_string(),
                    }
                }
                let url = format!("{}/api/router/models", base);
                match client.get(&url).send() {
                    Ok(r) if r.status().is_success() => {
                        if let Ok(json) = r.json::<serde_json::Value>() {
                            let providers = match json.get("providers").and_then(|p| p.as_object()) {
                                Some(o) => o,
                                None => return "Aucun modèle configuré.".to_string(),
                            };
                            if providers.is_empty() {
                                return "Aucun modèle configuré.".to_string();
                            }
                            let mut out = String::new();
                            for (provider, arr) in providers {
                                let models = match arr.as_array() {
                                    Some(a) => a,
                                    None => continue,
                                };
                                let names: Vec<String> = models
                                    .iter()
                                    .filter_map(|m| m.as_str().map(String::from))
                                    .collect();
                                if names.is_empty() {
                                    continue;
                                }
                                out.push_str(&format!("{}:\n", provider));
                                for n in &names {
                                    out.push_str(&format!("  {}\n", n));
                                }
                            }
                            if out.is_empty() {
                                return "Aucun modèle listé.".to_string();
                            }
                            return out;
                        }
                    }
                    _ => {}
                }
                return "Impossible de récupérer la liste des modèles.".to_string();
            }
            "routes" => {
                let url = format!("{}/api/router/routes", base);
                match client.get(&url).send() {
                    Ok(r) if r.status().is_success() => {
                        if let Ok(routes) = r.json::<serde_json::Value>() {
                            let obj = match routes.as_object() {
                                Some(o) => o,
                                None => return "Aucune route configurée.".to_string(),
                            };
                            let mut cats: Vec<_> = obj.keys().collect();
                            cats.sort();
                            let mut out = String::from("Modèles par catégorie (primary + fallback)\n\n");
                            for cat in cats {
                                let tt = match routes.get(cat).and_then(|v| v.as_object()) {
                                    Some(t) => t,
                                    None => continue,
                                };
                                let primary = tt
                                    .get("primary")
                                    .and_then(|p| p.as_object())
                                    .map(|p| format!("{} / {}", p.get("provider").and_then(|v| v.as_str()).unwrap_or("?"), p.get("model").and_then(|v| v.as_str()).unwrap_or("?")))
                                    .unwrap_or_else(|| "(aucun)".to_string());
                                out.push_str(&format!("  {}:\n    primary: {}\n", cat, primary));
                                let empty: Vec<serde_json::Value> = vec![];
                                let fallback = tt.get("fallback").and_then(|f| f.as_array()).unwrap_or(&empty);
                                if fallback.is_empty() {
                                    out.push_str("    fallback: (aucun)\n");
                                } else {
                                    for (i, e) in fallback.iter().enumerate() {
                                        let obj = e.as_object();
                                        let line = obj.map(|o| {
                                            let p = o.get("provider").and_then(|v| v.as_str()).unwrap_or("?");
                                            let m = o.get("model").and_then(|v| v.as_str()).unwrap_or("?");
                                            format!("{} / {}", p, m)
                                        }).unwrap_or_else(|| "?".to_string());
                                        out.push_str(&format!("    fallback[{}]: {}\n", i, line));
                                    }
                                }
                            }
                            return out;
                        }
                    }
                    _ => {}
                }
                return "Impossible de récupérer les routes.".to_string();
            }
            "config" => {
                let sub = parts.get(1).map(|s| *s).unwrap_or("").to_lowercase();
                match sub.as_str() {
                    "list" => {
                        let url = format!("{}/api/config", base);
                        match client.get(&url).send() {
                            Ok(r) if r.status().is_success() => {
                                if let Ok(json) = r.json::<serde_json::Value>() {
                                    let vars = match json.get("vars").and_then(|v| v.as_object()) {
                                        Some(m) => m,
                                        None => return "Aucune variable (akasha.env vide).".to_string(),
                                    };
                                    if vars.is_empty() {
                                        return "Aucune variable (akasha.env vide).".to_string();
                                    }
                                    let mut out = String::new();
                                    for (k, v) in vars {
                                        out.push_str(&format!("{}={}\n", k, v.as_str().unwrap_or("")));
                                    }
                                    return out;
                                }
                            }
                            _ => {}
                        }
                        return "Erreur lecture config.".to_string();
                    }
                    "get" => {
                        let key = parts.get(2).map(|s| *s);
                        if key.is_none() {
                            return "Usage: /config get KEY".to_string();
                        }
                        let url = format!("{}/api/config", base);
                        match client.get(&url).send() {
                            Ok(r) if r.status().is_success() => {
                                if let Ok(json) = r.json::<serde_json::Value>() {
                                    let vars = json.get("vars").and_then(|v| v.as_object());
                                    let val = key.and_then(|k| vars.and_then(|m| m.get(k)).and_then(|v| v.as_str()));
                                    return val.map(String::from).unwrap_or_else(|| "Clé non trouvée.".to_string());
                                }
                            }
                            _ => {}
                        }
                        return "Erreur lecture config.".to_string();
                    }
                    "set" => {
                        let key = parts.get(2).map(|s| *s);
                        let value = if parts.len() > 3 {
                            parts[3..].join(" ")
                        } else {
                            parts.get(3).map(|s| (*s).to_string()).unwrap_or_default()
                        };
                        if key.is_none() {
                            return "Usage: /config set KEY value".to_string();
                        }
                        let url = format!("{}/api/config", base);
                        let body = serde_json::json!({ "key": key.unwrap(), "value": value });
                        match client.post(&url).json(&body).send() {
                            Ok(r) if r.status().is_success() => return format!("Variable {} définie.", key.unwrap()),
                            Ok(r) => return format!("Erreur: {}", r.status()),
                            Err(e) => return format!("Erreur: {}", e),
                        }
                    }
                    _ => return "Usage: /config list | get KEY | set KEY value".to_string(),
                }
            }
            "vault" => {
                let sub = parts.get(1).map(|s| *s).unwrap_or("").to_lowercase();
                if sub == "list" {
                    let url = format!("{}/api/vault/keys", base);
                    match client.get(&url).send() {
                        Ok(r) if r.status().is_success() => {
                            if let Ok(json) = r.json::<serde_json::Value>() {
                                let keys = match json.get("keys").and_then(|k| k.as_array()) {
                                    Some(a) => a,
                                    None => return "Vault vide.".to_string(),
                                };
                                if keys.is_empty() {
                                    return "Vault vide.".to_string();
                                }
                                let names: Vec<String> = keys.iter().filter_map(|k| k.as_str().map(String::from)).collect();
                                return names.join("\n");
                            }
                        }
                        _ => {}
                    }
                    return "Erreur lecture vault.".to_string();
                }
                if sub == "set" {
                    return "Utilisez le CLI : akasha vault set KEY [value]".to_string();
                }
                return "Usage: /vault list".to_string();
            }
            "restart" => {
                let url = format!("{}/api/restart", base);
                match client.post(&url).send() {
                    Ok(r) if r.status().is_success() => return "Redémarrage demandé (le daemon va se fermer).".to_string(),
                    Ok(r) => return format!("Erreur: {}", r.status()),
                    Err(e) => return format!("Erreur: {}", e),
                }
            }
            _ if cmd.is_empty() => return "Tapez /help pour les commandes.".to_string(),
            _ => return format!("Commande inconnue: /{}. Tapez /help.", cmd),
        }
    }
}

fn ui(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
        ])
        .split(f.area());

    let (content_area, input_area_opt) = if app.mode == Mode::Chat {
        let vert = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(3)])
            .split(chunks[1]);
        (vert[0], Some(vert[1]))
    } else {
        (chunks[1], None)
    };

    let top_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Length(1)])
        .split(chunks[0]);
    let status = if app.daemon_ok {
        "Daemon connecté"
    } else {
        "Daemon déconnecté (akasha start)"
    };
    let theme = Theme::new(app.theme);
    let status_color = if app.daemon_ok {
        theme.palette().success
    } else {
        theme.palette().error
    };
    let header = Paragraph::new(format!(
        "Akasha — {} — Port {} — Thème: {} (F2)",
        status,
        app.port,
        app.theme.label()
    ))
        .block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(theme.block_border()),
        )
        .style(Style::default().fg(status_color));
    f.render_widget(header, top_chunks[0]);
    let titles = vec![" Chat ", " Routeur ", " Doc ", " Tâches ", " Calendrier ", " Mémoire "];
    let tab_index = match app.mode {
        Mode::Chat => 0,
        Mode::Router => 1,
        Mode::Doc => 2,
        Mode::Tasks => 3,
        Mode::Calendar => 4,
        Mode::Memory => 5,
    };
    let tabs = Tabs::new(titles)
        .block(Block::default().borders(Borders::BOTTOM).border_style(theme.block_border()))
        .select(tab_index)
        .style(theme.tab_inactive())
        .highlight_style(theme.tab_active());
    f.render_widget(tabs, top_chunks[1]);

    match app.mode {
        Mode::Chat => {
            let content_width = content_area.width as usize;
            let mut lines: Vec<Line<'static>> = Vec::new();
            for m in &app.messages {
                let (role_style, _base_style) = if m.role == "Vous" {
                    (
                        Style::default().fg(theme.palette().accent).add_modifier(Modifier::BOLD),
                        Style::default().fg(theme.palette().fg),
                    )
                } else if m.role == "Système" {
                    (
                        Style::default().fg(theme.palette().muted).add_modifier(Modifier::BOLD),
                        Style::default().fg(theme.palette().muted),
                    )
                } else if m.is_error {
                    (
                        Style::default().fg(theme.palette().error).add_modifier(Modifier::BOLD),
                        Style::default().fg(theme.palette().error),
                    )
                } else {
                    (
                        Style::default().fg(theme.palette().success).add_modifier(Modifier::BOLD),
                        Style::default().fg(theme.palette().fg),
                    )
                };
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!("  ─── {} ───", m.role),
                    role_style,
                )));
                let md_styles = theme.markdown_styles();
                let marked = markdown::from_str_with_width(&m.text, &md_styles, Some(content_width as u16));
                lines.extend(marked.to_flat_lines());
            }
            if let Some(ref tid) = app.pending_reply_task_id {
                lines.push(Line::from(""));
                let short = if tid.len() > 8 { &tid[tid.len()-8..] } else { tid.as_str() };
                lines.push(Line::from(Span::styled(
                    format!("  [ Task #{} en cours… ]", short),
                    Style::default().fg(theme.palette().warning).add_modifier(Modifier::ITALIC),
                )));
            }
            if app.loading {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "  … Akasha réfléchit …",
                    Style::default().fg(theme.palette().warning).add_modifier(Modifier::ITALIC),
                )));
            }
            let content_height = content_area.height.saturating_sub(2); // inner height (block borders)
            app.last_content_lines = lines.len();
            app.last_content_area_height = content_height;
            // Wrap-aware row count so scrolling shows full content (Paragraph wraps to area width).
            app.last_content_rendered_rows = if content_width > 0 {
                lines
                    .iter()
                    .map(|l| {
                        let w = l.width() as usize;
                        if w == 0 {
                            1
                        } else {
                            (w + content_width - 1) / content_width
                        }
                    })
                    .sum()
            } else {
                lines.len()
            };
            let max_scroll = app.max_scroll();
            if app.scroll > max_scroll {
                app.scroll = max_scroll;
            }
            let chat = Paragraph::new(lines)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Chat (↑↓ PgUp/PgDn défilement, F2 thème) ")
                        .border_style(theme.block_border()),
                )
                .wrap(Wrap { trim: true })
                .scroll((app.scroll as u16, 0));
            f.render_widget(chat, content_area);
        }
        Mode::Router => {
            let rows: Vec<Row> = app
                .metrics
                .iter()
                .map(|(k, m)| {
                    Row::new(vec![
                        Cell::from(k.clone()),
                        Cell::from(m.total_requests.to_string()),
                        Cell::from(m.successful_requests.to_string()),
                        Cell::from(m.failed_requests.to_string()),
                        Cell::from(m.total_latency_ms.to_string()),
                        Cell::from(m.total_tokens.to_string()),
                        Cell::from(format!("{} / {}", m.fallback_triggered, m.fallback_success)),
                    ])
                })
                .collect();
            let table = Table::new(
                rows,
                [
                    Constraint::Min(20),
                    Constraint::Length(8),
                    Constraint::Length(8),
                    Constraint::Length(6),
                    Constraint::Length(10),
                    Constraint::Length(8),
                    Constraint::Length(12),
                ],
            )
            .header(Row::new(vec![
                "Modèle",
                "Requêtes",
                "Réussies",
                "Échecs",
                "Latence ms",
                "Tokens",
                "Fallbacks",
            ]).style(Style::default().fg(theme.palette().accent)))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Métriques routeur (R: rafraîchir) ")
                    .border_style(theme.block_border()),
            );
            f.render_widget(table, content_area);
        }
        Mode::Doc => {
            let content_width = content_area.width as usize;
            let md_styles = theme.markdown_styles();
            let marked = markdown::from_str_with_width(
                &app.doc_content,
                &md_styles,
                Some(content_width as u16),
            );
            let lines = marked.to_flat_lines();
            let content_height = content_area.height.saturating_sub(2);
            app.last_content_lines = lines.len();
            app.last_content_area_height = content_height;
            app.last_content_rendered_rows = 0;
            let max_scroll = app.max_scroll();
            if app.scroll > max_scroll {
                app.scroll = max_scroll;
            }
            let doc_para = Paragraph::new(lines)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Documentation (↑↓ PgUp/PgDn, R: actualiser, F2: thème) ")
                        .border_style(theme.block_border()),
                )
                .wrap(Wrap { trim: true })
                .scroll((app.scroll as u16, 0));
            f.render_widget(doc_para, content_area);
        }
        Mode::Tasks => {
            let area = content_area;
            let (list_area, detail_area) = if area.height >= 8 {
                let list_h = (area.height / 2).max(4);
                let chunks_act = ratatui::layout::Layout::default()
                    .direction(ratatui::layout::Direction::Vertical)
                    .constraints([
                        ratatui::layout::Constraint::Length(list_h),
                        ratatui::layout::Constraint::Min(4),
                    ])
                    .split(area);
                (chunks_act[0], chunks_act[1])
            } else {
                (area, area)
            };

            let mut list_lines: Vec<Line<'static>> = vec![
                Line::from(""),
                Line::from(Span::styled(
                    " Tâches récentes — ↑↓ sélectionner, R actualiser. ID = identifiant de la demande envoyée.",
                    Style::default().fg(theme.palette().accent),
                )),
                Line::from(Span::styled(
                    " Date       │ Statut    │ Agent  │ ID (extrait) ",
                    Style::default().fg(theme.palette().muted),
                )),
            ];
            for (i, row) in app.activity_tasks.iter().enumerate() {
                let short_date = if row.created_at.len() >= 16 {
                    format!("{} {}", &row.created_at[5..10], &row.created_at[11..16])
                } else {
                    row.created_at.clone()
                };
                let short_id = if row.id.len() > 8 {
                    format!("…{}", &row.id[row.id.len()-8..])
                } else {
                    row.id.clone()
                };
                let style = if i == app.activity_selected {
                    Style::default().fg(theme.palette().accent).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.palette().fg)
                };
                list_lines.push(Line::from(Span::styled(
                    format!("  {} {} │ {:9} │ {:6} │ {}", if i == app.activity_selected { "►" } else { " " }, short_date, row.status, row.assigned_agent, short_id),
                    style,
                )));
            }
            if app.activity_tasks.is_empty() {
                list_lines.push(Line::from("  Aucune tâche. Envoyez un message dans Chat pour en créer."));
            }
            let list_len = list_lines.len();
            let list_block = Block::default()
                .borders(Borders::ALL)
                .title(" Liste des tâches ")
                .border_style(theme.block_border());
            f.render_widget(Paragraph::new(list_lines).block(list_block), list_area);

            let mut detail_lines: Vec<Line<'static>> = vec![Line::from("")];
            if let Some(d) = &app.activity_task_detail {
                detail_lines.push(Line::from(Span::styled(" Détails de la tâche sélectionnée ", Style::default().fg(theme.palette().accent))));
                detail_lines.push(Line::from(""));
                if let Some(ref msg) = d.user_message {
                    let preview = if msg.len() > 80 { format!("{}…", &msg[..80]) } else { msg.clone() };
                    detail_lines.push(Line::from(Span::styled(" Demande : ", Style::default().fg(theme.palette().warning))));
                    for line in preview.lines() {
                        detail_lines.push(Line::from(format!("   {}", line)));
                    }
                    detail_lines.push(Line::from(""));
                }
                if let Some(last) = d.progress.last() {
                    let reply_preview = if last.1.len() > 200 { format!("{}…", &last.1[..200]) } else { last.1.clone() };
                    detail_lines.push(Line::from(Span::styled(" Réponse : ", Style::default().fg(theme.palette().success))));
                    for line in reply_preview.lines() {
                        detail_lines.push(Line::from(format!("   {}", line)));
                    }
                    detail_lines.push(Line::from(""));
                }
                detail_lines.push(Line::from(format!("  Créé : {}  │  Mis à jour : {}  │  Statut : {}  │  Agent : {}", d.created_at, d.updated_at, d.status, d.assigned_agent)));
                detail_lines.push(Line::from(""));
                detail_lines.push(Line::from(Span::styled(" Événements ", Style::default().fg(theme.palette().muted))));
            }
            for ev in &app.activity_events {
                detail_lines.push(Line::from(format!("  {}", ev)));
            }
            if app.activity_task_detail.is_none() && !app.activity_tasks.is_empty() {
                detail_lines.push(Line::from("  Sélectionnez une tâche ci-dessus pour voir la demande et la réponse."));
            }
            let detail_len = detail_lines.len();
            let detail_inner_height = detail_area.height.saturating_sub(2) as usize; // block borders
            let max_detail_scroll = detail_len.saturating_sub(detail_inner_height);
            app.activity_detail_scroll = app.activity_detail_scroll.min(max_detail_scroll);
            let detail_block = Block::default()
                .borders(Borders::ALL)
                .title(" Détails et événements (PgUp/PgDn défilement) ")
                .border_style(theme.block_border());
            f.render_widget(
                Paragraph::new(detail_lines)
                    .block(detail_block)
                    .wrap(Wrap { trim: true })
                    .scroll((app.activity_detail_scroll as u16, 0)),
                detail_area,
            );

            app.last_content_lines = list_len + detail_len;
            app.last_content_area_height = area.height;
            app.last_content_rendered_rows = 0;
        }
        Mode::Calendar => {
            let mut lines: Vec<Line<'static>> = vec![
                Line::from(""),
                Line::from(Span::styled(
                    " Récurrences (schedules) — R = actualiser ",
                    Style::default().fg(theme.palette().accent).add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
            ];
            if app.calendar_schedules.is_empty() {
                lines.push(Line::from(Span::styled("  Aucune récurrence.", Style::default().fg(theme.palette().muted))));
            } else {
                for (id, name, enabled, interval_secs) in &app.calendar_schedules {
                    let short_id = if id.len() > 8 { format!("…{}", &id[id.len()-8..]) } else { id.clone() };
                    let status = if *enabled { "activée" } else { "en pause" };
                    let interval = interval_secs.map(|s| format!(" — {}s", s)).unwrap_or_default();
                    lines.push(Line::from(format!("  {}  {}  {}  {}", short_id, name, status, interval)));
                }
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                " Runs récents (task_runs) ",
                Style::default().fg(theme.palette().accent).add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
            if app.calendar_task_runs.is_empty() {
                lines.push(Line::from(Span::styled("  Aucun run.", Style::default().fg(theme.palette().muted))));
            } else {
                for (id, status, planned_for, task_id) in &app.calendar_task_runs {
                    let short_id = if id.len() > 8 { &id[id.len()-8..] } else { id.as_str() };
                    let short_task = if task_id.len() > 8 { format!("…{}", &task_id[task_id.len()-8..]) } else { task_id.clone() };
                    let planned = if planned_for.len() >= 19 { &planned_for[..19] } else { planned_for.as_str() };
                    lines.push(Line::from(format!("  {}  {}  {}  task {}", short_id, status, planned, short_task)));
                }
            }
            let content_height = content_area.height.saturating_sub(2);
            app.last_content_lines = lines.len();
            app.last_content_area_height = content_height;
            app.last_content_rendered_rows = 0;
            let cal_block = Block::default()
                .borders(Borders::ALL)
                .title(" Calendrier (récurrences et runs) ")
                .border_style(theme.block_border());
            f.render_widget(Paragraph::new(lines).block(cal_block).wrap(Wrap { trim: true }), content_area);
        }
        Mode::Memory => {
            let mut lines: Vec<Line<'static>> = vec![
                Line::from(""),
                Line::from(Span::styled(
                    " Mémoire court terme (session en cours — perdue si daemon redémarre) ",
                    Style::default().fg(theme.palette().accent).add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
            ];
            for (role, content) in &app.memory_short_term {
                let role_style = match role.as_str() {
                    "user" => Style::default().fg(theme.palette().accent),
                    "assistant" => Style::default().fg(theme.palette().success),
                    _ => Style::default().fg(theme.palette().muted),
                };
                lines.push(Line::from(Span::styled(format!("  [{}] ", role), role_style)));
                for l in content.lines().take(5) {
                    lines.push(Line::from(format!("    {}", l)));
                }
                if content.lines().count() > 5 {
                    lines.push(Line::from(Span::styled("    …", Style::default().fg(theme.palette().muted))));
                }
                lines.push(Line::from(""));
            }
            if app.memory_short_term.is_empty() {
                lines.push(Line::from(Span::styled("  (aucun tour pour cette session)", Style::default().fg(theme.palette().muted))));
                lines.push(Line::from(""));
            }
            let lt_status = if app.memory_long_term_available {
                "Mémoire long terme (persistante)"
            } else {
                "Mémoire long terme (désactivée — compiler daemon avec embeddings ou embeddings-tract)"
            };
            lines.push(Line::from(Span::styled(
                lt_status,
                Style::default().fg(theme.palette().accent).add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
            for (content, created_at, source) in &app.memory_long_term {
                lines.push(Line::from(Span::styled(
                    format!("  [{}] {} — {}", source, &created_at[..created_at.len().min(19)], content.chars().take(80).collect::<String>()),
                    Style::default().fg(theme.palette().fg),
                )));
                if content.chars().count() > 80 {
                    lines.push(Line::from(Span::styled("    …", Style::default().fg(theme.palette().muted))));
                }
                lines.push(Line::from(""));
            }
            if app.memory_long_term.is_empty() && app.memory_long_term_available {
                lines.push(Line::from(Span::styled("  (aucune entrée)", Style::default().fg(theme.palette().muted))));
            }
            let content_height = content_area.height.saturating_sub(2);
            app.last_content_lines = lines.len();
            app.last_content_area_height = content_height;
            app.last_content_rendered_rows = 0;
            let max_scroll = app.max_scroll();
            if app.scroll > max_scroll {
                app.scroll = max_scroll;
            }
            let mem_block = Block::default()
                .borders(Borders::ALL)
                .title(" Mémoire agent (R = actualiser, Tab = onglet) ")
                .border_style(theme.block_border());
            f.render_widget(
                Paragraph::new(lines).block(mem_block).wrap(Wrap { trim: true }).scroll((app.scroll as u16, 0)),
                content_area,
            );
        }
    }

    if let Some(input_rect) = input_area_opt {
        let input_label = " Message (Entrée = envoyer, Maj+Entrée = nouvelle ligne, ↑↓ = chat, Ctrl+↑↓ = saisie, Tab = onglet) ";
        let input_area_width = input_rect.width.saturating_sub(2) as usize;
        app.input_inner_height = input_rect.height.saturating_sub(2) as usize;
        app.input_wrapped_lines = App::wrapped_line_count(&app.input, input_area_width.max(1));
        let max_input_scroll = app.input_wrapped_lines.saturating_sub(app.input_inner_height);
        if app.input_scroll > max_input_scroll {
            app.input_scroll = max_input_scroll;
        }
        let input_para = Paragraph::new(app.input.as_str())
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(input_label)
                    .border_style(theme.block_border()),
            )
            .wrap(Wrap { trim: true })
            .scroll((app.input_scroll as u16, 0))
            .style(Style::default().fg(theme.palette().fg));
        f.render_widget(input_para, input_rect);
    }
}

fn run_app(
    terminal: &mut Terminal<ratatui::backend::CrosstermBackend<Stdout>>,
    app: &mut App,
    rx: &mpsc::Receiver<Result<(String, String, Option<String>), String>>,
) -> anyhow::Result<()> {
    let mut last_health = std::time::Instant::now();
    loop {
        if last_health.elapsed() > Duration::from_secs(5) {
            app.check_health();
            last_health = std::time::Instant::now();
        }
        if app.mode == Mode::Router && app.metrics.is_empty() {
            app.fetch_metrics();
        }
        while let Ok(result) = rx.try_recv() {
            app.loading = false;
            match result {
                Ok((text, session_id, pending_task_id)) => {
                    let role = if session_id.is_empty() { "Système" } else { "Akasha" };
                    if !session_id.is_empty() {
                        app.session_id = Some(session_id);
                    }
                    app.pending_reply_task_id = pending_task_id;
                    app.messages.push(ChatMessage {
                        role: role.into(),
                        text,
                        is_error: false,
                    });
                }
                Err(e) => {
                    app.pending_reply_task_id = None;
                    app.messages.push(ChatMessage {
                        role: "Erreur".into(),
                        text: e,
                        is_error: true,
                    });
                }
            }
            app.scroll = usize::MAX;
        }

        terminal.draw(|f| ui(f, app))?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match (app.mode, key.code, key.modifiers) {
                    (_, KeyCode::Esc, _) | (_, KeyCode::Char('q'), KeyModifiers::CONTROL) => return Ok(()),
                    (_, KeyCode::Tab, _) => {
                        app.mode = match app.mode {
                            Mode::Chat => Mode::Router,
                            Mode::Router => Mode::Doc,
                            Mode::Doc => Mode::Tasks,
                            Mode::Tasks => Mode::Calendar,
                            Mode::Calendar => Mode::Memory,
                            Mode::Memory => Mode::Chat,
                        };
                        if app.mode == Mode::Router {
                            app.fetch_metrics();
                        }
                        if app.mode == Mode::Doc && app.doc_content.is_empty() {
                            app.fetch_doc();
                        }
                        if app.mode == Mode::Tasks {
                            app.fetch_activity_tasks();
                        }
                        if app.mode == Mode::Calendar {
                            app.fetch_calendar();
                        }
                        if app.mode == Mode::Memory {
                            app.fetch_memory();
                        }
                    }
                    (Mode::Chat, KeyCode::Enter, mods) => {
                        if mods.contains(KeyModifiers::SHIFT) {
                            app.input.push('\n');
                        } else {
                            let msg = app.input.trim_end().to_string();
                            if msg.is_empty() {
                                continue;
                            }
                            app.messages.push(ChatMessage {
                                role: "Vous".into(),
                                text: msg.clone(),
                                is_error: false,
                            });
                            app.input.clear();
                            app.input_scroll = 0;
                        if msg.starts_with('/') {
                            let cmd_lower = msg.trim().to_lowercase();
                            if cmd_lower == "/newsession" || cmd_lower == "/nouvelle session" {
                                app.session_id = None;
                                app.force_new_session = true;
                                app.messages.push(ChatMessage {
                                    role: "Système".into(),
                                    text: "Nouvelle session demandée. Votre prochain message repartira de zéro (contexte court terme effacé).".into(),
                                    is_error: false,
                                });
                                app.scroll = usize::MAX;
                            } else {
                                let long_running = cmd_lower.starts_with("/advice") || cmd_lower.starts_with("/doctor");
                                if long_running {
                                    app.loading = true;
                                    let port = app.port;
                                    let tx = app.tx.clone();
                                    let cmd = msg.clone();
                                    thread::spawn(move || {
                                        let result = App::run_slash_command_blocking(port, &cmd);
                                        let _ = tx.send(Ok((result, String::new(), None)));
                                    });
                                } else {
                                    let result = App::run_slash_command_blocking(app.port, &msg);
                                    app.messages.push(ChatMessage {
                                        role: "Système".into(),
                                        text: result,
                                        is_error: false,
                                    });
                                    app.scroll = usize::MAX;
                                    let _ = terminal.draw(|f| ui(f, app));
                                }
                            }
                        } else if !app.loading && app.daemon_ok {
                            app.loading = true;
                            let port = app.port;
                            let tx = app.tx.clone();
                            let session_id = app.session_id.clone();
                            let new_session = app.force_new_session;
                            app.force_new_session = false;
                            thread::spawn(move || {
                                App::send_message_non_blocking(tx, msg, port, session_id, new_session);
                            });
                        }
                        }
                    }
                    (Mode::Chat, KeyCode::Up, KeyModifiers::CONTROL) => {
                        app.input_scroll_up();
                    }
                    (Mode::Chat, KeyCode::Down, KeyModifiers::CONTROL) => {
                        app.input_scroll_down();
                    }
                    (Mode::Chat, KeyCode::Up, _) => {
                        app.scroll_up();
                    }
                    (Mode::Chat, KeyCode::Down, _) => {
                        app.scroll_down();
                    }
                    (Mode::Chat, KeyCode::PageUp, _) => {
                        app.scroll_page_up();
                    }
                    (Mode::Chat, KeyCode::PageDown, _) => {
                        app.scroll_page_down();
                    }
                    (Mode::Chat, KeyCode::Home, _) => {
                        app.scroll = 0;
                    }
                    (Mode::Chat, KeyCode::End, _) => {
                        app.scroll_to_bottom();
                    }
                    (Mode::Chat, KeyCode::Backspace, _) => {
                        app.input.pop();
                    }
                    (Mode::Chat, KeyCode::Char(c), _) => {
                        app.input.push(c);
                    }
                    (Mode::Router, KeyCode::Char('r') | KeyCode::Char('R'), _) => {
                        app.fetch_metrics();
                    }
                    (Mode::Doc, KeyCode::Up, _) => app.scroll_up(),
                    (Mode::Doc, KeyCode::Down, _) => app.scroll_down(),
                    (Mode::Doc, KeyCode::PageUp, _) => app.scroll_page_up(),
                    (Mode::Doc, KeyCode::PageDown, _) => app.scroll_page_down(),
                    (Mode::Doc, KeyCode::Home, _) => app.scroll = 0,
                    (Mode::Doc, KeyCode::End, _) => app.scroll_to_bottom(),
                    (Mode::Memory, KeyCode::Up, _) => app.scroll_up(),
                    (Mode::Memory, KeyCode::Down, _) => app.scroll_down(),
                    (Mode::Memory, KeyCode::PageUp, _) => app.scroll_page_up(),
                    (Mode::Memory, KeyCode::PageDown, _) => app.scroll_page_down(),
                    (Mode::Memory, KeyCode::Home, _) => app.scroll = 0,
                    (Mode::Memory, KeyCode::End, _) => app.scroll_to_bottom(),
                    (Mode::Tasks, KeyCode::Char('r') | KeyCode::Char('R'), _) => {
                        app.fetch_activity_tasks();
                    }
                    (Mode::Tasks, KeyCode::Up, _) => {
                        if app.activity_selected > 0 {
                            app.activity_selected -= 1;
                            app.activity_detail_scroll = 0;
                            app.fetch_activity_events_for_selected();
                            app.fetch_activity_task_detail();
                        }
                    }
                    (Mode::Tasks, KeyCode::Down, _) => {
                        if app.activity_selected + 1 < app.activity_tasks.len() {
                            app.activity_selected += 1;
                            app.activity_detail_scroll = 0;
                            app.fetch_activity_events_for_selected();
                            app.fetch_activity_task_detail();
                        }
                    }
                    (Mode::Tasks, KeyCode::PageUp, _) => {
                        app.activity_detail_scroll = app.activity_detail_scroll.saturating_sub(1);
                    }
                    (Mode::Tasks, KeyCode::PageDown, _) => {
                        app.activity_detail_scroll = app.activity_detail_scroll.saturating_add(1);
                    }
                    (Mode::Tasks, KeyCode::Home, _) => {
                        app.activity_detail_scroll = 0;
                    }
                    (Mode::Tasks, KeyCode::End, _) => {
                        app.activity_detail_scroll = usize::MAX;
                    }
                    (Mode::Calendar, KeyCode::Char('r') | KeyCode::Char('R'), _) => {
                        app.fetch_calendar();
                    }
                    (Mode::Memory, KeyCode::Char('r') | KeyCode::Char('R'), _) => {
                        app.fetch_memory();
                    }
                    (Mode::Doc, KeyCode::Char('r') | KeyCode::Char('R'), _) => {
                        app.doc_content.clear();
                        if app.daemon_ok {
                            app.fetch_doc();
                        }
                    }
                    (_, KeyCode::F(2), _) => {
                        app.theme = app.theme.next();
                    }
                    _ => {}
                }
            }
        }
    }
}

fn main() -> anyhow::Result<()> {
    let port = std::env::var("AKASHA_PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(DAEMON_PORT);

    let (tx, rx) = mpsc::channel::<Result<(String, String, Option<String>), String>>();
    let mut app = App::new(port, tx);

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    app.check_health();
    let result = run_app(&mut terminal, &mut app, &rx);

    disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}
