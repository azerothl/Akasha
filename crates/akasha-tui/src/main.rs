//! Akasha TUI — Interface terminal (ratatui + crossterm).
//! Onglets Chat et Routeur (métriques), statut daemon, envoi de messages avec polling tâche.
//! Thèmes (cycle F2) et rendu markdown (tables, listes, blocs de code).

mod i18n;
mod markdown;
mod theme;

use crossterm::{
    event::{
        self, EnableMouseCapture, DisableMouseCapture, Event, KeyCode, KeyEventKind,
        KeyModifiers, MouseButton, MouseEventKind,
    },
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, Tabs, Wrap},
};
use i18n::I18n;
use theme::{Theme, ThemeName};
use std::io::{self, Stdout};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const DAEMON_PORT: u16 = 3876;
const TASK_POLL_INTERVAL_MS: u64 = 1500;
const TASK_POLL_TIMEOUT_SECS: u64 = 600;
const TUI_THEME_FILENAME: &str = "tui_theme.txt";

fn akasha_data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("AKASHA_DATA_DIR") {
        return PathBuf::from(dir);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("akasha")
}

fn load_theme_from_disk() -> Option<ThemeName> {
    let path = akasha_data_dir().join(TUI_THEME_FILENAME);
    let s = std::fs::read_to_string(&path).ok()?;
    let s = s.trim();
    ThemeName::parse(s)
}

fn save_theme_to_disk(theme: ThemeName) {
    let dir = akasha_data_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(TUI_THEME_FILENAME);
    let _ = std::fs::write(path, theme.to_saved_str());
}

fn daemon_base_url(port: u16) -> String {
    format!("http://127.0.0.1:{}", port)
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Chat,
    ScheduleReports,
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
    parent_task_id: Option<String>,
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

/// One task run in the calendar (from GET /api/task_runs).
#[derive(Clone)]
struct CalendarTaskRun {
    id: String,
    status: String,
    planned_for: String,
    task_id: String,
    schedule_id: Option<String>,
    started_at: Option<String>,
    ended_at: Option<String>,
}

/// Full detail for selected calendar run (run metadata + GET /api/tasks/:id).
#[derive(Clone, Default)]
struct CalendarRunDetail {
    run_status: String,
    _run_started_at: Option<String>,
    _run_ended_at: Option<String>,
    task_status: String,
    _created_at: String,
    _updated_at: String,
    progress: Vec<(u8, String)>,
    _schedule_id: Option<String>,
}

/// Detail for selected schedule (from GET /api/schedules/:id).
#[derive(Clone, Default)]
struct ScheduleDetail {
    id: String,
    name: String,
    description: String,
    channel_context: Option<String>,
    enabled: bool,
    interval_seconds: Option<u64>,
    _timezone: Option<String>,
    _rrule: Option<String>,
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
    i18n: I18n,
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
    calendar_task_runs: Vec<CalendarTaskRun>,
    #[allow(dead_code)]
    calendar_scroll: usize,
    /// Pending task id after ack (show "En cours: Task #xxx" in chat).
    pending_reply_task_id: Option<String>,
    /// Progress % for the pending task (from background poll).
    pending_reply_pct: Option<u8>,
    /// Schedule run reports to show in chat (Rappel « X » exécuté : …).
    schedule_reports: Vec<(String, String)>,
    /// Calendar: focus on schedules (true) or runs (false). Tab toggles.
    calendar_focus_schedules: bool,
    /// Calendar: selected schedule index (when focus on schedules).
    calendar_schedule_index: usize,
    /// Calendar: selected task_run index (when focus on runs).
    calendar_selected_run: Option<usize>,
    /// Calendar: detail for selected run (run + task status, progress, parent).
    calendar_run_detail: Option<CalendarRunDetail>,
    /// Calendar: detail for selected schedule (id, name, description, channel_context).
    calendar_schedule_detail: Option<ScheduleDetail>,
    /// Tasks tab: scroll offset for the task list (so selection stays visible).
    activity_list_scroll: usize,
    /// Tasks tab: if true, task list body is collapsed (only header visible; Space/Enter to expand).
    activity_list_collapsed: bool,
    /// Tasks tab: if true, list shows only root tasks (one row per discussion). Toggle with 'd' for "discussions".
    activity_show_roots_only: bool,
    /// Indices into activity_tasks for the visible list (roots only or all). Updated on fetch and when toggling activity_show_roots_only.
    activity_visible_indices: Vec<usize>,
    /// Tasks tab: list widget rect (for mouse click to select task).
    activity_list_rect: Option<ratatui::prelude::Rect>,
    /// Tabs bar rect (for mouse click to switch tab).
    tabs_rect: Option<ratatui::prelude::Rect>,
    /// Calendar tab: content area rect (for mouse click to select schedule or run).
    calendar_content_rect: Option<ratatui::prelude::Rect>,
    /// Session id for short-term memory (returned by daemon, send back on next message).
    session_id: Option<String>,
    /// If true, next message will request a new session (context reset).
    force_new_session: bool,
    /// Memory tab: short-term turns (role, content) for current session.
    memory_short_term: Vec<(String, String)>,
    /// Memory tab: long-term entries (id, content, created_at, source).
    memory_long_term: Vec<(String, String, String, String)>,
    /// Relations per entry id: entry_id -> [(to_id, kind)].
    memory_lt_related: std::collections::HashMap<String, Vec<(String, String)>>,
    /// Selected index in long-term list (for delete).
    memory_lt_selected: usize,
    /// Line index in Memory tab where each long-term entry starts (set during render).
    memory_lt_line_starts: Vec<usize>,
    /// Whether long-term memory is available (daemon has embeddings).
    memory_long_term_available: bool,
    /// Memory search: query being typed.
    memory_search_query: String,
    /// Memory search: results (id, content) from last search.
    memory_search_results: Vec<(String, String)>,
    /// If true, show search bar and results instead of long-term list.
    memory_search_active: bool,
    /// If true, show graph view (tree from selected entry) instead of flat list.
    memory_view_graph: bool,
    /// Current theme (cycle with F2).
    theme: ThemeName,
    port: u16,
    /// (content, session_id, pending_task_id). When pending_task_id is Some, reply will follow later.
    tx: mpsc::Sender<Result<(String, String, Option<String>), String>>,
    /// Optional channel to send (task_id, progress_pct) for running task (TUI progress %).
    progress_tx: Option<mpsc::Sender<(String, u8)>>,
    /// Scroll offset for input area when text wraps to more lines than visible (Ctrl+↑/↓).
    input_scroll: usize,
    /// Set by ui(): inner height of input area for clamping input_scroll.
    input_inner_height: usize,
    /// Set by ui(): wrapped line count of input text.
    input_wrapped_lines: usize,
    /// If true, we have already loaded today's chat history from short-term memory (so we don't refetch every frame).
    chat_history_loaded: bool,
    /// Human in the loop: when the agent asked for user input, (task_id, question, context, choices).
    pending_human_input: Option<(String, String, String, Option<Vec<String>>)>,
    /// All tasks currently waiting for user input (from GET /api/pending-human-input), so we can show them after relaunch or when user was away.
    pending_human_input_list: Vec<(String, String, String, Option<Vec<String>>)>,
    /// Operator snapshot (schedules, task_runs, process watch, terminal, tools, recall, MCP, lifecycle hooks).
    operator_ops_text: String,
    /// Vertical scroll for the operator snapshot block on the Router tab.
    operator_ops_scroll: usize,
}

fn trim_tui(s: &str, max: usize) -> String {
    let compact = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= max {
        compact
    } else {
        let mut out = String::new();
        for (i, ch) in compact.chars().enumerate() {
            if i >= max.saturating_sub(1) {
                out.push('…');
                break;
            }
            out.push(ch);
        }
        out
    }
}

fn payload_as_f64(v: &serde_json::Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_i64().map(|n| n as f64)).or_else(|| v.as_u64().map(|n| n as f64))
}

fn summarize_task_event_payload_tui(payload: &serde_json::Value) -> String {
    let obj = match payload.as_object() {
        Some(o) => o,
        None => return trim_tui(&serde_json::to_string(payload).unwrap_or_default(), 180),
    };

    if let Some(view) = obj.get("view").and_then(|v| v.as_str()) {
        let view_l = view.to_lowercase();
        if view_l == "map" {
            let distance = obj
                .get("distance_m")
                .and_then(payload_as_f64)
                .map(|m| if m < 1000.0 { format!("{} m", m.round() as i64) } else { format!("{:.2} km", m / 1000.0) });
            let duration = obj
                .get("duration_s")
                .and_then(payload_as_f64)
                .map(|s| {
                    if s < 60.0 {
                        format!("{} s", s.round() as i64)
                    } else {
                        let min = (s / 60.0).floor() as i64;
                        let sec = (s % 60.0).round() as i64;
                        if sec > 0 { format!("{} min {} s", min, sec) } else { format!("{} min", min) }
                    }
                });
            let mut parts = vec!["view=map".to_string()];
            if let Some(d) = distance { parts.push(format!("distance={}", d)); }
            if let Some(d) = duration { parts.push(format!("duration={}", d)); }
            return parts.join(" | ");
        }
        if view_l == "graph" || view_l == "timeseries" {
            let series_count = obj
                .get("series")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .or_else(|| {
                    obj.get("figure")
                        .and_then(|f| f.get("data"))
                        .and_then(|d| d.as_array())
                        .map(|a| a.len())
                })
                .unwrap_or(0);
            return format!("view={} | series={}", view_l, series_count);
        }
    }

    let mut parts: Vec<String> = Vec::new();
    if let Some(model) = obj.get("model").and_then(|v| v.as_str()) {
        parts.push(format!("model={}", model));
    }
    if let Some(reason) = obj.get("done_reason").and_then(|v| v.as_str()) {
        parts.push(format!("done_reason={}", reason));
    }
    if let Some(eval_count) = obj.get("eval_count").and_then(|v| v.as_u64()) {
        parts.push(format!("eval_count={}", eval_count));
    }
    if let Some(resp) = obj.get("response").and_then(|v| v.as_str()) {
        if !resp.trim().is_empty() {
            parts.push(format!("response=\"{}\"", trim_tui(resp, 120)));
        }
    }
    if let Some(thinking) = obj.get("thinking").and_then(|v| v.as_str()) {
        if !thinking.trim().is_empty() {
            parts.push(format!("thinking=\"{}\"", trim_tui(thinking, 120)));
        }
    }

    if parts.is_empty() {
        trim_tui(&serde_json::to_string(payload).unwrap_or_default(), 180)
    } else {
        parts.join(" | ")
    }
}

impl App {
    fn new(port: u16, tx: mpsc::Sender<Result<(String, String, Option<String>), String>>, progress_tx: Option<mpsc::Sender<(String, u8)>>) -> Self {
        Self {
            i18n: i18n::load(i18n::detect_locale()),
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
            pending_reply_pct: None,
            schedule_reports: Vec::new(),
            calendar_focus_schedules: false,
            calendar_schedule_index: 0,
            calendar_selected_run: None,
            calendar_run_detail: None,
            calendar_schedule_detail: None,
            activity_list_scroll: 0,
            activity_list_collapsed: false,
            activity_show_roots_only: true,
            activity_visible_indices: Vec::new(),
            activity_list_rect: None,
            tabs_rect: None,
            calendar_content_rect: None,
            session_id: None,
            force_new_session: false,
            memory_short_term: Vec::new(),
            memory_long_term: Vec::new(),
            memory_lt_related: std::collections::HashMap::new(),
            memory_lt_selected: 0,
            memory_lt_line_starts: Vec::new(),
            memory_long_term_available: false,
            memory_search_query: String::new(),
            memory_search_results: Vec::new(),
            memory_search_active: false,
            memory_view_graph: false,
            theme: ThemeName::default(),
            port,
            tx,
            progress_tx,
            input_scroll: 0,
            input_inner_height: 3,
            input_wrapped_lines: 0,
            chat_history_loaded: false,
            pending_human_input: None,
            pending_human_input_list: Vec::new(),
            operator_ops_text: String::new(),
            operator_ops_scroll: 0,
        }
    }

    /// Fetch all pending human-input (GET /api/pending-human-input). Updates pending_human_input_list and, if needed, pending_human_input so the user sees questions after relaunch or when they were away.
    fn fetch_pending_human_input_list(&mut self) {
        let url = format!("{}/api/pending-human-input", daemon_base_url(self.port));
        let client = match reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
        {
            Ok(c) => c,
            Err(_) => return,
        };
        let resp = match client.get(&url).send() {
            Ok(r) => r,
            Err(_) => return,
        };
        if !resp.status().is_success() {
            self.pending_human_input_list.clear();
            return;
        }
        let json: serde_json::Value = match resp.json() {
            Ok(j) => j,
            Err(_) => return,
        };
        let pending = match json.get("pending").and_then(|p| p.as_array()) {
            Some(a) => a,
            None => {
                self.pending_human_input_list.clear();
                return;
            }
        };
        let mut list = Vec::new();
        for p in pending {
            let task_id = p.get("task_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let question = p.get("question").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if task_id.is_empty() || question.is_empty() {
                continue;
            }
            let context = p.get("context").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let choices = p.get("choices")
                .and_then(|c| c.as_array())
                .map(|arr| arr.iter().filter_map(|x| x.as_str().map(String::from)).collect());
            list.push((task_id, question, context, choices));
        }
        self.pending_human_input_list = list;
        if let Some(first) = self.pending_human_input_list.first() {
            let current_id = self.pending_human_input.as_ref().map(|t| t.0.as_str());
            if current_id.is_none() || !self.pending_human_input_list.iter().any(|t| Some(t.0.as_str()) == current_id) {
                self.pending_human_input = Some(first.clone());
            }
        } else {
            self.pending_human_input = None;
        }
    }

    /// Fetch pending human-input for a task (GET /api/tasks/:id/human-input). Sets pending_human_input if the agent is waiting.
    fn fetch_pending_human_input(&mut self, task_id: &str) {
        let url = format!("{}/api/tasks/{}/human-input", daemon_base_url(self.port), task_id);
        let client = match reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
        {
            Ok(c) => c,
            Err(_) => return,
        };
        let resp = match client.get(&url).send() {
            Ok(r) => r,
            Err(_) => return,
        };
        if !resp.status().is_success() {
            self.pending_human_input = None;
            return;
        }
        let json: serde_json::Value = match resp.json() {
            Ok(j) => j,
            Err(_) => return,
        };
        let question = json.get("question").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if question.is_empty() {
            self.pending_human_input = None;
            return;
        }
        let context = json.get("context").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let choices = json.get("choices").and_then(|c| c.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect());
        self.pending_human_input = Some((task_id.to_string(), question, context, choices));
    }

    /// Submit user reply for human-in-the-loop (POST /api/tasks/:id/human-reply).
    fn submit_human_reply(&mut self, task_id: &str, response: &str) -> bool {
        let url = format!("{}/api/tasks/{}/human-reply", daemon_base_url(self.port), task_id);
        let client = match reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
        {
            Ok(c) => c,
            Err(_) => return false,
        };
        let body = serde_json::json!({ "response": response });
        let resp = match client.post(&url).json(&body).send() {
            Ok(r) => r,
            Err(_) => return false,
        };
        if resp.status().is_success() {
            self.pending_human_input_list.retain(|(id, _, _, _)| id != task_id);
            self.pending_human_input = self.pending_human_input_list.first().cloned();
            true
        } else {
            false
        }
    }

    /// Side effects when switching to a mode (fetch data, reset scroll, etc.).
    fn trigger_mode_entered(&mut self) {
        if self.mode == Mode::Router {
            self.fetch_metrics();
            self.fetch_operator_ops_snapshot();
            self.operator_ops_scroll = 0;
        }
        if self.mode == Mode::Doc && self.doc_content.is_empty() {
            self.fetch_doc();
        }
        if self.mode == Mode::Tasks {
            self.fetch_activity_tasks();
        }
        if self.mode == Mode::Chat {
            self.fetch_pending_human_input_list();
        }
        if self.mode == Mode::Calendar {
            self.fetch_calendar();
            self.scroll = 0;
            if !self.calendar_task_runs.is_empty() {
                self.calendar_focus_schedules = false;
                self.calendar_selected_run = Some(0);
                let run = self.calendar_task_runs[0].clone();
                self.fetch_calendar_run_detail(&run);
            } else if !self.calendar_schedules.is_empty() {
                self.calendar_focus_schedules = true;
                self.calendar_schedule_index = 0;
                let id = self.calendar_schedules[0].0.clone();
                self.fetch_schedule_detail(&id);
            }
        }
        if self.mode == Mode::ScheduleReports {
            self.fetch_schedule_reports();
        }
        if self.mode == Mode::Memory {
            self.fetch_memory();
        }
    }

    /// Load today's conversation from short-term memory (GET /api/memory/short-term without session_id = day-YYYY-MM-DD).
    fn fetch_chat_history_from_memory(&mut self) {
        self.chat_history_loaded = true;
        let url = format!("{}/api/memory/short-term", daemon_base_url(self.port));
        let client = match reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
        {
            Ok(c) => c,
            Err(_) => return,
        };
        let resp = match client.get(&url).send() {
            Ok(r) => r,
            Err(_) => return,
        };
        if !resp.status().is_success() {
            return;
        }
        let json: serde_json::Value = match resp.json() {
            Ok(j) => j,
            Err(_) => return,
        };
        let session_id = json.get("session_id").and_then(|v| v.as_str()).map(String::from);
        let turns = json.get("turns").and_then(|t| t.as_array()).cloned().unwrap_or_default();
        if let Some(ref sid) = session_id {
            self.session_id = Some(sid.clone());
        }
        if turns.is_empty() {
            return;
        }
        self.messages = turns
            .iter()
            .filter_map(|t| {
                let role = t.get("role")?.as_str()?;
                let content = t.get("content")?.as_str()?.to_string();
                let role_label = match role {
                    "user" => "user",
                    "assistant" => "assistant",
                    _ => "system",
                };
                Some(ChatMessage {
                    role: role_label.to_string(),
                    text: content,
                    is_error: false,
                })
            })
            .collect();
        self.scroll = usize::MAX;
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
                            let parent_task_id = t.get("parent_task_id").and_then(|v| v.as_str()).map(String::from);
                            Some(ActivityTaskRow { id, status, created_at, assigned_agent, parent_task_id })
                        })
                        .collect();
                    self.activity_visible_indices = if self.activity_show_roots_only {
                        self.activity_tasks.iter().enumerate()
                            .filter(|(_, r)| r.parent_task_id.is_none())
                            .map(|(i, _)| i)
                            .collect()
                    } else {
                        (0..self.activity_tasks.len()).collect()
                    };
                    if self.activity_selected >= self.activity_visible_indices.len() && !self.activity_visible_indices.is_empty() {
                        self.activity_selected = self.activity_visible_indices.len() - 1;
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
        let id = self.activity_visible_indices.get(self.activity_selected)
            .and_then(|&idx| self.activity_tasks.get(idx))
            .map(|row| row.id.clone());
        let id = match id {
            Some(id) => id,
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
                            let key = format!("events.{}", typ);
                            let label = self.i18n.t(&key);
                            let label = if label == key { typ.to_string() } else { label };
                            let at = e.get("at").and_then(|v| v.as_str()).unwrap_or("");
                            let payload = e.get("payload").cloned();
                            let line = if let Some(p) = payload {
                                format!("{} @ {} — {}", label, at, summarize_task_event_payload_tui(&p))
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
        let id = match self.activity_visible_indices.get(self.activity_selected)
            .and_then(|&idx| self.activity_tasks.get(idx))
        {
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

    fn fetch_schedule_reports(&mut self) {
        let base = daemon_base_url(self.port);
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        self.schedule_reports.clear();
        if let Ok(resp) = client.get(format!("{}/api/schedule_run_reports", base)).send() {
            if resp.status().is_success() {
                if let Ok(json) = resp.json::<serde_json::Value>() {
                    if let Some(arr) = json.get("reports").and_then(|a| a.as_array()) {
                        for r in arr {
                            let name = r.get("schedule_name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let msg = r.get("message").and_then(|v| v.as_str()).unwrap_or("Exécuté.").to_string();
                            self.schedule_reports.push((name, msg));
                        }
                    }
                }
            }
        }
    }

    fn fetch_calendar_run_detail(&mut self, run: &CalendarTaskRun) {
        let base = daemon_base_url(self.port);
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        if let Ok(resp) = client.get(format!("{}/api/tasks/{}", base, run.task_id)).send() {
            if resp.status().is_success() {
                if let Ok(json) = resp.json::<serde_json::Value>() {
                    let task_status = json.get("status").and_then(|v| v.as_str()).unwrap_or("?").to_string();
                    let created_at = json.get("created_at").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let updated_at = json.get("updated_at").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let progress: Vec<(u8, String)> = json.get("progress")
                        .and_then(|p| p.as_array())
                        .map(|arr| arr.iter().filter_map(|e| {
                            let pct = e.get("progress_pct").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
                            let msg = e.get("message").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            Some((pct, msg))
                        }).collect())
                        .unwrap_or_default();
                    self.calendar_run_detail = Some(CalendarRunDetail {
                        run_status: run.status.clone(),
                        _run_started_at: run.started_at.clone(),
                        _run_ended_at: run.ended_at.clone(),
                        task_status,
                        _created_at: created_at,
                        _updated_at: updated_at,
                        progress,
                        _schedule_id: run.schedule_id.clone(),
                    });
                    return;
                }
            }
        }
        self.calendar_run_detail = None;
    }

    fn fetch_schedule_detail(&mut self, schedule_id: &str) {
        let base = daemon_base_url(self.port);
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        if let Ok(resp) = client.get(format!("{}/api/schedules/{}", base, schedule_id)).send() {
            if resp.status().is_success() {
                if let Ok(json) = resp.json::<serde_json::Value>() {
                    let id = json.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let name = json.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let description = json.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let channel_context = json.get("channel_context").and_then(|v| v.as_str()).map(String::from);
                    let enabled = json.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
                    let interval_seconds = json.get("interval_seconds").and_then(|v| v.as_u64());
                    let timezone = json.get("timezone").and_then(|v| v.as_str()).map(String::from);
                    let rrule = json.get("rrule").and_then(|v| v.as_str()).map(String::from);
                    self.calendar_schedule_detail = Some(ScheduleDetail {
                        id,
                        name,
                        description,
                        channel_context,
                        enabled,
                        interval_seconds,
                        _timezone: timezone,
                        _rrule: rrule,
                    });
                    return;
                }
            }
        }
        self.calendar_schedule_detail = None;
    }

    fn fetch_calendar(&mut self) {
        let base = daemon_base_url(self.port);
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        self.calendar_schedules.clear();
        self.calendar_task_runs.clear();
        self.calendar_selected_run = None;
        self.calendar_run_detail = None;
        self.calendar_schedule_detail = None;
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
        self.calendar_schedule_index = self.calendar_schedule_index.min(self.calendar_schedules.len().saturating_sub(1));
        if let Ok(resp) = client.get(format!("{}/api/task_runs", base)).send() {
            if resp.status().is_success() {
                if let Ok(json) = resp.json::<serde_json::Value>() {
                    if let Some(arr) = json.get("task_runs").and_then(|a| a.as_array()) {
                        for r in arr.iter().take(50) {
                            let id = r.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let status = r.get("status").and_then(|v| v.as_str()).unwrap_or("?").to_string();
                            let planned_for = r.get("planned_for").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let task_id = r.get("task_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let schedule_id = r.get("schedule_id").and_then(|v| v.as_str()).map(String::from);
                            let started_at = r.get("started_at").and_then(|v| v.as_str()).map(String::from);
                            let ended_at = r.get("ended_at").and_then(|v| v.as_str()).map(String::from);
                            self.calendar_task_runs.push(CalendarTaskRun {
                                id,
                                status,
                                planned_for,
                                task_id,
                                schedule_id,
                                started_at,
                                ended_at,
                            });
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
                    self.memory_lt_related.clear();
                    self.memory_long_term = entries
                        .iter()
                        .filter_map(|e| {
                            let id = e.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let content = e.get("content")?.as_str()?.to_string();
                            let created_at = e.get("created_at")?.as_str()?.to_string();
                            let source = e.get("source")?.as_str()?.to_string();
                            let related: Vec<(String, String)> = e
                                .get("related")
                                .and_then(|r| r.as_array())
                                .map(|arr| {
                                    arr.iter()
                                        .filter_map(|r| {
                                            let to_id = r.get("id")?.as_str()?.to_string();
                                            let kind = r.get("kind").and_then(|k| k.as_str()).unwrap_or("").to_string();
                                            Some((to_id, kind))
                                        })
                                        .collect()
                                })
                                .unwrap_or_default();
                            if !related.is_empty() {
                                self.memory_lt_related.insert(id.clone(), related);
                            }
                            Some((id, content, created_at, source))
                        })
                        .collect();
                    self.memory_lt_selected = self.memory_lt_selected.min(self.memory_long_term.len().saturating_sub(1));
                }
            } else {
                self.memory_long_term.clear();
                self.memory_lt_related.clear();
                self.memory_long_term_available = false;
            }
        } else {
            self.memory_long_term.clear();
            self.memory_lt_related.clear();
            self.memory_long_term_available = false;
        }
    }

    /// Delete the long-term memory entry at the current selection (by id). Refreshes the list on success.
    fn delete_memory_long_term_selected(&mut self) {
        let id = match self.memory_long_term.get(self.memory_lt_selected) {
            Some((id, _, _, _)) if !id.is_empty() => id.clone(),
            _ => return,
        };
        let url = format!("{}/api/memory/long-term/{}", daemon_base_url(self.port), id);
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        if client.delete(&url).send().map(|r| r.status().is_success()).unwrap_or(false) {
            self.fetch_memory();
        }
    }

    /// Run semantic search in long-term memory (GET /api/memory/search). Fills memory_search_results.
    fn fetch_memory_search(&mut self) {
        let query = self.memory_search_query.trim();
        if query.is_empty() {
            self.memory_search_results = Vec::new();
            return;
        }
        let url = format!(
            "{}/api/memory/search?q={}&top_k=20",
            daemon_base_url(self.port),
            urlencoding::encode(query)
        );
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        if let Ok(resp) = client.get(&url).send() {
            if resp.status().is_success() {
                if let Ok(json) = resp.json::<serde_json::Value>() {
                    self.memory_search_results = json
                        .get("results")
                        .and_then(|r| r.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|e| {
                                    let id = e.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                    let content = e.get("content").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                    Some((id, content))
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                }
            } else {
                self.memory_search_results.clear();
            }
        } else {
            self.memory_search_results.clear();
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
        self.doc_content = self.i18n.t("tui.doc_unavailable").to_string();
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

    /// Fetch operator HTTP endpoints for display under router metrics (daemon cockpit).
    fn fetch_operator_ops_snapshot(&mut self) {
        let base = daemon_base_url(self.port);
        let client = match reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(8))
            .build()
        {
            Ok(c) => c,
            Err(_) => {
                self.operator_ops_text = "(client HTTP)".to_string();
                return;
            }
        };
        let paths: [(&str, &str); 9] = [
            ("schedules", "/api/schedules"),
            ("task_runs", "/api/task_runs"),
            ("process_watch", "/api/process/watch/recent?limit=12"),
            ("terminal", "/api/terminal/capabilities"),
            ("tools", "/api/tools/effective"),
            ("recall", "/api/memory/recall-metrics"),
            ("mcp", "/api/mcp/status"),
            ("mcp_runtime", "/api/mcp/runtime"),
            ("lifecycle", "/api/lifecycle/hooks"),
        ];
        let mut parts: Vec<String> = Vec::new();
        let mut ok = 0usize;
        for (label, path) in paths {
            let url = format!("{base}{path}");
            let line = match client.get(&url).send() {
                Ok(r) => {
                    let status = r.status();
                    if status.is_success() {
                        ok += 1;
                    }
                    let body = r.text().unwrap_or_default();
                    format!("{label} {path} → {status}\n{}", trim_tui(&body, 1400))
                }
                Err(e) => format!("{label} {path} → (error: {e})"),
            };
            parts.push(line);
        }
        let mut out = vec![format!("Cockpit health: {ok}/{} endpoints OK", parts.len())];
        out.extend(parts);
        self.operator_ops_text = out.join("\n---\n");
    }

    /// Non-blocking: POST /api/message, send ack via tx, then poll and send final reply (FR-025).
    /// If progress_tx is Some, sends (task_id, progress_pct) on each poll for TUI progress display.
    fn send_message_non_blocking(
        tx: mpsc::Sender<Result<(String, String, Option<String>), String>>,
        progress_tx: Option<mpsc::Sender<(String, u8)>>,
        message: String,
        port: u16,
        session_id: Option<String>,
        new_session: bool,
        i18n: I18n,
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
        let default_ack = i18n.t("chat.ack_default");
        let ack_msg = json.get("message").and_then(|v| v.as_str()).unwrap_or_else(|| default_ack.as_str());
        let ack_text = if task_id.is_empty() {
            ack_msg.to_string()
        } else {
            let short = if task_id.len() > 8 { &task_id[task_id.len()-8..] } else { &task_id[..] };
            format!("{}\n\n{} Task #{}", ack_msg, i18n.t("chat.follow_tasks"), short)
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
                    if last_message.is_empty() { i18n.t("chat.timeout") } else { last_message },
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
                    if let Some(ref ptx) = progress_tx {
                        let pct = last
                            .get("progress_pct")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as u8;
                        let _ = ptx.send((task_id.clone(), pct));
                    }
                }
            }
            let status = task_json.get("status").and_then(|v| v.as_str()).unwrap_or("");
            if status == "completed" {
                let _ = tx.send(Ok((
                    if last_message.is_empty() { i18n.t("chat.done") } else { last_message },
                    session_id,
                    None,
                )));
                return;
            }
            if status == "failed" {
                let _ = tx.send(Ok((
                    if last_message.is_empty() { i18n.t("chat.task_failed") } else { last_message },
                    session_id,
                    None,
                )));
                return;
            }
            if status == "cancelled" {
                let _ = tx.send(Ok((i18n.t("chat.cancelled"), session_id, None)));
                return;
            }
            if status == "waiting_user_input" {
                let _ = tx.send(Ok((
                    i18n.t("chat.waiting_input"),
                    session_id,
                    None,
                )));
                return;
            }
            if status == "paused" {
                let _ = tx.send(Ok((
                    i18n.t("chat.task_paused"),
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
    fn run_slash_command_blocking(port: u16, input: &str, i18n: &I18n) -> String {
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
  /task create "msg" — créer une tâche (envoie le message au daemon, comme un message chat)
  /schedule create NAME INTERVAL_SEC "description" — créer une récurrence
  /schedule delete SCHEDULE_ID — supprimer une récurrence
  /stop TASK_ID     — annuler une tâche (en cours ou en attente)
  /cancel TASK_ID   — idem que /stop
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
  /skills            — liste des skills installés
  /skills list       — idem
  /skills install <url> — installer un skill depuis une URL (GitHub ou hôte autorisé)
  /skills reload     — recharger les skills (data_dir/skills, spec/skills)
  /skills uninstall <nom> — désinstaller un skill (ex. /skills uninstall bankr)
  /restart          — redémarrer le daemon (superviseur)
  /vault set        — utiliser le CLI : akasha vault set KEY [value]"#.to_string();
            }
            "task" => {
                let sub = parts.get(1).map(|s| s.to_lowercase()).unwrap_or_default();
                if sub == "create" {
                    let msg = if parts.len() > 2 {
                        parts[2..].join(" ").trim_matches('"').to_string()
                    } else {
                        parts.get(2).map(|s| s.trim_matches('"').to_string()).unwrap_or_default()
                    };
                    if msg.is_empty() {
                        return "Usage: /task create \"votre message\"".to_string();
                    }
                    let url = format!("{}/api/message", base);
                    let body = serde_json::json!({ "message": msg });
                    match client.post(&url).json(&body).timeout(Duration::from_secs(30)).send() {
                        Ok(r) if r.status().is_success() => {
                            if let Ok(json) = r.json::<serde_json::Value>() {
                                let task_id = json.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
                                let ack = json.get("message").and_then(|v| v.as_str()).unwrap_or("Tâche créée.");
                                return format!("{} Task #{}", ack, if task_id.len() > 8 { &task_id[task_id.len()-8..] } else { task_id });
                            }
                            return "Tâche créée.".to_string();
                        }
                        Ok(r) => return format!("Erreur : {}", r.status()),
                        Err(e) => return format!("Erreur : {}", e),
                    }
                }
                return "Usage: /task create \"message\"".to_string();
            }
            "schedule" => {
                let sub = parts.get(1).map(|s| s.to_lowercase()).unwrap_or_default();
                if sub == "create" {
                    let name = parts.get(2).map(|s| s.to_string()).unwrap_or_default();
                    let interval_s = parts.get(3).and_then(|s| s.parse::<u64>().ok());
                    let description = if parts.len() > 4 {
                        parts[4..].join(" ").trim_matches('"').to_string()
                    } else {
                        parts.get(4).map(|s| s.trim_matches('"').to_string()).unwrap_or_default()
                    };
                    if name.is_empty() {
                        return "Usage: /schedule create NOM INTERVAL_SEC \"description\"".to_string();
                    }
                    let interval_seconds = interval_s.unwrap_or(3600);
                    let now = chrono::Utc::now();
                    let url = format!("{}/api/schedules", base);
                    let body = serde_json::json!({
                        "name": name,
                        "description": description,
                        "enabled": true,
                        "timezone": "UTC",
                        "rrule": "",
                        "interval_seconds": interval_seconds,
                        "start_at": now.to_rfc3339(),
                        "channel_context": description
                    });
                    match client.post(&url).json(&body).send() {
                        Ok(r) if r.status().is_success() => {
                            if let Ok(json) = r.json::<serde_json::Value>() {
                                let id = json.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                                return format!("Récurrence créée : {} (id: {})", name, if id.len() > 8 { &id[id.len()-8..] } else { id });
                            }
                            return format!("Récurrence {} créée.", name);
                        }
                        Ok(r) => return format!("Erreur : {}", r.status()),
                        Err(e) => return format!("Erreur : {}", e),
                    }
                }
                if sub == "delete" {
                    let id = parts.get(2).map(|s| s.trim()).filter(|s| !s.is_empty());
                    match id {
                        Some(sid) => {
                            let url = format!("{}/api/schedules/{}", base, sid);
                            match client.delete(&url).send() {
                                Ok(r) if r.status().is_success() => return format!("Récurrence {} supprimée.", sid),
                                Ok(r) => return format!("Erreur : {}", r.status()),
                                Err(e) => return format!("Erreur : {}", e),
                            }
                        }
                        None => return "Usage: /schedule delete SCHEDULE_ID".to_string(),
                    }
                }
                return "Usage: /schedule create NOM INTERVAL \"desc\" | /schedule delete ID".to_string();
            }
            "stop" | "cancel" => {
                let task_id = parts.get(1).map(|s| s.trim()).filter(|s| !s.is_empty());
                match task_id {
                    Some(id) => {
                        let url = format!("{}/api/tasks/{}/cancel", base, id);
                        match client.post(&url).send() {
                            Ok(r) if r.status().is_success() => return format!("Tâche {} annulée.", id),
                            Ok(r) => {
                                let status = r.status();
                                if let Ok(json) = r.json::<serde_json::Value>() {
                                    let detail = json.get("detail").and_then(|v| v.as_str()).unwrap_or_else(|| json.get("error").and_then(|v| v.as_str()).unwrap_or("Erreur"));
                                    return format!("Erreur : {}", detail);
                                }
                                return format!("Erreur : {}", status);
                            }
                            Err(e) => return format!("Erreur : {}", e),
                        }
                    }
                    None => return "Usage: /stop TASK_ID ou /cancel TASK_ID".to_string(),
                }
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
                            let mut out = format!("{}\n", i18n.t("doctor.title"));
                            for c in checks {
                                let id = c.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                                let ok = c.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                                let desc = c.get("description").and_then(|v| v.as_str()).unwrap_or("");
                                out.push_str(&format!("  [{}] {} — {}\n", if ok { "OK" } else { "KO" }, id, desc));
                            }
                            let all_ok = json.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                            let status_msg = if all_ok { i18n.t("doctor.all_ok") } else { i18n.t("doctor.some_failed") };
                            out.push_str(&status_msg);
                            return out;
                        }
                    }
                    _ => {}
                }
                return i18n.t("doctor.unavailable");
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
            "skills" => {
                let sub = parts.get(1).map(|s| s.to_lowercase()).unwrap_or_default();
                if sub.is_empty() || sub == "list" {
                    let url = format!("{}/api/skills", base);
                    match client.get(&url).send() {
                        Ok(r) if r.status().is_success() => {
                            if let Ok(list) = r.json::<Vec<serde_json::Value>>() {
                                if list.is_empty() {
                                    return "Aucun skill installé. Utilisez /skills reload après en avoir ajouté dans data_dir/skills ou spec/skills.".to_string();
                                }
                                let mut out = String::new();
                                for s in &list {
                                    let name = s.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                                    let desc = s.get("description").and_then(|v| v.as_str()).unwrap_or("").trim();
                                    let desc = if desc.is_empty() { "(sans description)" } else { desc };
                                    out.push_str(&format!("  • {} — {}\n", name, desc));
                                }
                                return out;
                            }
                        }
                        _ => {}
                    }
                    return "Impossible de lister les skills (daemon ou erreur).".to_string();
                }
                if sub == "reload" {
                    let url = format!("{}/api/skills/reload", base);
                    match client.post(&url).send() {
                        Ok(r) if r.status().is_success() => {
                            if let Ok(json) = r.json::<serde_json::Value>() {
                                let count = json.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
                                return format!("Skills rechargés ({} skill(s)).", count);
                            }
                            return "Skills rechargés.".to_string();
                        }
                        Ok(r) => return format!("Erreur: {}", r.status()),
                        Err(e) => return format!("Erreur: {}", e),
                    }
                }
                if sub == "install" {
                    let skill_url = parts.get(2).map(|s| s.trim()).unwrap_or("");
                    if skill_url.is_empty() {
                        return "Usage: /skills install <url> (ex. /skills install https://github.com/BankrBot/skills/tree/main/bankr)".to_string();
                    }
                    let url = format!("{}/api/skills/install", base);
                    let body = serde_json::json!({ "url": skill_url });
                    match client.post(&url).json(&body).send() {
                        Ok(r) if r.status().is_success() => {
                            if let Ok(json) = r.json::<serde_json::Value>() {
                                let msg = json.get("message").and_then(|v| v.as_str()).unwrap_or("Skill installé.");
                                return msg.to_string();
                            }
                            return "Skill installé.".to_string();
                        }
                        Ok(r) => {
                            let status = r.status();
                            let err_body = r.text().unwrap_or_default();
                            return format!("Erreur: {} — {}", status, err_body);
                        }
                        Err(e) => return format!("Erreur: {}", e),
                    }
                }
                if sub == "uninstall" {
                    let name = parts.get(2).map(|s| s.trim()).unwrap_or("");
                    if name.is_empty() {
                        return "Usage: /skills uninstall <nom> (ex. /skills uninstall bankr)".to_string();
                    }
                    let url = format!("{}/api/skills/uninstall", base);
                    let body = serde_json::json!({ "name": name });
                    match client.post(&url).json(&body).send() {
                        Ok(r) if r.status().is_success() => {
                            if let Ok(json) = r.json::<serde_json::Value>() {
                                let msg = json.get("message").and_then(|v| v.as_str()).unwrap_or("Skill désinstallé.");
                                return msg.to_string();
                            }
                            return "Skill désinstallé.".to_string();
                        }
                        Ok(r) => {
                            let status = r.status();
                            let err_body = r.text().unwrap_or_default();
                            return format!("Erreur: {} — {}", status, err_body);
                        }
                        Err(e) => return format!("Erreur: {}", e),
                    }
                }
                return "Usage: /skills [list] — lister les skills ; /skills install <url> — installer ; /skills reload — recharger ; /skills uninstall <nom> — désinstaller.".to_string();
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
            Constraint::Length(4),
            Constraint::Min(0),
        ])
        .split(f.area());

    let (content_area, input_area_opt, human_input_area_opt) = if app.mode == Mode::Chat {
        if app.pending_human_input.is_some() {
            let vert = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(0),
                    Constraint::Length(4),
                    Constraint::Length(3),
                ])
                .split(chunks[1]);
            (vert[0], Some(vert[2]), Some(vert[1]))
        } else {
            let vert = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(0), Constraint::Length(3)])
                .split(chunks[1]);
            (vert[0], Some(vert[1]), None)
        }
    } else {
        (chunks[1], None, None)
    };

    let top_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
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
        "{} — {} — Port {} — {} (F2)",
        app.i18n.t("app.title"),
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
    let titles = vec![
        format!(" {} ", app.i18n.t("tabs.chat")),
        format!(" {} ", app.i18n.t("tabs.scheduled")),
        format!(" {} ", app.i18n.t("tabs.router")),
        format!(" {} ", app.i18n.t("tabs.docs")),
        format!(" {} ", app.i18n.t("tabs.tasks")),
        format!(" {} ", app.i18n.t("tabs.calendar")),
        format!(" {} ", app.i18n.t("tabs.memory")),
    ];
    let tab_index = match app.mode {
        Mode::Chat => 0,
        Mode::ScheduleReports => 1,
        Mode::Router => 2,
        Mode::Doc => 3,
        Mode::Tasks => 4,
        Mode::Calendar => 5,
        Mode::Memory => 6,
    };
    let tabs = Tabs::new(titles.clone())
        .block(Block::default().borders(Borders::BOTTOM).title(format!(" {} ", app.i18n.t("tui.tab_switch_hint"))).border_style(theme.block_border()))
        .select(tab_index)
        .style(theme.tab_inactive())
        .highlight_style(theme.tab_active());
    f.render_widget(tabs, top_chunks[1]);
    app.tabs_rect = Some(top_chunks[1]);

    let help_line = if !app.pending_human_input_list.is_empty() {
        app.i18n.t("tui.help_pending").replacen("{}", &app.pending_human_input_list.len().to_string(), 1)
    } else {
        app.i18n.t("tui.help_footer")
    };
    let help_para = Paragraph::new(help_line)
        .style(Style::default().fg(theme.palette().muted));
    f.render_widget(help_para, top_chunks[2]);

    match app.mode {
        Mode::Chat => {
            let content_width = content_area.width as usize;
            let mut lines: Vec<Line<'static>> = Vec::new();
            for m in &app.messages {
                let role_display = if m.is_error { app.i18n.t("common.error") } else { app.i18n.t(&format!("chat.role_{}", m.role)) };
                let (role_style, _base_style) = if m.role == "user" {
                    (
                        Style::default().fg(theme.palette().accent).add_modifier(Modifier::BOLD),
                        Style::default().fg(theme.palette().fg),
                    )
                } else if m.role == "system" {
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
                    format!("  ─── {} ───", role_display),
                    role_style,
                )));
                if m.role == "system" {
                    for line in m.text.lines() {
                        lines.push(Line::from(Span::styled(
                            format!("  {}", line),
                            Style::default().fg(theme.palette().muted),
                        )));
                    }
                } else {
                    let md_styles = theme.markdown_styles();
                    let marked = markdown::from_str_with_width(&m.text, &md_styles, Some(content_width as u16));
                    lines.extend(marked.to_flat_lines());
                }
                lines.push(Line::from(""));
            }
            if let Some(ref tid) = app.pending_reply_task_id {
                lines.push(Line::from(""));
                let short = if tid.len() > 8 { &tid[tid.len()-8..] } else { tid.as_str() };
                let pct_str = app.pending_reply_pct.map(|p| format!(" {}%", p)).unwrap_or_default();
                lines.push(Line::from(Span::styled(
                    {
                    let s = app.i18n.t("tui.task_in_progress");
                    let s = s.replacen("{}", short, 1);
                    let s = s.replacen("{}", &pct_str, 1);
                    format!("  [ {} ]", s)
                },
                    Style::default().fg(theme.palette().warning).add_modifier(Modifier::ITALIC),
                )));
            }
            if app.loading {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!(" {}", app.i18n.t("chat.thinking")),
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
                        .title(app.i18n.t("tui.chat_block_title"))
                        .border_style(theme.block_border()),
                )
                .wrap(Wrap { trim: true })
                .scroll((app.scroll as u16, 0));
            f.render_widget(chat, content_area);
        }
        Mode::ScheduleReports => {
            let content_width = content_area.width as usize;
            let mut lines: Vec<Line<'static>> = Vec::new();
            let style_muted = Style::default().fg(theme.palette().muted).add_modifier(Modifier::BOLD);
            for (name, msg) in &app.schedule_reports {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(format!("  ─── {} ───", app.i18n.t("tui.schedule_executed")), style_muted)));
                lines.push(Line::from(Span::styled(format!("  « {} »", name), Style::default().fg(theme.palette().muted))));
                let md_styles = theme.markdown_styles();
                let marked = markdown::from_str_with_width(msg, &md_styles, Some(content_width.saturating_sub(2) as u16));
                lines.extend(marked.to_flat_lines());
            }
            if app.schedule_reports.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    app.i18n.t("tui.scheduled_empty"),
                    Style::default().fg(theme.palette().muted),
                )));
            }
            let content_height = content_area.height.saturating_sub(2);
            app.last_content_lines = lines.len();
            app.last_content_area_height = content_height;
            app.last_content_rendered_rows = if content_width > 0 {
                lines
                    .iter()
                    .map(|l| {
                        let w = l.width() as usize;
                        if w == 0 { 1 } else { (w + content_width - 1) / content_width }
                    })
                    .sum()
            } else {
                lines.len()
            };
            let max_scroll = app.max_scroll();
            if app.scroll > max_scroll {
                app.scroll = max_scroll;
            }
            let block = Paragraph::new(lines)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(app.i18n.t("tui.scheduled_block_title"))
                        .border_style(theme.block_border()),
                )
                .wrap(Wrap { trim: true })
                .scroll((app.scroll as u16, 0));
            f.render_widget(block, content_area);
        }
        Mode::Router => {
            let split = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(45), Constraint::Min(4)])
                .split(content_area);
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
                app.i18n.t("tui.router_model"),
                app.i18n.t("tui.router_requests"),
                app.i18n.t("tui.router_success"),
                app.i18n.t("tui.router_failed"),
                app.i18n.t("tui.router_latency"),
                app.i18n.t("tui.router_tokens"),
                app.i18n.t("tui.router_fallbacks"),
            ]).style(Style::default().fg(theme.palette().accent)))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(app.i18n.t("tui.router_block"))
                    .border_style(theme.block_border()),
            );
            f.render_widget(table, split[0]);
            let snap_h = split[1].height.saturating_sub(2).max(1) as usize;
            let snap_lines = app.operator_ops_text.lines().count().max(1);
            let snap_max = snap_lines.saturating_sub(snap_h);
            if app.operator_ops_scroll > snap_max {
                app.operator_ops_scroll = snap_max;
            }
            let snap_para = Paragraph::new(app.operator_ops_text.clone())
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(app.i18n.t("tui.router_operator_title"))
                        .border_style(theme.block_border()),
                )
                .style(Style::default().fg(theme.palette().muted))
                .wrap(Wrap { trim: false })
                .scroll((app.operator_ops_scroll as u16, 0));
            f.render_widget(snap_para, split[1]);
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
                        .title(app.i18n.t("tui.doc_block_title"))
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

            let list_inner_h = list_area.height.saturating_sub(2) as usize;
            let mut list_lines: Vec<Line<'static>> = vec![
                Line::from(""),
                Line::from(Span::styled(
                    app.i18n.t("tui.tasks_block_title"),
                    Style::default().fg(theme.palette().accent),
                )),
                Line::from(Span::styled(
                    app.i18n.t("tui.tasks_list_header"),
                    Style::default().fg(theme.palette().muted),
                )),
            ];
            if app.activity_list_collapsed {
                list_lines.push(Line::from(Span::styled(
                    app.i18n.t("tasks.list_collapsed"),
                    Style::default().fg(theme.palette().muted),
                )));
            } else {
                let visible_len = app.activity_visible_indices.len();
                for (i, &idx) in app.activity_visible_indices.iter().enumerate() {
                    let row = match app.activity_tasks.get(idx) {
                        Some(r) => r,
                        None => continue,
                    };
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
                    let parent_short = row.parent_task_id.as_ref()
                        .map(|p| if p.len() > 8 { format!("…{}", &p[p.len()-8..]) } else { p.clone() })
                        .unwrap_or_else(|| "—".to_string());
                    let style = if i == app.activity_selected {
                        Style::default().fg(theme.palette().accent).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(theme.palette().fg)
                    };
                    list_lines.push(Line::from(Span::styled(
                        format!("  {} {} │ {:9} │ {:6} │ {:8} │ {}", if i == app.activity_selected { "►" } else { " " }, short_date, row.status, row.assigned_agent, parent_short, short_id),
                        style,
                    )));
                }
                if visible_len == 0 {
                    list_lines.push(Line::from(if app.activity_tasks.is_empty() {
                        "  Aucune tâche. Envoyez un message dans Chat ou /task create \"message\"."
                    } else {
                        "  Aucune discussion (racine). Touche 'd' : afficher toutes les tâches."
                    }));
                }
            }
            let list_len = list_lines.len();
            let visible_len = app.activity_visible_indices.len();
            if visible_len > 0 {
                let selected_line = 3 + app.activity_selected.min(visible_len - 1);
                if selected_line >= app.activity_list_scroll + list_inner_h && list_inner_h > 0 {
                    app.activity_list_scroll = selected_line - list_inner_h + 1;
                }
                if selected_line < app.activity_list_scroll {
                    app.activity_list_scroll = selected_line;
                }
            }
            let max_scroll = list_len.saturating_sub(list_inner_h).max(0);
            app.activity_list_scroll = app.activity_list_scroll.min(max_scroll);
            let list_block = Block::default()
                .borders(Borders::ALL)
                .title(if app.activity_show_roots_only { app.i18n.t("tui.tasks_discussions") } else { app.i18n.t("tui.tasks_list_all") })
                .border_style(theme.block_border());
            app.activity_list_rect = Some(list_area);
            f.render_widget(
                Paragraph::new(list_lines).block(list_block).scroll((app.activity_list_scroll as u16, 0)),
                list_area,
            );

            let mut detail_lines: Vec<Line<'static>> = vec![Line::from("")];
            if let Some(d) = &app.activity_task_detail {
                detail_lines.push(Line::from(Span::styled(app.i18n.t("tui.tasks_detail_title"), Style::default().fg(theme.palette().accent))));
                detail_lines.push(Line::from(""));
                if let Some(ref msg) = d.user_message {
                    let preview = if msg.len() > 80 { format!("{}…", &msg[..80]) } else { msg.clone() };
                    detail_lines.push(Line::from(Span::styled(app.i18n.t("tui.tasks_request_label"), Style::default().fg(theme.palette().warning))));
                    for line in preview.lines() {
                        detail_lines.push(Line::from(format!("   {}", line)));
                    }
                    detail_lines.push(Line::from(""));
                }
                if let Some(last) = d.progress.last() {
                    let reply_preview = if last.1.chars().count() > 2000 {
                        let truncated: String = last.1.chars().take(2000).collect();
                        format!("{}…", truncated)
                    } else {
                        last.1.clone()
                    };
                    detail_lines.push(Line::from(Span::styled(app.i18n.t("tui.tasks_response_label"), Style::default().fg(theme.palette().success))));
                    let md_styles = theme.markdown_styles();
                    let detail_width = detail_area.width.saturating_sub(4) as u16;
                    let marked = markdown::from_str_with_width(&reply_preview, &md_styles, Some(detail_width));
                    detail_lines.extend(marked.to_flat_lines());
                    detail_lines.push(Line::from(""));
                }
                detail_lines.push(Line::from(format!(
                    "  {} {}  │  {} {}  │  {} {}  │  {} {}",
                    app.i18n.t("tui.created"),
                    d.created_at,
                    app.i18n.t("tui.updated"),
                    d.updated_at,
                    app.i18n.t("tui.status_label"),
                    d.status,
                    app.i18n.t("tui.agent"),
                    d.assigned_agent
                )));
                detail_lines.push(Line::from(""));
                detail_lines.push(Line::from(Span::styled(app.i18n.t("tui.tasks_events"), Style::default().fg(theme.palette().muted))));
            }
            for ev in &app.activity_events {
                detail_lines.push(Line::from(format!("  {}", ev)));
            }
            if app.activity_task_detail.is_none() && !app.activity_visible_indices.is_empty() {
                detail_lines.push(Line::from(app.i18n.t("tasks.select_above")));
            }
            let detail_len = detail_lines.len();
            let detail_inner_height = detail_area.height.saturating_sub(2) as usize; // block borders
            let max_detail_scroll = detail_len.saturating_sub(detail_inner_height);
            app.activity_detail_scroll = app.activity_detail_scroll.min(max_detail_scroll);
            let detail_block = Block::default()
                .borders(Borders::ALL)
                .title(app.i18n.t("tui.tasks_detail_block"))
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
            const CALENDAR_DETAIL_HEIGHT: u16 = 10;
            let cal_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(4),
                    Constraint::Length(CALENDAR_DETAIL_HEIGHT),
                ])
                .split(content_area);
            let list_area = cal_chunks[0];
            let detail_area = cal_chunks[1];

            let mut list_lines: Vec<Line<'static>> = vec![
                Line::from(""),
                Line::from(Span::styled(
                    app.i18n.t("tui.calendar_schedules_title"),
                    Style::default().fg(theme.palette().accent).add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
            ];
            if app.calendar_schedules.is_empty() {
                list_lines.push(Line::from(Span::styled(app.i18n.t("calendar.no_schedule"), Style::default().fg(theme.palette().muted))));
            } else {
                for (i, (id, name, enabled, interval_secs)) in app.calendar_schedules.iter().enumerate() {
                    let short_id = if id.len() > 8 { format!("…{}", &id[id.len()-8..]) } else { id.clone() };
                    let status = if *enabled { app.i18n.t("tui.calendar_enabled") } else { app.i18n.t("tui.calendar_paused") };
                    let interval = interval_secs.map(|s| format!(" — {}s", s)).unwrap_or_default();
                    let sel = app.calendar_focus_schedules && app.calendar_schedule_index == i;
                    let style = if sel { Style::default().fg(theme.palette().accent).add_modifier(Modifier::BOLD) } else { Style::default().fg(theme.palette().fg) };
                    list_lines.push(Line::from(Span::styled(
                        format!("  {} {}  {}  {}", if sel { "►" } else { " " }, short_id, name, format!("{} {}", status, interval)),
                        style,
                    )));
                }
            }
            list_lines.push(Line::from(""));
            list_lines.push(Line::from(Span::styled(
                app.i18n.t("tui.calendar_runs_block"),
                Style::default().fg(theme.palette().accent).add_modifier(Modifier::BOLD),
            )));
            list_lines.push(Line::from(""));
            if app.calendar_task_runs.is_empty() {
                list_lines.push(Line::from(Span::styled(app.i18n.t("calendar.no_run"), Style::default().fg(theme.palette().muted))));
            } else {
                for (i, run) in app.calendar_task_runs.iter().enumerate() {
                    let short_id = if run.id.len() > 8 { &run.id[run.id.len()-8..] } else { run.id.as_str() };
                    let short_task = if run.task_id.len() > 8 { format!("…{}", &run.task_id[run.task_id.len()-8..]) } else { run.task_id.clone() };
                    let planned = if run.planned_for.len() >= 19 { &run.planned_for[..19] } else { run.planned_for.as_str() };
                    let sel = !app.calendar_focus_schedules && app.calendar_selected_run == Some(i);
                    let style = if sel { Style::default().fg(theme.palette().accent).add_modifier(Modifier::BOLD) } else { Style::default().fg(theme.palette().fg) };
                    list_lines.push(Line::from(Span::styled(
                        format!("  {} {}  {}  {}  task {}  {}", if sel { "►" } else { " " }, short_id, run.status, planned, short_task, run.schedule_id.as_ref().map(|s| format!("sched…{}", if s.len() > 8 { &s[s.len()-8..] } else { s })).unwrap_or_default()),
                        style,
                    )));
                }
            }

            let list_height = list_area.height.saturating_sub(2);
            app.last_content_lines = list_lines.len();
            app.last_content_area_height = list_height;
            app.last_content_rendered_rows = 0;
            let max_scroll = app.max_scroll();
            if app.scroll > max_scroll {
                app.scroll = max_scroll;
            }
            let list_block = Block::default()
                .borders(Borders::ALL)
                .title(app.i18n.t("tui.calendar_block_title"))
                .border_style(theme.block_border());
            app.calendar_content_rect = Some(list_area);
            f.render_widget(
                Paragraph::new(list_lines).block(list_block).scroll((app.scroll as u16, 0)),
                list_area,
            );

            let mut detail_lines: Vec<Line<'static>> = vec![];
            if let Some(sched) = &app.calendar_schedule_detail {
                detail_lines.push(Line::from(Span::styled(" Récurrence sélectionnée ", Style::default().fg(theme.palette().accent))));
                detail_lines.push(Line::from(format!("  ID : {}  │  Nom : {}  │  {}", sched.id, sched.name, if sched.enabled { "activée" } else { "en pause" })));
                if let Some(secs) = sched.interval_seconds {
                    detail_lines.push(Line::from(format!("  Intervalle : {} s", secs)));
                }
                let demand = sched.channel_context.as_deref().unwrap_or(sched.description.as_str());
                if !demand.is_empty() {
                    for line in demand.lines().take(2) {
                        detail_lines.push(Line::from(Span::styled(format!("  {}", line), Style::default().fg(theme.palette().muted))));
                    }
                }
            }
            if let Some(d) = &app.calendar_run_detail {
                if !detail_lines.is_empty() {
                    detail_lines.push(Line::from(""));
                }
                detail_lines.push(Line::from(Span::styled(" Run sélectionné ", Style::default().fg(theme.palette().accent))));
                let run_label = match d.run_status.as_str() {
                    "completed" => "Terminé",
                    "failed" => "Échec",
                    "cancelled" => "Annulé",
                    "running" => "En cours",
                    "queued" => "En attente",
                    _ => d.run_status.as_str(),
                };
                detail_lines.push(Line::from(format!("  Run : {}  │  Tâche : {}", run_label, d.task_status)));
                if let Some((_, last_msg)) = d.progress.last().map(|(p, m)| (*p, m.as_str())) {
                    let preview = last_msg.lines().next().unwrap_or("").chars().take(60).collect::<String>();
                    if !preview.is_empty() {
                        detail_lines.push(Line::from(Span::styled(format!("  {}", preview), Style::default().fg(theme.palette().muted))));
                    }
                }
            }
            if detail_lines.is_empty() {
                detail_lines.push(Line::from(Span::styled(" Sélectionnez une récurrence ou un run ci-dessus pour afficher le détail ici.", Style::default().fg(theme.palette().muted))));
            }
            let detail_block = Block::default()
                .borders(Borders::ALL)
                .title(" Détail (visible au premier coup d’œil) ")
                .border_style(theme.block_border());
            f.render_widget(
                Paragraph::new(detail_lines).block(detail_block).wrap(Wrap { trim: true }),
                detail_area,
            );
        }
        Mode::Memory => {
            let mut lines: Vec<Line<'static>> = vec![
                Line::from(""),
                Line::from(Span::styled(
                    app.i18n.t("memory.short_term_title"),
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
                lines.push(Line::from(Span::styled(app.i18n.t("memory.no_turn"), Style::default().fg(theme.palette().muted))));
                lines.push(Line::from(""));
            }
            if app.memory_search_active {
                lines.push(Line::from(Span::styled(
                    format!("  Recherche: {}_{}", app.memory_search_query, if app.memory_search_query.is_empty() { " (Entrée pour lancer)" } else { "" }),
                    Style::default().fg(theme.palette().accent),
                )));
                lines.push(Line::from(""));
                if app.memory_search_results.is_empty() {
                    if app.memory_search_query.trim().is_empty() {
                        lines.push(Line::from(Span::styled("  Saisir une requête puis Entrée. Escape ou q pour annuler.", Style::default().fg(theme.palette().muted))));
                    } else {
                        lines.push(Line::from(Span::styled("  Aucun résultat.", Style::default().fg(theme.palette().muted))));
                    }
                } else {
                    for (id, content) in &app.memory_search_results {
                        lines.push(Line::from(Span::styled(
                            format!("  (id: {}) {}", &id[..id.len().min(8)], content.chars().take(70).collect::<String>()),
                            Style::default().fg(theme.palette().fg),
                        )));
                        if content.chars().count() > 70 {
                            lines.push(Line::from(Span::styled("    …", Style::default().fg(theme.palette().muted))));
                        }
                        lines.push(Line::from(""));
                    }
                }
            } else if app.memory_view_graph {
                lines.push(Line::from(Span::styled(
                    "  Vue graphe (g pour revenir à la liste)",
                    Style::default().fg(theme.palette().muted),
                )));
                lines.push(Line::from(""));
                if let Some((id, content, created_at, source)) = app.memory_long_term.get(app.memory_lt_selected) {
                    lines.push(Line::from(Span::styled(
                        format!("  ● [{}] {} — {}", source, &created_at[..created_at.len().min(19)], content.chars().take(60).collect::<String>()),
                        Style::default().fg(theme.palette().accent),
                    )));
                    if !id.is_empty() {
                        lines.push(Line::from(Span::styled(format!("    (id: {})", &id[..id.len().min(8)]), Style::default().fg(theme.palette().muted))));
                    }
                    if let Some(related) = app.memory_lt_related.get(id) {
                        for (i, (to_id, kind)) in related.iter().enumerate() {
                            let prefix = if i + 1 == related.len() { "└─" } else { "├─" };
                            let kind_str = if kind.is_empty() { "related" } else { kind.as_str() };
                            let content_excerpt = app.memory_long_term.iter().find(|(eid, _, _, _)| eid == to_id).map(|(_, c, _, _)| c.chars().take(50).collect::<String>());
                            let node_line = if let Some(excerpt) = content_excerpt {
                                format!("  {} {} ({}): {}", prefix, &to_id[..to_id.len().min(8)], kind_str, excerpt)
                            } else {
                                format!("  {} {} ({})", prefix, &to_id[..to_id.len().min(8)], kind_str)
                            };
                            lines.push(Line::from(Span::styled(node_line, Style::default().fg(theme.palette().fg))));
                        }
                    }
                } else {
                    lines.push(Line::from(Span::styled("  Sélectionnez une entrée en vue liste (g pour basculer).", Style::default().fg(theme.palette().muted))));
                }
            } else {
                let lt_status = if app.memory_long_term_available {
                    app.i18n.t("memory.long_term_on")
                } else {
                    app.i18n.t("memory.long_term_off")
                };
                lines.push(Line::from(Span::styled(
                    lt_status,
                    Style::default().fg(theme.palette().accent).add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::from(""));
                let mut lt_line_starts = Vec::new();
                for (idx, (id, content, created_at, source)) in app.memory_long_term.iter().enumerate() {
                lt_line_starts.push(lines.len());
                let style = if idx == app.memory_lt_selected {
                    Style::default().fg(theme.palette().accent)
                } else {
                    Style::default().fg(theme.palette().fg)
                };
                lines.push(Line::from(Span::styled(
                    format!("  [{}] {} — {} {}", source, &created_at[..created_at.len().min(19)], content.chars().take(80).collect::<String>(), if !id.is_empty() { format!("(id: {})", &id[..id.len().min(8)]) } else { String::new() }),
                    style,
                )));
                if content.chars().count() > 80 {
                    lines.push(Line::from(Span::styled("    …", Style::default().fg(theme.palette().muted))));
                }
                if let Some(related) = app.memory_lt_related.get(id) {
                    let parts: Vec<String> = related
                        .iter()
                        .map(|(to_id, kind)| {
                            let content_excerpt = app.memory_long_term.iter().find(|(eid, _, _, _)| eid == to_id).map(|(_, c, _, _)| c.chars().take(40).collect::<String>());
                            if let Some(excerpt) = content_excerpt {
                                format!("{} ({})", excerpt, if kind.is_empty() { "related" } else { kind })
                            } else {
                                format!("{} ({})", &to_id[..to_id.len().min(8)], if kind.is_empty() { "related" } else { kind })
                            }
                        })
                        .collect();
                    lines.push(Line::from(Span::styled(
                        format!("    → lié à: {}", parts.join(", ")),
                        Style::default().fg(theme.palette().muted),
                    )));
                }
                lines.push(Line::from(""));
            }
                app.memory_lt_line_starts = lt_line_starts;
                if app.memory_long_term.is_empty() && app.memory_long_term_available {
                    lines.push(Line::from(Span::styled("  (aucune entrée)", Style::default().fg(theme.palette().muted))));
                }
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
                .title(app.i18n.t("memory.block_title"))
                .border_style(theme.block_border());
            f.render_widget(
                Paragraph::new(lines).block(mem_block).wrap(Wrap { trim: true }).scroll((app.scroll as u16, 0)),
                content_area,
            );
        }
    }

    if let (Some(rect), Some((_, ref question, ref context, ref choices))) = (human_input_area_opt, &app.pending_human_input) {
        let theme = Theme::new(app.theme);
        let style_warning = Style::default().fg(theme.palette().error).add_modifier(Modifier::BOLD);
        let mut banner_lines = vec![
            Line::from(Span::styled(app.i18n.t("tui.action_required"), style_warning)),
            Line::from(question.as_str()),
        ];
        if !context.is_empty() {
            banner_lines.push(Line::from(""));
            banner_lines.push(Line::from(Span::styled(context.as_str(), Style::default().fg(theme.palette().muted))));
        }
        if let Some(ref c) = choices {
            let choice_preview = c.iter().enumerate().map(|(i, s)| format!("{}) {}", i + 1, s)).take(4).collect::<Vec<_>>().join("  ");
            banner_lines.push(Line::from(Span::styled(choice_preview, Style::default().fg(theme.palette().muted))));
        }
        let block = Block::default()
            .borders(Borders::ALL)
            .title(app.i18n.t("tui.reply_prompt"))
            .border_style(theme.block_border());
        f.render_widget(Paragraph::new(banner_lines).block(block).wrap(Wrap { trim: true }), rect);
    }
    if let Some(input_rect) = input_area_opt {
        let input_label = if app.pending_human_input.is_some() {
            app.i18n.t("tui.input_label_reply")
        } else {
            app.i18n.t("tui.input_label_chat")
        };
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
    progress_rx: &mpsc::Receiver<(String, u8)>,
) -> anyhow::Result<()> {
    let mut last_health = std::time::Instant::now();
    loop {
        if app.mode == Mode::Chat && !app.chat_history_loaded {
            app.fetch_chat_history_from_memory();
        }
        if last_health.elapsed() > Duration::from_secs(5) {
            app.check_health();
            if app.mode == Mode::Chat {
                app.fetch_schedule_reports();
                app.fetch_pending_human_input_list();
                if let Some(task_id) = app.pending_reply_task_id.clone() {
                    app.fetch_pending_human_input(&task_id);
                }
            }
            last_health = std::time::Instant::now();
        }
        if app.mode == Mode::Router {
            if app.metrics.is_empty() {
                app.fetch_metrics();
            }
            if app.operator_ops_text.is_empty() {
                app.fetch_operator_ops_snapshot();
            }
        }
        while let Ok((task_id, pct)) = progress_rx.try_recv() {
            if app.pending_reply_task_id.as_deref() == Some(&task_id) {
                app.pending_reply_pct = Some(pct);
            }
        }
        while let Ok(result) = rx.try_recv() {
            app.loading = false;
            match result {
                Ok((text, session_id, pending_task_id)) => {
                    let role = if session_id.is_empty() { "system" } else { "assistant" };
                    if !session_id.is_empty() {
                        app.session_id = Some(session_id);
                    }
                    app.pending_reply_task_id = pending_task_id.clone();
                    if pending_task_id.is_none() {
                        app.pending_reply_pct = None;
                        app.pending_human_input = None;
                    }
                    app.messages.push(ChatMessage {
                        role: role.into(),
                        text,
                        is_error: false,
                    });
                }
                Err(e) => {
                    app.pending_reply_task_id = None;
                    app.pending_human_input = None;
                    app.messages.push(ChatMessage {
                        role: "system".into(),
                        text: e,
                        is_error: true,
                    });
                }
            }
            app.scroll = usize::MAX;
        }

        terminal.draw(|f| ui(f, app))?;

        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Mouse(mouse) => {
                    // Click on tabs bar to switch tab
                    if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                        if let Some(rect) = app.tabs_rect {
                            let x = mouse.column;
                            let y = mouse.row;
                            if x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height {
                                const N_TABS: u16 = 7;
                                let tab_w = (rect.width / N_TABS).max(1);
                                let col = x.saturating_sub(rect.x);
                                let tab_idx = (col / tab_w).min(N_TABS - 1) as usize;
                                let new_mode = match tab_idx {
                                    0 => Mode::Chat,
                                    1 => Mode::ScheduleReports,
                                    2 => Mode::Router,
                                    3 => Mode::Doc,
                                    4 => Mode::Tasks,
                                    5 => Mode::Calendar,
                                    _ => Mode::Memory,
                                };
                                if app.mode != new_mode {
                                    app.mode = new_mode;
                                    app.trigger_mode_entered();
                                }
                            }
                        }
                    }
                    // Click in Calendar content to select schedule or run
                    if app.mode == Mode::Calendar
                        && mouse.kind == MouseEventKind::Down(MouseButton::Left)
                    {
                        if let Some(rect) = app.calendar_content_rect {
                            let inner_y = mouse.row.saturating_sub(rect.y + 1);
                            if inner_y < rect.height.saturating_sub(2) {
                                let actual_line = app.scroll + inner_y as usize;
                                let n_sched = app.calendar_schedules.len();
                                let n_runs = app.calendar_task_runs.len();
                                const SCHEDULE_START: usize = 3;
                                let schedule_end = SCHEDULE_START + n_sched;
                                // When there are no schedules, a single "Aucune récurrence" placeholder
                                // line is rendered, so runs_start must account for that extra line.
                                let runs_start = SCHEDULE_START + if n_sched == 0 { 1 } else { n_sched } + 3;
                                let runs_end = runs_start + n_runs;
                                if actual_line >= SCHEDULE_START && actual_line < schedule_end && n_sched > 0 {
                                    let idx = actual_line - SCHEDULE_START;
                                    app.calendar_focus_schedules = true;
                                    app.calendar_schedule_index = idx;
                                    if let Some((id, _, _, _)) = app.calendar_schedules.get(idx) {
                                        let id = id.clone();
                                        app.fetch_schedule_detail(&id);
                                    }
                                } else if actual_line >= runs_start && actual_line < runs_end && n_runs > 0 {
                                    let idx = actual_line - runs_start;
                                    app.calendar_focus_schedules = false;
                                    app.calendar_selected_run = Some(idx);
                                    if let Some(run) = app.calendar_task_runs.get(idx) {
                                        let run = run.clone();
                                        app.fetch_calendar_run_detail(&run);
                                    }
                                }
                            }
                        }
                    }
                    // Mouse wheel: scroll in Chat, Doc, Memory, Calendar
                    if matches!(mouse.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown) {
                        match app.mode {
                            Mode::Chat | Mode::ScheduleReports | Mode::Doc | Mode::Memory | Mode::Calendar => {
                                if mouse.kind == MouseEventKind::ScrollUp {
                                    app.scroll_up();
                                } else {
                                    app.scroll_down();
                                }
                            }
                            Mode::Tasks => {
                                if app.activity_list_collapsed {
                                    if mouse.kind == MouseEventKind::ScrollUp {
                                        app.scroll_up();
                                    } else {
                                        app.scroll_down();
                                    }
                                } else if mouse.kind == MouseEventKind::ScrollUp {
                                    app.activity_detail_scroll = app.activity_detail_scroll.saturating_sub(1);
                                } else {
                                    app.activity_detail_scroll = app.activity_detail_scroll.saturating_add(1);
                                }
                            }
                            _ => {}
                        }
                    }
                    // Click on task list row (Tasks tab)
                    if app.mode == Mode::Tasks
                        && mouse.kind == MouseEventKind::Down(MouseButton::Left)
                        && !app.activity_tasks.is_empty()
                        && !app.activity_list_collapsed
                    {
                        if let Some(rect) = app.activity_list_rect {
                            let x = mouse.column;
                            let y = mouse.row;
                            if x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height {
                                let inner_y = (y - rect.y).saturating_sub(1);
                                let content_line = app.activity_list_scroll + inner_y as usize;
                                if content_line >= 3 {
                                    let task_idx = content_line - 3;
                                    if task_idx < app.activity_visible_indices.len() {
                                        app.activity_selected = task_idx;
                                        app.activity_detail_scroll = 0;
                                        app.fetch_activity_events_for_selected();
                                        app.fetch_activity_task_detail();
                                    }
                                }
                            }
                        }
                    }
                }
                Event::Key(key) => {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match (app.mode, key.code, key.modifiers) {
                    (Mode::Memory, KeyCode::Esc, _) if app.memory_search_active => {
                        app.memory_search_active = false;
                        app.memory_search_query.clear();
                    }
                    (_, KeyCode::Esc, _) | (_, KeyCode::Char('q'), KeyModifiers::CONTROL) => return Ok(()),
                    (Mode::Calendar, KeyCode::Left, _) => {
                        app.calendar_focus_schedules = true;
                        app.calendar_schedule_index = app.calendar_schedule_index.min(app.calendar_schedules.len().saturating_sub(1));
                        let id_opt = app.calendar_schedules.get(app.calendar_schedule_index).map(|(id, _, _, _)| id.clone());
                        if let Some(id) = id_opt {
                            app.fetch_schedule_detail(&id);
                        } else {
                            app.calendar_schedule_detail = None;
                        }
                    }
                    (Mode::Calendar, KeyCode::Right, _) => {
                        app.calendar_focus_schedules = false;
                        if app.calendar_task_runs.is_empty() {
                            app.calendar_run_detail = None;
                        } else {
                            let i = app.calendar_selected_run.unwrap_or(0).min(app.calendar_task_runs.len() - 1);
                            app.calendar_selected_run = Some(i);
                            let run_opt = app.calendar_task_runs.get(i).cloned();
                            if let Some(run) = run_opt {
                                app.fetch_calendar_run_detail(&run);
                            }
                        }
                    }
                    (_, KeyCode::Tab, _) => {
                        app.mode = match app.mode {
                            Mode::Chat => Mode::ScheduleReports,
                            Mode::ScheduleReports => Mode::Router,
                            Mode::Router => Mode::Doc,
                            Mode::Doc => Mode::Tasks,
                            Mode::Tasks => Mode::Calendar,
                            Mode::Calendar => Mode::Memory,
                            Mode::Memory => Mode::Chat,
                        };
                        app.trigger_mode_entered();
                    }
                    (_, KeyCode::Char(c), _) if app.mode != Mode::Chat && ('1'..='7').contains(&c) => {
                        let idx = (c as u8 - b'1') as usize;
                        let new_mode = match idx {
                            0 => Mode::Chat,
                            1 => Mode::ScheduleReports,
                            2 => Mode::Router,
                            3 => Mode::Doc,
                            4 => Mode::Tasks,
                            5 => Mode::Calendar,
                            6 => Mode::Memory,
                            _ => continue,
                        };
                        if app.mode != new_mode {
                            app.mode = new_mode;
                            app.trigger_mode_entered();
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
                            // Human in the loop: if the agent is waiting for a reply, send it and don't send as normal message
                            let pending = app.pending_human_input.clone();
                            if let Some((task_id, _, _, choices)) = pending {
                                let response = if let Some(ref c) = choices {
                                    if msg == "1" && c.len() >= 1 {
                                        c[0].clone()
                                    } else if msg == "2" && c.len() >= 2 {
                                        c[1].clone()
                                    } else if msg == "3" && c.len() >= 3 {
                                        c[2].clone()
                                    } else if msg == "4" && c.len() >= 4 {
                                        c[3].clone()
                                    } else {
                                        msg.clone()
                                    }
                                } else {
                                    msg.clone()
                                };
                                if app.submit_human_reply(&task_id, &response) {
                                    app.messages.push(ChatMessage {
                                        role: "user".into(),
                                        text: msg,
                                        is_error: false,
                                    });
                                    app.input.clear();
                                    app.input_scroll = 0;
                                    app.scroll = usize::MAX;
                                }
                                continue;
                            }
                            app.messages.push(ChatMessage {
                                role: "user".into(),
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
                                    role: "system".into(),
                                    text: app.i18n.t("tui.new_session").into(),
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
                                    let i18n = app.i18n.clone();
                                    thread::spawn(move || {
                                        let result = App::run_slash_command_blocking(port, &cmd, &i18n);
                                        let _ = tx.send(Ok((result, String::new(), None)));
                                    });
                                } else {
                                    let result = App::run_slash_command_blocking(app.port, &msg, &app.i18n);
                                    app.messages.push(ChatMessage {
                                        role: "system".into(),
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
                            let progress_tx = app.progress_tx.clone();
                            let i18n = app.i18n.clone();
                            thread::spawn(move || {
                                App::send_message_non_blocking(tx, progress_tx, msg, port, session_id, new_session, i18n);
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
                        app.fetch_operator_ops_snapshot();
                    }
                    (Mode::Router, KeyCode::PageUp, _) => {
                        app.operator_ops_scroll = app.operator_ops_scroll.saturating_sub(8);
                    }
                    (Mode::Router, KeyCode::PageDown, _) => {
                        app.operator_ops_scroll = app.operator_ops_scroll.saturating_add(8);
                    }
                    (Mode::ScheduleReports, KeyCode::Char('r') | KeyCode::Char('R'), _) => {
                        app.fetch_schedule_reports();
                    }
                    (Mode::Doc, KeyCode::Up, _) => app.scroll_up(),
                    (Mode::Doc, KeyCode::Down, _) => app.scroll_down(),
                    (Mode::Doc, KeyCode::PageUp, _) => app.scroll_page_up(),
                    (Mode::Doc, KeyCode::PageDown, _) => app.scroll_page_down(),
                    (Mode::Doc, KeyCode::Home, _) => app.scroll = 0,
                    (Mode::Doc, KeyCode::End, _) => app.scroll_to_bottom(),
                    (Mode::ScheduleReports, KeyCode::Up, _) => app.scroll_up(),
                    (Mode::ScheduleReports, KeyCode::Down, _) => app.scroll_down(),
                    (Mode::ScheduleReports, KeyCode::PageUp, _) => app.scroll_page_up(),
                    (Mode::ScheduleReports, KeyCode::PageDown, _) => app.scroll_page_down(),
                    (Mode::ScheduleReports, KeyCode::Home, _) => app.scroll = 0,
                    (Mode::ScheduleReports, KeyCode::End, _) => app.scroll_to_bottom(),
                    (Mode::Memory, KeyCode::Char('/'), _) if !app.memory_search_active => {
                        app.memory_search_active = true;
                    }
                    (Mode::Memory, KeyCode::Char('s'), _) if !app.memory_search_active => {
                        app.memory_search_active = true;
                    }
                    (Mode::Memory, KeyCode::Char('g') | KeyCode::Char('G'), _) if !app.memory_search_active => {
                        app.memory_view_graph = !app.memory_view_graph;
                    }
                    (Mode::Memory, KeyCode::Char('q'), _) if app.memory_search_active => {
                        app.memory_search_active = false;
                        app.memory_search_query.clear();
                    }
                    (Mode::Memory, KeyCode::Enter, _) if app.memory_search_active => {
                        app.fetch_memory_search();
                    }
                    (Mode::Memory, KeyCode::Backspace, _) if app.memory_search_active => {
                        app.memory_search_query.pop();
                    }
                    (Mode::Memory, KeyCode::Char(c), _) if app.memory_search_active => {
                        app.memory_search_query.push(c);
                    }
                    (Mode::Memory, KeyCode::Up, _) if !app.memory_search_active => {
                        if !app.memory_long_term.is_empty() && app.memory_lt_selected > 0 {
                            app.memory_lt_selected -= 1;
                            if let Some(&line) = app.memory_lt_line_starts.get(app.memory_lt_selected) {
                                app.scroll = line.min(app.max_scroll());
                            }
                        } else {
                            app.scroll_up();
                        }
                    }
                    (Mode::Memory, KeyCode::Down, _) if !app.memory_search_active => {
                        if !app.memory_long_term.is_empty() && app.memory_lt_selected + 1 < app.memory_long_term.len() {
                            app.memory_lt_selected += 1;
                            if let Some(&line) = app.memory_lt_line_starts.get(app.memory_lt_selected) {
                                app.scroll = line.min(app.max_scroll());
                            }
                        } else {
                            app.scroll_down();
                        }
                    }
                    (Mode::Memory, KeyCode::PageUp, _) if !app.memory_search_active => app.scroll_page_up(),
                    (Mode::Memory, KeyCode::PageDown, _) if !app.memory_search_active => app.scroll_page_down(),
                    (Mode::Memory, KeyCode::Home, _) if !app.memory_search_active => app.scroll = 0,
                    (Mode::Memory, KeyCode::End, _) if !app.memory_search_active => app.scroll_to_bottom(),
                    (Mode::Memory, KeyCode::Char('d') | KeyCode::Char('D'), _) if !app.memory_search_active => app.delete_memory_long_term_selected(),
                    (Mode::Memory, KeyCode::Delete, _) if !app.memory_search_active => app.delete_memory_long_term_selected(),
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
                        if app.activity_selected + 1 < app.activity_visible_indices.len() {
                            app.activity_selected += 1;
                            app.activity_detail_scroll = 0;
                            app.fetch_activity_events_for_selected();
                            app.fetch_activity_task_detail();
                        }
                    }
                    (Mode::Tasks, KeyCode::Char('d') | KeyCode::Char('D'), _) => {
                        app.activity_show_roots_only = !app.activity_show_roots_only;
                        app.activity_visible_indices = if app.activity_show_roots_only {
                            app.activity_tasks.iter().enumerate()
                                .filter(|(_, r)| r.parent_task_id.is_none())
                                .map(|(i, _)| i)
                                .collect()
                        } else {
                            (0..app.activity_tasks.len()).collect()
                        };
                        if app.activity_selected >= app.activity_visible_indices.len() && !app.activity_visible_indices.is_empty() {
                            app.activity_selected = app.activity_visible_indices.len() - 1;
                        }
                        app.fetch_activity_events_for_selected();
                        app.fetch_activity_task_detail();
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
                    (Mode::Tasks, KeyCode::Char(' '), _) => {
                        app.activity_list_collapsed = !app.activity_list_collapsed;
                    }
                    (Mode::Tasks, KeyCode::Enter, m) if m.is_empty() => {
                        app.activity_list_collapsed = !app.activity_list_collapsed;
                    }
                    (Mode::Calendar, KeyCode::Char('r') | KeyCode::Char('R'), _) => {
                        app.fetch_calendar();
                    }
                    (Mode::Calendar, KeyCode::Up, _) => {
                        if app.calendar_focus_schedules {
                            let n = app.calendar_schedules.len();
                            if n > 0 {
                                app.calendar_schedule_index = app.calendar_schedule_index.saturating_sub(1).min(n - 1);
                                let id_opt = app.calendar_schedules.get(app.calendar_schedule_index).map(|(id, _, _, _)| id.clone());
                                if let Some(id) = id_opt {
                                    app.fetch_schedule_detail(&id);
                                }
                            }
                        } else {
                            let n = app.calendar_task_runs.len();
                            if n > 0 {
                                app.calendar_selected_run = Some(match app.calendar_selected_run {
                                    None => n - 1,
                                    Some(i) if i > 0 => i - 1,
                                    Some(i) => i,
                                });
                                if let Some(i) = app.calendar_selected_run {
                                    let run_opt = app.calendar_task_runs.get(i).cloned();
                                    if let Some(run) = run_opt {
                                        app.fetch_calendar_run_detail(&run);
                                    }
                                }
                            }
                        }
                    }
                    (Mode::Calendar, KeyCode::Down, _) => {
                        if app.calendar_focus_schedules {
                            let n = app.calendar_schedules.len();
                            if n > 0 {
                                app.calendar_schedule_index = (app.calendar_schedule_index + 1).min(n - 1);
                                let id_opt = app.calendar_schedules.get(app.calendar_schedule_index).map(|(id, _, _, _)| id.clone());
                                if let Some(id) = id_opt {
                                    app.fetch_schedule_detail(&id);
                                }
                            }
                        } else {
                            let n = app.calendar_task_runs.len();
                            if n > 0 {
                                app.calendar_selected_run = Some(match app.calendar_selected_run {
                                    None => 0,
                                    Some(i) if i + 1 < n => i + 1,
                                    Some(i) => i,
                                });
                                if let Some(i) = app.calendar_selected_run {
                                    let run_opt = app.calendar_task_runs.get(i).cloned();
                                    if let Some(run) = run_opt {
                                        app.fetch_calendar_run_detail(&run);
                                    }
                                }
                            }
                        }
                    }
                    (Mode::Calendar, KeyCode::PageUp, _) => {
                        app.scroll_page_up();
                    }
                    (Mode::Calendar, KeyCode::PageDown, _) => {
                        app.scroll_page_down();
                    }
                    (Mode::Calendar, KeyCode::Home, _) => {
                        app.scroll = 0;
                    }
                    (Mode::Calendar, KeyCode::End, _) => {
                        app.scroll_to_bottom();
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
                        save_theme_to_disk(app.theme);
                    }
                    _ => {}
                }
                }
                _ => {}
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
    let (progress_tx, progress_rx) = mpsc::channel::<(String, u8)>();
    let mut app = App::new(port, tx, Some(progress_tx));
    if let Some(saved) = load_theme_from_disk() {
        app.theme = saved;
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    app.check_health();
    let result = run_app(&mut terminal, &mut app, &rx, &progress_rx);

    disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;

    result
}
