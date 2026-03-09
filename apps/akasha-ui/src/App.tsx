import { useState, useEffect, useCallback, useRef, lazy, Suspense, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCached, setCached } from "./useTabCache";
import { useI18n } from "./useI18n";

const LazyMarkdownContent = lazy(() => import("./MarkdownContent").then((m) => ({ default: m.default })));

const DAEMON_PORT = 3876;
const THEME_STORAGE_KEY = "akasha_theme";

export type ThemeId = "dark" | "dark_nord" | "light" | "light_latte";

const THEME_IDS: ThemeId[] = ["dark", "dark_nord", "light", "light_latte"];

function loadSavedTheme(): ThemeId {
  try {
    const s = localStorage.getItem(THEME_STORAGE_KEY);
    if (s && THEME_IDS.includes(s as ThemeId)) return s as ThemeId;
  } catch {
    /* ignore */
  }
  return "dark";
}

type Tab = "chat" | "router" | "settings" | "docs" | "tasks" | "calendar" | "memory";

/** Format duration in seconds as "X min Y s" or "Y s". */
function formatDurationSec(sec: number): string {
  const total = Math.round(sec);
  if (total < 60) return `${total} s`;
  const m = Math.floor(total / 60);
  const s = total % 60;
  return s > 0 ? `${m} min ${s} s` : `${m} min`;
}

/** Compare version strings "X.Y.Z"; returns true if remote > current. */
function versionGt(remote: string, current: string): boolean {
  const parse = (s: string) => {
    const t = s.replace(/^v/, "").trim();
    const parts = t.split(".").map((p) => parseInt(p, 10) || 0);
    return [parts[0] ?? 0, parts[1] ?? 0, parts[2] ?? 0] as const;
  };
  const [r0, r1, r2] = parse(remote);
  const [c0, c1, c2] = parse(current);
  if (r0 !== c0) return r0 > c0;
  if (r1 !== c1) return r1 > c1;
  return r2 > c2;
}

/** Parse "TOOL: ask_user" + JSON from assistant message text. Returns null if not present or invalid. */
function parseAskUserMessage(
  text: string
): { question: string; context?: string; choices?: string[] } | null {
  if (!text?.includes("ask_user")) return null;
  const i = text.indexOf("{");
  if (i === -1) return null;
  let depth = 0;
  let end = -1;
  for (let j = i; j < text.length; j++) {
    if (text[j] === "{") depth++;
    else if (text[j] === "}") {
      depth--;
      if (depth === 0) {
        end = j;
        break;
      }
    }
  }
  if (end === -1) return null;
  try {
    const obj = JSON.parse(text.slice(i, end + 1)) as Record<string, unknown>;
    const question = obj.question ?? "";
    if (typeof question !== "string" || !question.trim()) return null;
    return {
      question: String(question).trim(),
      context: obj.context != null ? String(obj.context).trim() : undefined,
      choices: Array.isArray(obj.choices)
        ? (obj.choices as unknown[]).map((c) => String(c))
        : undefined,
    };
  } catch {
    return null;
  }
}

interface HealthState {
  ok: boolean;
  port?: number;
}

interface ModelMetricsEntry {
  total_requests: number;
  successful_requests: number;
  failed_requests: number;
  total_latency_ms: number;
  total_tokens: number;
  total_cost_usd: number;
  fallback_triggered: number;
  fallback_success: number;
  last_success?: string;
  last_failure?: string;
}

type RouterMetrics = Record<string, ModelMetricsEntry>;

function App() {
  const { t, locale, setLocale } = useI18n();
  const themes = useMemo(
    () =>
      THEME_IDS.map((id) => ({
        id,
        label: t("theme." + id),
      })),
    [t]
  );
  const [tab, setTab] = useState<Tab>("chat");
  const [theme, setTheme] = useState<ThemeId>(loadSavedTheme);
  const eventLabel = useCallback(
    (typ: string) => {
      const key = "events." + typ;
      const s = t(key);
      return s === key ? typ : s;
    },
    [t]
  );
  const [health, setHealth] = useState<HealthState | null>(null);
  const [message, setMessage] = useState("");
  const [messages, setMessages] = useState<
    Array<{ role: "user" | "assistant" | "system"; text: string; error?: boolean }>
  >([]);
  const [loading, setLoading] = useState(false);
  const [routerMetrics, setRouterMetrics] = useState<RouterMetrics | null>(null);
  const [routerLoading, setRouterLoading] = useState(false);
  const [routerError, setRouterError] = useState<string | null>(null);
  const [docContent, setDocContent] = useState<string | null>(null);
  const [docLoading, setDocLoading] = useState(false);
  const [docError, setDocError] = useState<string | null>(null);
  const [tasksList, setTasksList] = useState<Array<{ id: string; status: string }>>([]);
  const [tasksSelected, setTasksSelected] = useState(0);
  const [tasksEvents, setTasksEvents] = useState<Array<{ event_type: string; payload?: unknown; at: string }>>([]);
  const [tasksLoading, setTasksLoading] = useState(false);
  const [runningTaskChips, setRunningTaskChips] = useState<Record<string, { pct?: number; message?: string }>>({});
  /** Events (sub_agent_spawned, progress_update, etc.) per running task for collapsible sub-agent panel. Each event may have task_id (root or child). */
  const [runningTaskEvents, setRunningTaskEvents] = useState<Record<string, Array<{ event_type: string; payload?: unknown; at: string; task_id?: string }>>>({});
  /** Human in the loop: when the agent asks for user input, we store question/context/choices per task_id. */
  const [pendingHumanInput, setPendingHumanInput] = useState<Record<string, { question: string; context: string; choices?: string[] }>>({});
  /** Task id for which the human-input modal is open (null = closed). */
  const [humanInputModalTaskId, setHumanInputModalTaskId] = useState<string | null>(null);
  const [humanInputFreeText, setHumanInputFreeText] = useState("");
  /** Reply text for the inline ask_user form in the chat (when modal is not used). */
  const [inlineHumanReplyText, setInlineHumanReplyText] = useState("");
  const [subAgentPanelCollapsed, setSubAgentPanelCollapsed] = useState(true);
  /** Per-root task: whether the discussion block is collapsed in the sub-agent panel (true = collapsed). */
  const [collapsedRootTasks, setCollapsedRootTasks] = useState<Record<string, boolean>>({});
  const [schedules, setSchedules] = useState<Array<{ id: string; name: string; enabled: boolean; interval_seconds?: number }>>([]);
  const [taskRuns, setTaskRuns] = useState<Array<{
    id: string;
    schedule_id?: string;
    task_id: string;
    status: string;
    planned_for: string;
    started_at?: string;
    ended_at?: string;
    label?: string;
  }>>([]);
  const [calendarLoading, setCalendarLoading] = useState(false);
  const [calendarSelectedTaskId, setCalendarSelectedTaskId] = useState<string | null>(null);
  const [calendarTaskDetail, setCalendarTaskDetail] = useState<{
    status: string;
    created_at?: string;
    updated_at?: string;
    progress?: Array<{ progress_pct?: number; message?: string }>;
  } | null>(null);
  const [calendarSelectedScheduleId, setCalendarSelectedScheduleId] = useState<string | null>(null);
  const [scheduleDetail, setScheduleDetail] = useState<{
    id: string;
    name: string;
    description: string;
    enabled: boolean;
    interval_seconds?: number;
    timezone?: string;
    rrule?: string;
    channel_context?: string | null;
  } | null>(null);
  const [scheduleDetailError, setScheduleDetailError] = useState<string | null>(null);
  const [_calendarRunsCollapsed, _setCalendarRunsCollapsed] = useState(false);
  type CalendarGridView = "day" | "week" | "month";
  const [calendarGridView, setCalendarGridView] = useState<CalendarGridView>("week");
  const [calendarGridEvents, setCalendarGridEvents] = useState<Array<{ at: string; task_id: string; type: string; status: string; label?: string }>>([]);
  const [calendarGridDate, setCalendarGridDate] = useState(() => new Date());
  type CalendarSubTab = "grid" | "recent" | "schedules";
  const [calendarSubTab, setCalendarSubTab] = useState<CalendarSubTab>("grid");
  const [memoryShortTerm, setMemoryShortTerm] = useState<Array<{ role: string; content: string }>>([]);
  const [memoryLongTerm, setMemoryLongTerm] = useState<Array<{ id?: string; content: string; created_at: string; source: string }>>([]);
  const [memoryLongTermAvailable, setMemoryLongTermAvailable] = useState(false);
  const [memoryLoading, setMemoryLoading] = useState(false);
  const [memoryError, setMemoryError] = useState<string | null>(null);
  type MemorySubTab = "short" | "long";
  const [memorySubTab, setMemorySubTab] = useState<MemorySubTab>("short");
  const [scheduleReports, setScheduleReports] = useState<Array<{ schedule_name: string; message: string; ended_at?: string }>>([]);
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [userRagDocuments, setUserRagDocuments] = useState<Array<{ id: string; name: string; mime_type: string; added_at: string }>>([]);
  const [userRagLoading, setUserRagLoading] = useState(false);
  const [userRagError, setUserRagError] = useState<string | null>(null);
  const userRagFileInputRef = useRef<HTMLInputElement>(null);
  /** Attachments for the next message: images (vision) and documents (text appended to message). */
  const [attachments, setAttachments] = useState<Array<{ id: string; name: string; typ: "image" | "document"; content_base64: string; mime_type: string }>>([]);
  const chatEndRef = useRef<HTMLDivElement>(null);
  const chatInputRef = useRef<HTMLInputElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  /** Tasks for which we already auto-opened the human-input modal (avoid re-opening every poll). */
  const humanInputAutoOpenedRef = useRef<Set<string>>(new Set());
  /** Whether the "pending actions" notification dropdown is open. */
  const [pendingNotifOpen, setPendingNotifOpen] = useState(false);
  const pendingNotifRef = useRef<HTMLDivElement>(null);
  /** Update banner: when set, a new version is available. Null when dismissed or no update. */
  const [updateBannerInfo, setUpdateBannerInfo] = useState<{
    remote_version: string;
    current_version: string;
    download_url: string;
    release_notes_url?: string | null;
  } | null>(null);

  const checkHealth = useCallback(async () => {
    try {
      const result = await invoke<{ ok: boolean; port?: number }>("check_health", {
        port: DAEMON_PORT,
      });
      setHealth({ ok: result.ok, port: result.port ?? DAEMON_PORT });
    } catch {
      setHealth({ ok: false, port: DAEMON_PORT });
    }
  }, []);

  useEffect(() => {
    checkHealth();
    const id = setInterval(checkHealth, 10000);
    return () => clearInterval(id);
  }, [checkHealth]);

  // Check for app update (daemon caches latest.json; compare with app version)
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const [currentVersion, status] = await Promise.all([
          invoke<string>("get_app_version"),
          invoke<{ remote_version?: string; download_url?: string; error?: string | null }>("get_update_status", {
            port: DAEMON_PORT,
          }),
        ]);
        if (cancelled) return;
        if (status?.error || !status?.remote_version) return;
        const remote = (status.remote_version ?? "0.0.0").trim();
        const current = (currentVersion ?? "0.0.0").trim();
        if (versionGt(remote, current) && status.download_url) {
          setUpdateBannerInfo({
            remote_version: remote,
            current_version: current,
            download_url: status.download_url,
            release_notes_url: (status as { release_notes_url?: string | null }).release_notes_url,
          });
        }
      } catch {
        /* daemon may be down; skip banner */
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  // Fetch all pending human-input (agent questions) on load and periodically, so user sees them after relaunch or when popup was missed.
  const fetchPendingHumanInput = useCallback(async () => {
    if (!health?.ok) return;
    try {
      const data = await invoke<{ pending?: Array<{ task_id: string; question: string; context?: string; choices?: string[] }> }>(
        "get_pending_human_input",
        { port: DAEMON_PORT }
      );
      if (data?.pending?.length) {
        setPendingHumanInput((prev) => {
          const next = { ...prev };
          for (const p of data.pending!) {
            next[p.task_id] = {
              question: p.question ?? "",
              context: p.context ?? "",
              choices: p.choices,
            };
          }
          return next;
        });
      }
    } catch {
      /* ignore */
    }
  }, [health?.ok]);

  useEffect(() => {
    fetchPendingHumanInput();
    const id = setInterval(fetchPendingHumanInput, 25000);
    return () => clearInterval(id);
  }, [fetchPendingHumanInput]);

  // Load today's conversation history on mount (short-term = current day, so it survives UI restart).
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const data = await invoke<{ session_id?: string; turns?: Array<{ role: string; content: string }> }>(
          "get_memory_short_term",
          { port: DAEMON_PORT }
        );
        if (cancelled) return;
        if (data?.session_id && (data.turns?.length ?? 0) > 0) {
          setMessages(
            data.turns!.map((t) => ({
              role: (t.role === "user" ? "user" : t.role === "assistant" ? "assistant" : "system") as "user" | "assistant" | "system",
              text: t.content,
            }))
          );
          setSessionId(data.session_id);
        } else if (data?.session_id) {
          setSessionId(data.session_id);
        }
      } catch {
        /* ignore */
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  // Apply theme to document (for CSS variables)
  useEffect(() => {
    document.documentElement.setAttribute("data-theme", theme);
  }, [theme]);

  const setThemeAndSave = useCallback((next: ThemeId) => {
    setTheme(next);
    try {
      localStorage.setItem(THEME_STORAGE_KEY, next);
    } catch {
      /* ignore */
    }
  }, []);

  // Scroll chat to last message and keep focus on input
  useEffect(() => {
    chatEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages, loading]);
  useEffect(() => {
    if (tab === "chat") chatInputRef.current?.focus();
  }, [tab]);

  // Close pending-actions dropdown when clicking outside
  useEffect(() => {
    if (!pendingNotifOpen) return;
    const onDocClick = (e: MouseEvent) => {
      if (pendingNotifRef.current && !pendingNotifRef.current.contains(e.target as Node)) {
        setPendingNotifOpen(false);
      }
    };
    document.addEventListener("click", onDocClick);
    return () => document.removeEventListener("click", onDocClick);
  }, [pendingNotifOpen]);

  // Global keyboard shortcuts: 1–7 = switch tab (when not in a modal or input)
  const tabsByIndex: Tab[] = ["chat", "router", "docs", "tasks", "calendar", "memory", "settings"];
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (humanInputModalTaskId != null) return;
      const target = e.target as HTMLElement;
      if (target?.closest("input") || target?.closest("textarea") || target?.closest("[role='dialog']")) return;
      const n = e.key === "1" ? 1 : e.key === "2" ? 2 : e.key === "3" ? 3 : e.key === "4" ? 4 : e.key === "5" ? 5 : e.key === "6" ? 6 : e.key === "7" ? 7 : 0;
      if (n >= 1 && n <= 7) {
        e.preventDefault();
        setTab(tabsByIndex[n - 1]);
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [humanInputModalTaskId]);

  type MetricsPeriod = "all" | "day" | "week" | "month" | "year";
  const [routerMetricsPeriod, setRouterMetricsPeriod] = useState<MetricsPeriod>("all");

  const fetchRouterMetrics = useCallback(async () => {
    setRouterLoading(true);
    setRouterError(null);
    try {
      const data = await invoke<RouterMetrics>("get_router_metrics", {
        port: DAEMON_PORT,
        period: routerMetricsPeriod === "all" ? undefined : routerMetricsPeriod,
      });
      const metrics = data as RouterMetrics;
      setRouterMetrics(metrics);
      setCached("router", metrics);
    } catch (e) {
      setRouterError(String(e));
      setRouterMetrics(null);
    } finally {
      setRouterLoading(false);
    }
  }, [routerMetricsPeriod]);

  useEffect(() => {
    if (tab !== "router") return;
    const cached = getCached<RouterMetrics>("router");
    if (cached != null) {
      setRouterMetrics(cached);
      setRouterLoading(false);
      setRouterError(null);
      return;
    }
    fetchRouterMetrics();
  }, [tab, fetchRouterMetrics]);

  const fetchDocs = useCallback(async () => {
    setDocLoading(true);
    setDocError(null);
    try {
      const content = await invoke<string>("get_docs", { port: DAEMON_PORT });
      setDocContent(content);
      setCached("docs", content);
    } catch (e) {
      setDocError(String(e));
      setDocContent(null);
    } finally {
      setDocLoading(false);
    }
  }, []);

  useEffect(() => {
    if (tab !== "docs") return;
    const cached = getCached<string>("docs");
    if (cached != null) {
      setDocContent(cached);
      setDocLoading(false);
      setDocError(null);
      return;
    }
    fetchDocs();
  }, [tab, fetchDocs]);

  const fetchTasksList = useCallback(async () => {
    setTasksLoading(true);
    try {
      const data = await invoke<{ tasks?: Array<{ id?: string; status?: string }> }>("get_tasks", {
        port: DAEMON_PORT,
      });
      const list = data?.tasks ?? [];
      const tasks = list
        .map((t) => ({ id: t.id ?? "", status: t.status ?? "?" }))
        .filter((t) => t.id);
      setTasksList(tasks);
      setTasksSelected((prev) => (prev >= tasks.length && tasks.length > 0 ? tasks.length - 1 : prev));
      setCached("tasks", tasks);
    } catch {
      setTasksList([]);
    } finally {
      setTasksLoading(false);
    }
  }, []);

  const fetchTasksEvents = useCallback(async (taskId: string) => {
    try {
      const data = await invoke<{ events?: Array<{ event_type?: string; payload?: unknown; at?: string }> }>(
        "get_task_events",
        { taskId, port: DAEMON_PORT }
      );
      const list = data?.events ?? [];
      setTasksEvents(
        list.map((e) => ({
          event_type: e.event_type ?? "?",
          payload: e.payload,
          at: e.at ?? "",
        }))
      );
    } catch {
      setTasksEvents([]);
    }
  }, []);

  const fetchCalendar = useCallback(async () => {
    setCalendarLoading(true);
    try {
      const [schedData, runsData] = await Promise.all([
        invoke<{ schedules?: Array<{ id?: string; name?: string; enabled?: boolean; interval_seconds?: number }> }>("get_schedules", { port: DAEMON_PORT }),
        invoke<{ task_runs?: Array<{ id?: string; schedule_id?: string; task_id?: string; status?: string; planned_for?: string; started_at?: string; ended_at?: string; label?: string }> }>("get_task_runs", { port: DAEMON_PORT }),
      ]);
      const sched = (schedData?.schedules ?? []).map((s) => ({ id: s.id ?? "", name: s.name ?? "", enabled: s.enabled ?? false, interval_seconds: s.interval_seconds }));
      const runs = (runsData?.task_runs ?? []).map((r) => ({
        id: r.id ?? "",
        schedule_id: r.schedule_id,
        task_id: r.task_id ?? "",
        status: r.status ?? "?",
        planned_for: r.planned_for ?? "",
        started_at: r.started_at,
        ended_at: r.ended_at,
        label: r.label,
      }));
      setSchedules(sched);
      setTaskRuns(runs);
      setCached("calendar", { schedules: sched, taskRuns: runs });
    } catch {
      setSchedules([]);
      setTaskRuns([]);
    } finally {
      setCalendarLoading(false);
    }
  }, []);

  const fetchCalendarGridEvents = useCallback(async () => {
    const d = calendarGridDate;
    let from: Date;
    let to: Date;
    if (calendarGridView === "day") {
      from = new Date(d.getFullYear(), d.getMonth(), d.getDate(), 0, 0, 0);
      to = new Date(d.getFullYear(), d.getMonth(), d.getDate(), 23, 59, 59);
    } else if (calendarGridView === "week") {
      const day = d.getDay();
      const monday = new Date(d);
      monday.setDate(d.getDate() - (day === 0 ? 6 : day - 1));
      from = new Date(monday.getFullYear(), monday.getMonth(), monday.getDate(), 0, 0, 0);
      to = new Date(monday);
      to.setDate(monday.getDate() + 6);
      to.setHours(23, 59, 59, 999);
    } else {
      from = new Date(d.getFullYear(), d.getMonth(), 1, 0, 0, 0);
      to = new Date(d.getFullYear(), d.getMonth() + 1, 0, 23, 59, 59);
    }
    try {
      const data = await invoke<{ events?: Array<{ at: string; task_id: string; type: string; status: string; label?: string }> }>("get_calendar_events", {
        port: DAEMON_PORT,
        from: from.toISOString(),
        to: to.toISOString(),
      });
      setCalendarGridEvents(data?.events ?? []);
    } catch {
      setCalendarGridEvents([]);
    }
  }, [calendarGridView, calendarGridDate]);

  useEffect(() => {
    if (tab === "calendar") fetchCalendarGridEvents();
  }, [tab, fetchCalendarGridEvents]);

  useEffect(() => {
    if (tab !== "tasks") return;
    const cached = getCached<Array<{ id: string; status: string }>>("tasks");
    if (cached != null) {
      setTasksList(cached);
      setTasksSelected((prev) => (prev >= cached.length && cached.length > 0 ? cached.length - 1 : prev));
      setTasksLoading(false);
      return;
    }
    fetchTasksList();
  }, [tab, fetchTasksList]);

  useEffect(() => {
    const task = tasksList[tasksSelected];
    if (task?.id) fetchTasksEvents(task.id);
    else setTasksEvents([]);
  }, [tasksList, tasksSelected, fetchTasksEvents]);

  useEffect(() => {
    if (tab !== "calendar") return;
    const cached = getCached<{ schedules: Array<{ id: string; name: string; enabled: boolean; interval_seconds?: number }>; taskRuns: Array<{ id: string; schedule_id?: string; task_id: string; status: string; planned_for: string; started_at?: string; ended_at?: string; label?: string }> }>("calendar");
    if (cached != null) {
      setSchedules(cached.schedules);
      setTaskRuns(cached.taskRuns);
      setCalendarLoading(false);
      return;
    }
    fetchCalendar();
  }, [tab, fetchCalendar]);

  const fetchMemory = useCallback(async () => {
    setMemoryLoading(true);
    setMemoryError(null);
    try {
      const [shortRes, longRes] = await Promise.all([
        invoke<{ session_id?: string; turns?: Array<{ role: string; content: string }> }>("get_memory_short_term", {
          sessionId: sessionId ?? undefined,
          port: DAEMON_PORT,
        }),
        invoke<{ entries?: Array<{ id?: string; content: string; created_at: string; source: string }>; long_term_available?: boolean }>("get_memory_long_term", {
          limit: 50,
          port: DAEMON_PORT,
        }),
      ]);
      const short = shortRes?.turns ?? [];
      const long = longRes?.entries ?? [];
      setMemoryShortTerm(short);
      setMemoryLongTerm(long);
      setMemoryLongTermAvailable(longRes?.long_term_available ?? false);
      setCached("memory", { short, long, longTermAvailable: longRes?.long_term_available ?? false });
    } catch (e) {
      setMemoryError(String(e));
      setMemoryShortTerm([]);
      setMemoryLongTerm([]);
      setMemoryLongTermAvailable(false);
    } finally {
      setMemoryLoading(false);
    }
  }, [sessionId]);

  useEffect(() => {
    if (tab !== "memory") return;
    const cached = getCached<{ short: Array<{ role: string; content: string }>; long: Array<{ id?: string; content: string; created_at: string; source: string }>; longTermAvailable: boolean }>("memory");
    if (cached != null) {
      setMemoryShortTerm(cached.short);
      setMemoryLongTerm(cached.long);
      setMemoryLongTermAvailable(cached.longTermAvailable);
      setMemoryLoading(false);
      setMemoryError(null);
      return;
    }
    fetchMemory();
  }, [tab, fetchMemory]);

  const fetchScheduleReports = useCallback(async () => {
    try {
      const data = await invoke<{ reports?: Array<{ schedule_name?: string; message?: string; ended_at?: string }> }>("get_schedule_run_reports", { port: DAEMON_PORT });
      setScheduleReports((data?.reports ?? []).map((r) => ({ schedule_name: r.schedule_name ?? "", message: r.message ?? "Exécuté.", ended_at: r.ended_at })));
    } catch {
      setScheduleReports([]);
    }
  }, []);

  useEffect(() => {
    if (tab === "chat") fetchScheduleReports();
  }, [tab, fetchScheduleReports]);

  const fetchUserRagDocuments = useCallback(async () => {
    setUserRagLoading(true);
    setUserRagError(null);
    try {
      const data = await invoke<{ documents?: Array<{ id?: string; name?: string; mime_type?: string; added_at?: string }> }>(
        "get_user_rag_documents",
        { port: DAEMON_PORT }
      );
      const docs = (data?.documents ?? []).map((d) => ({
        id: d.id ?? "",
        name: d.name ?? "",
        mime_type: d.mime_type ?? "",
        added_at: d.added_at ?? "",
      }));
      setUserRagDocuments(docs);
      setCached("userRag", docs);
    } catch (e) {
      setUserRagError(String(e));
      setUserRagDocuments([]);
    } finally {
      setUserRagLoading(false);
    }
  }, []);

  useEffect(() => {
    if (tab !== "settings") return;
    const cached = getCached<Array<{ id: string; name: string; mime_type: string; added_at: string }>>("userRag");
    if (cached != null) {
      setUserRagDocuments(cached);
      setUserRagLoading(false);
      setUserRagError(null);
      return;
    }
    fetchUserRagDocuments();
  }, [tab, fetchUserRagDocuments]);

  useEffect(() => {
    if (!calendarSelectedTaskId) {
      setCalendarTaskDetail(null);
      return;
    }
    let cancelled = false;
    (async () => {
      try {
        const raw = await invoke<string>("get_task_status", { taskId: calendarSelectedTaskId, port: DAEMON_PORT });
        if (cancelled) return;
        const st = JSON.parse(raw) as { status?: string; created_at?: string; updated_at?: string; progress?: Array<{ progress_pct?: number; message?: string }> };
        setCalendarTaskDetail({
          status: st?.status ?? "?",
          created_at: st?.created_at,
          updated_at: st?.updated_at,
          progress: st?.progress,
        });
      } catch {
        if (!cancelled) setCalendarTaskDetail(null);
      }
    })();
    return () => { cancelled = true; };
  }, [calendarSelectedTaskId]);

  useEffect(() => {
    if (!calendarSelectedTaskId) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setCalendarSelectedTaskId(null);
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [calendarSelectedTaskId]);

  useEffect(() => {
    if (!calendarSelectedScheduleId) {
      setScheduleDetail(null);
      setScheduleDetailError(null);
      return;
    }
    setScheduleDetailError(null);
    let cancelled = false;
    (async () => {
      try {
        const data = await invoke<{ id?: string; name?: string; description?: string; enabled?: boolean; interval_seconds?: number; timezone?: string; rrule?: string; channel_context?: string | null }>("get_schedule_by_id", {
          scheduleId: calendarSelectedScheduleId,
          port: DAEMON_PORT,
        });
        if (cancelled) return;
        setScheduleDetail({
          id: data?.id ?? "",
          name: data?.name ?? "",
          description: data?.description ?? "",
          enabled: data?.enabled ?? false,
          interval_seconds: data?.interval_seconds,
          timezone: data?.timezone,
          rrule: data?.rrule,
          channel_context: data?.channel_context ?? null,
        });
        setScheduleDetailError(null);
      } catch (err) {
        if (!cancelled) {
          setScheduleDetail(null);
          setScheduleDetailError(err instanceof Error ? err.message : String(err));
        }
      }
    })();
    return () => { cancelled = true; };
  }, [calendarSelectedScheduleId]);

  useEffect(() => {
    if (!calendarSelectedScheduleId) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setCalendarSelectedScheduleId(null);
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [calendarSelectedScheduleId]);

  const runSlashCommand = async (input: string): Promise<string> => {
    const parts = input.replace(/^\//, "").trim().split(/\s+/);
    const cmd = parts[0]?.toLowerCase() ?? "";
    const port = DAEMON_PORT;

    if (cmd === "help" || cmd === "?") {
      return `Commandes disponibles:
/help, /?         — cette aide
/task create "msg" — créer une tâche (envoie le message au daemon, comme un message chat)
/schedule create NOM INTERVAL_SEC "description" — créer une récurrence
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
/skills reload     — recharger les skills (data_dir/skills, spec/skills)
/skills uninstall <nom> — désinstaller un skill (ex. /skills uninstall bankr)
/restart          — redémarrer le daemon (superviseur)
/vault set        — utiliser le CLI : akasha vault set KEY [value]`;
    }
    if (cmd === "task") {
      const sub = parts[1]?.toLowerCase() ?? "";
      if (sub === "create") {
        const msg = (parts.slice(2).join(" ").replace(/^"|"$/g, "").trim() || parts[2]?.replace(/^"|"$/g, "")) ?? "";
        if (!msg) return "Usage: /task create \"message\"";
        try {
          const ack = await invoke<{ message?: string; task_id?: string }>("send_message_ack", {
            message: msg,
            sessionId: sessionId,
            port: DAEMON_PORT,
          });
          return (ack?.message ?? "Tâche créée.") + (ack?.task_id ? ` Task #${ack.task_id.slice(-8)}` : "");
        } catch (err) {
          return `Erreur: ${String(err)}`;
        }
      }
      return "Usage: /task create \"message\"";
    }
    if (cmd === "schedule") {
      const sub = parts[1]?.toLowerCase() ?? "";
      if (sub === "create") {
        const name = parts[2] ?? "";
        const intervalSec = parts[3] ? parseInt(parts[3], 10) : 3600;
        const description = (parts.slice(4).join(" ").replace(/^"|"$/g, "").trim() || parts[4]?.replace(/^"|"$/g, "")) ?? "";
        if (!name) return "Usage: /schedule create NOM INTERVAL_SEC \"description\"";
        try {
          const data = await invoke<{ id?: string }>("create_schedule", {
            name,
            description,
            intervalSeconds: isNaN(intervalSec) ? 3600 : intervalSec,
            port: DAEMON_PORT,
          });
          const id = data?.id ?? "?";
          return `Récurrence créée : ${name} (id: ${id.slice(-8)})`;
        } catch (err) {
          return `Erreur: ${String(err)}`;
        }
      }
      if (sub === "delete") {
        const id = parts[2]?.trim();
        if (!id) return "Usage: /schedule delete SCHEDULE_ID";
        try {
          await invoke("delete_schedule", { scheduleId: id, port: DAEMON_PORT });
          return `Récurrence ${id} supprimée.`;
        } catch (err) {
          return `Erreur: ${String(err)}`;
        }
      }
      return "Usage: /schedule create NOM INTERVAL \"desc\" | /schedule delete ID";
    }
    if (cmd === "stop" || cmd === "cancel") {
      const taskId = parts[1]?.trim();
      if (!taskId) return "Usage: /stop TASK_ID ou /cancel TASK_ID (ex: /stop 412e7256-f808-4e83-b371-b7dd9b6fc4f8)";
      try {
        await invoke<{ cancelled?: boolean }>("cancel_task", { task_id: taskId, port: DAEMON_PORT });
        return `Tâche ${taskId.slice(-8)} annulée.`;
      } catch (err) {
        return `Erreur: ${String(err)}`;
      }
    }
    if (cmd === "newsession" || cmd === "nouvelle" || (cmd === "session" && parts[1]?.toLowerCase() === "nouvelle")) {
      return "Nouvelle session demandée. Votre prochain message repartira de zéro (contexte court terme effacé).";
    }
    if (cmd === "status") {
      const r = await invoke<{ ok: boolean }>("check_health", { port });
      return r?.ok ? "Daemon : OK" : "Daemon : déconnecté ou erreur";
    }
    if (cmd === "doctor") {
      const doc = await invoke<{ ok: boolean; checks: Array<{ id: string; ok: boolean; description: string }> }>("get_doctor", { port });
      if (!doc?.checks?.length) return "Impossible de récupérer le diagnostic.";
      const lines = doc.checks.map((c) => `  [${c.ok ? "OK" : "KO"}] ${c.id} — ${c.description}`);
      return `Doctor — diagnostic\n${lines.join("\n")}\n\n${doc.ok ? "Tous les checks sont OK." : "Certains checks ont échoué."}`;
    }
    if (cmd === "advice") {
      const doc = await invoke("get_doctor", { port });
      const adviceResp = await invoke<{ advice?: string; model_used?: string }>("get_advice", { health: doc, port });
      const advice = (adviceResp?.advice ?? "").trim();
      const model = adviceResp?.model_used ?? "?";
      if (!advice) return `(Aucun conseil retourné. Modèle utilisé : ${model}.)`;
      return `Conseil diagnostic (modèle: ${model})\n\n${advice}`;
    }
    if (cmd === "embedded") {
      const sub = parts[1]?.toLowerCase() ?? "";
      if (sub === "reload") {
        try {
          const json = await invoke<{ message?: string }>("embedded_reload", { port });
          return json?.message ?? "Modèle déchargé.";
        } catch {
          return "Impossible de recharger (daemon ou routeur).";
        }
      }
      try {
        const json = await invoke<{ embedded_available?: boolean; embedded_loaded?: boolean; hint?: string }>("get_embedded_status", { port });
        const available = json?.embedded_available ?? false;
        const loaded = json?.embedded_loaded ?? false;
        const hint = json?.hint ?? "";
        const status = !available ? "non disponible" : loaded ? "disponible et chargé (prêt)" : "disponible (chargement au 1ᵉʳ appel, 5–15 min possibles)";
        return `Modèle embarqué : ${status}\n${hint}`;
      } catch {
        return "Impossible de joindre le daemon ou routeur.";
      }
    }
    if (cmd === "plugins") {
      const list = await invoke<Array<{ id?: string; name?: string; version?: string }>>("get_plugins", { port });
      if (!list?.length) return "Aucun plugin installé.";
      return list.map((p) => `${p.id ?? "?"} — ${p.name ?? "?"} (${p.version ?? "?"})`).join("\n");
    }
    if (cmd === "reload") {
      await invoke("reload_plugins", { port });
      return "Plugins rechargés.";
    }
    if (cmd === "skills") {
      const sub = parts[1]?.toLowerCase() ?? "";
      if (sub === "reload") {
        try {
          const json = await invoke<{ reloaded?: boolean; count?: number }>("reload_skills", { port });
          const count = json?.count ?? 0;
          return json?.reloaded ? `Skills rechargés (${count} skill(s)).` : `Erreur rechargement skills.`;
        } catch {
          return "Impossible de recharger les skills (daemon déconnecté ou erreur).";
        }
      }
      if (sub === "uninstall") {
        const skillName = parts[2]?.trim();
        if (!skillName) return "Usage: /skills uninstall <nom> (ex. /skills uninstall bankr)";
        try {
          const json = await invoke<{ uninstalled?: boolean; name?: string; message?: string }>("uninstall_skill", {
            name: skillName,
            port,
          });
          return json?.uninstalled ? (json?.message ?? `Skill « ${skillName} » désinstallé.`) : (json?.message ?? "Erreur désinstallation.");
        } catch (err) {
          return `Impossible de désinstaller le skill : ${String(err)}`;
        }
      }
      return "Usage: /skills reload — recharger les skills ; /skills uninstall <nom> — désinstaller un skill.";
    }
    if (cmd === "metrics") {
      const data = await invoke<Record<string, ModelMetricsEntry>>("get_router_metrics", { port });
      if (!data || Object.keys(data).length === 0) return "Aucune métrique.";
      return Object.entries(data)
        .map(
          ([model, m]) =>
            `${model} : ${m.total_requests} requêtes (${m.successful_requests} ok, ${m.failed_requests} échecs), ${m.total_latency_ms} ms`
        )
        .join("\n");
    }
    function formatRoutes(
      routes: Record<
        string,
        {
          primary?: { provider?: string; model?: string };
          fallback?: Array<{ provider?: string; model?: string }>;
        }
      >
    ): string {
      const lines: string[] = ["Modèles par catégorie (primary + fallback)\n"];
      for (const cat of Object.keys(routes).sort()) {
        const t = routes[cat];
        const primary = t?.primary
          ? `${t.primary.provider ?? "?"} / ${t.primary.model ?? "?"}`
          : "(aucun)";
        lines.push(`  ${cat}:`);
        lines.push(`    primary: ${primary}`);
        const fallback = t?.fallback ?? [];
        if (fallback.length === 0) {
          lines.push("    fallback: (aucun)");
        } else {
          fallback.forEach((f, i) =>
            lines.push(`    fallback[${i}]: ${f?.provider ?? "?"} / ${f?.model ?? "?"}`)
          );
        }
      }
      return lines.join("\n");
    }
    if (cmd === "models") {
      const sub = parts[1]?.toLowerCase() ?? "";
      if (sub === "list") {
        const routes = await invoke<Record<string, { primary?: { provider?: string; model?: string }; fallback?: Array<{ provider?: string; model?: string }> }>>("get_router_routes", { port });
        if (!routes || Object.keys(routes).length === 0) return "Aucune route configurée.";
        return formatRoutes(routes);
      }
      if (sub === "set") {
        const category = parts[2];
        const provider = parts[3];
        const model = parts.slice(4).join(" ")?.trim() ?? "";
        if (!category || !provider || !model) return "Usage: /models set CATÉGORIE PROVIDER MODÈLE (ex. /models set conversation ollama llama3.2)";
        try {
          const json = await invoke<{ message?: string; category?: string }>("set_router_route", { category, provider, model, port });
          return `${json?.category ?? ""} — ${json?.message ?? "Route mise à jour."}`;
        } catch (err) {
          return `Erreur: ${String(err)}`;
        }
      }
      const providers = await invoke<Record<string, string[]>>("get_router_models", { port });
      if (!providers || Object.keys(providers).length === 0) return "Aucun modèle configuré.";
      const lines: string[] = [];
      for (const [provider, models] of Object.entries(providers)) {
        if (models?.length) {
          lines.push(`${provider}:`);
          lines.push(...models.map((m) => `  ${m}`));
        }
      }
      return lines.length ? lines.join("\n") : "Aucun modèle listé.";
    }
    if (cmd === "routes") {
      const routes = await invoke<
        Record<
          string,
          {
            primary?: { provider?: string; model?: string };
            fallback?: Array<{ provider?: string; model?: string }>;
          }
        >
      >("get_router_routes", { port });
      if (!routes || Object.keys(routes).length === 0) return "Aucune route configurée.";
      return formatRoutes(routes);
    }
    if (cmd === "config") {
      const sub = parts[1]?.toLowerCase() ?? "";
      if (sub === "list") {
        const vars = await invoke<Record<string, string>>("get_config", { port });
        if (!vars || Object.keys(vars).length === 0) return "Aucune variable.";
        return Object.entries(vars)
          .map(([k, v]) => `${k}=${v}`)
          .join("\n");
      }
      if (sub === "get") {
        const key = parts[2];
        if (!key) return "Usage: /config get KEY";
        const vars = await invoke<Record<string, string>>("get_config", { port });
        const val = vars?.[key];
        return val ?? "Clé non trouvée.";
      }
      if (sub === "set") {
        const key = parts[2];
        const value = parts.slice(3).join(" ") || "";
        if (!key) return "Usage: /config set KEY value";
        await invoke("set_config", { key, value, port });
        return `Variable ${key} définie.`;
      }
      return "Usage: /config list | get KEY | set KEY value";
    }
    if (cmd === "vault") {
      const sub = parts[1]?.toLowerCase() ?? "";
      if (sub === "list") {
        const keys = await invoke<string[]>("get_vault_keys", { port });
        return keys?.length ? keys.join("\n") : "Vault vide.";
      }
      if (sub === "set") return "Utilisez le CLI : akasha vault set KEY [value]";
      return "Usage: /vault list";
    }
    if (cmd === "restart") {
      await invoke("restart_daemon", { port });
      return "Redémarrage demandé (le daemon va se fermer).";
    }
    if (!cmd) return "Tapez /help pour les commandes.";
    return `Commande inconnue: /${cmd}. Tapez /help.`;
  };

  const readFileAsBase64 = (file: File): Promise<{ content_base64: string; mime_type: string }> => {
    return new Promise((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => {
        const dataUrl = reader.result as string;
        const match = dataUrl.match(/^data:([^;]+);base64,(.+)$/);
        if (match) {
          resolve({ content_base64: match[2], mime_type: match[1] });
        } else {
          reject(new Error("Invalid data URL"));
        }
      };
      reader.onerror = () => reject(reader.error);
      reader.readAsDataURL(file);
    });
  };

  const onAttachFiles = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const files = e.target.files;
    if (!files?.length) return;
    const imageMimes = ["image/png", "image/jpeg", "image/webp", "image/gif"];
    for (let i = 0; i < files.length; i++) {
      const file = files[i];
      try {
        const { content_base64, mime_type } = await readFileAsBase64(file);
        const typ = imageMimes.includes(mime_type) ? "image" : "document";
        setAttachments((prev) => [
          ...prev,
          { id: `${Date.now()}-${i}-${file.name}`, name: file.name, typ, content_base64, mime_type },
        ]);
      } catch (err) {
        console.error("Failed to read file", file.name, err);
      }
    }
    e.target.value = "";
  };

  const handleSend = async () => {
    const hasContent = message.trim() || attachments.length > 0;
    if (!hasContent || loading) return;

    const userMessage = message.trim() || "(Pièce(s) jointe(s))";
    setMessages((prev) => [...prev, { role: "user", text: userMessage }]);
    setMessage("");
    chatInputRef.current?.focus();

    if (userMessage.startsWith("/")) {
      const cmdLower = userMessage.replace(/^\//, "").trim().toLowerCase().split(/\s+/)[0] ?? "";
      if (cmdLower === "newsession" || cmdLower === "nouvelle" || (cmdLower === "session" && userMessage.toLowerCase().includes("nouvelle"))) {
        setSessionId(null);
      }
      setLoading(true);
      try {
        const result = await runSlashCommand(userMessage);
        setMessages((prev) => [...prev, { role: "system", text: result }]);
      } catch (err) {
        setMessages((prev) => [...prev, { role: "assistant", text: `Erreur : ${String(err)}`, error: true }]);
      } finally {
        setLoading(false);
      }
      return;
    }

    // Non-blocking: ACK + task_id, then poll in background (FR-025)
    const attachmentsPayload = attachments.length > 0
      ? attachments.map((a) => ({ type: a.typ, name: a.name, content_base64: a.content_base64, mime_type: a.mime_type }))
      : undefined;
    setAttachments([]);
    setLoading(true);
    try {
      const ack = await invoke<{ task_id: string; session_id: string; message: string }>("send_message_ack", {
        message: userMessage,
        session_id: sessionId,
        attachments: attachmentsPayload,
        port: DAEMON_PORT,
      });
      setLoading(false);
      if (ack?.session_id) setSessionId(ack.session_id);
      const ackText = ack?.message ?? "Je prends en compte votre demande.";
      setMessages((prev) => [...prev, { role: "assistant", text: ackText + (ack?.task_id ? " Tu peux suivre l'avancement dans Tâches." : "") }]);
      if (ack?.task_id) {
        setRunningTaskChips((prev) => ({ ...prev, [ack.task_id]: { pct: 0, message: "en cours…" } }));
        setRunningTaskEvents((prev) => ({ ...prev, [ack.task_id]: [] }));
        setSubAgentPanelCollapsed(false);
        fetchTasksList();
        const taskId = ack.task_id;
        const pollUntilDone = async () => {
          const maxWait = 600;
          const MIN_INTERVAL = 1500;
          const MAX_INTERVAL = 5000;
          let pollIntervalMs = MIN_INTERVAL;
          let ticksWithoutChange = 0;
          let lastStatus = "";
          let lastMsg = "";
          for (let i = 0; i < maxWait; i++) {
            await new Promise((r) => setTimeout(r, pollIntervalMs));
            try {
              const [raw, eventsData, humanInputData] = await Promise.all([
                invoke<string>("get_task_status", { taskId, port: DAEMON_PORT }),
                invoke<{ events?: Array<{ event_type?: string; payload?: unknown; at?: string; task_id?: string }> }>("get_task_events", { taskId, port: DAEMON_PORT }).catch(() => ({ events: [] })),
                invoke<{ question?: string; context?: string; choices?: string[] }>("get_task_human_input", { taskId, port: DAEMON_PORT }).catch(() => null),
              ]);
              const status = JSON.parse(raw) as { status?: string; progress?: Array<{ progress_pct?: number; message?: string }> };
              const pct = status?.progress?.slice(-1)[0]?.progress_pct ?? 0;
              const msg = status?.progress?.slice(-1)[0]?.message ?? "";
              const currentStatus = status?.status ?? "";
              if (currentStatus === lastStatus && msg === lastMsg) {
                ticksWithoutChange++;
                if (ticksWithoutChange >= 4 && pollIntervalMs < MAX_INTERVAL) {
                  pollIntervalMs = Math.min(pollIntervalMs + 1500, MAX_INTERVAL);
                  ticksWithoutChange = 0;
                }
              } else {
                lastStatus = currentStatus;
                lastMsg = msg;
                ticksWithoutChange = 0;
                pollIntervalMs = MIN_INTERVAL;
              }
              setRunningTaskChips((prev) => (prev[taskId] !== undefined ? { ...prev, [taskId]: { pct, message: msg } } : prev));
              const events = (eventsData?.events ?? []).map((e) => ({
                event_type: e.event_type ?? "?",
                payload: e.payload,
                at: e.at ?? "",
                task_id: e.task_id,
              }));
              setRunningTaskEvents((prev) => (prev[taskId] !== undefined ? { ...prev, [taskId]: events } : prev));
              if (humanInputData?.question) {
                setPendingHumanInput((prev) => ({ ...prev, [taskId]: { question: humanInputData.question ?? "", context: humanInputData.context ?? "", choices: humanInputData.choices } }));
                if (!humanInputAutoOpenedRef.current.has(taskId)) {
                  humanInputAutoOpenedRef.current.add(taskId);
                  setHumanInputModalTaskId(taskId);
                }
              } else {
                setPendingHumanInput((prev) => {
                  const next = { ...prev };
                  delete next[taskId];
                  return next;
                });
                humanInputAutoOpenedRef.current.delete(taskId);
              }
              if (status?.status === "completed") {
                setRunningTaskChips((prev) => { const next = { ...prev }; delete next[taskId]; return next; });
                setRunningTaskEvents((prev) => { const next = { ...prev }; delete next[taskId]; return next; });
                setPendingHumanInput((prev) => { const next = { ...prev }; delete next[taskId]; return next; });
                humanInputAutoOpenedRef.current.delete(taskId);
                setHumanInputModalTaskId((c) => (c === taskId ? null : c));
                const finalMsg = status?.progress?.slice(-1)[0]?.message ?? "Terminé.";
                setMessages((prev) => [...prev, { role: "assistant", text: finalMsg }]);
                return;
              }
              if (status?.status === "failed") {
                setRunningTaskChips((prev) => { const next = { ...prev }; delete next[taskId]; return next; });
                setRunningTaskEvents((prev) => { const next = { ...prev }; delete next[taskId]; return next; });
                setPendingHumanInput((prev) => { const next = { ...prev }; delete next[taskId]; return next; });
                humanInputAutoOpenedRef.current.delete(taskId);
                setHumanInputModalTaskId((c) => (c === taskId ? null : c));
                setMessages((prev) => [...prev, { role: "assistant", text: "Tâche en échec.", error: true }]);
                return;
              }
            } catch {
              /* ignore */
            }
          }
          setRunningTaskChips((prev) => {
            const next = { ...prev };
            delete next[taskId];
            return next;
          });
          setRunningTaskEvents((prev) => {
            const next = { ...prev };
            delete next[taskId];
            return next;
          });
          setMessages((prev) => [...prev, { role: "assistant", text: "Délai dépassé. Consultez Tâches." }]);
        };
        pollUntilDone();
      }
    } catch (err) {
      setLoading(false);
      setMessages((prev) => [...prev, { role: "assistant", text: `Erreur : ${String(err)}`, error: true }]);
    }
    chatInputRef.current?.focus();
  };

  return (
    <div className="app">
      <a href="#main-content" className="skip-link">Aller au contenu principal</a>
      {updateBannerInfo && (
        <div className="update-banner" role="region" aria-label="Mise à jour disponible">
          <div className="update-banner-inner">
            <p className="update-banner-text">
              {t("update.available")} <strong>{updateBannerInfo.remote_version}</strong> ({t("update.you_have")} {updateBannerInfo.current_version}). {t("update.banner_install")}
            </p>
            <div className="update-banner-actions">
              <button
                type="button"
                className="update-banner-download-btn"
                onClick={async () => {
                  try {
                    await invoke("open_url", { url: updateBannerInfo!.download_url });
                  } catch (e) {
                    console.error(e);
                  }
                }}
              >
                {t("update.download")}
              </button>
              <button
                type="button"
                className="update-banner-dismiss-btn"
                onClick={() => setUpdateBannerInfo(null)}
              >
                {t("update.later")}
              </button>
            </div>
            <details className="update-banner-steps">
              <summary>{t("update.steps_title")}</summary>
              <ol>
                <li>{t("update.step1")}</li>
                <li>{t("update.step2")}</li>
                <li>{t("update.step3")}</li>
              </ol>
            </details>
          </div>
        </div>
      )}
      <header className="header">
        <h1 className="logo">Akasha</h1>
        <p className="tagline">Local-first AI assistant · 1–7 : onglets</p>
        <div className="daemon-status" role="status" aria-live="polite">
          <span
            className={`status-dot ${health?.ok ? "connected" : "disconnected"}`}
            aria-hidden
          />
          {health?.ok ? (
            <span>Daemon connecté (port {health.port ?? DAEMON_PORT})</span>
          ) : (
            <span>Daemon déconnecté — lancez <code>akasha start</code></span>
          )}
        </div>
        {Object.keys(pendingHumanInput).length > 0 && (
          <div ref={pendingNotifRef} className="header-pending-actions" role="region" aria-label={t("pending_actions.region_label")}>
            <button
              type="button"
              className="header-pending-actions-trigger"
              onClick={() => setPendingNotifOpen((o) => !o)}
              aria-expanded={pendingNotifOpen}
              aria-haspopup="true"
              title={t("pending_actions.title")}
            >
              <span className="header-pending-actions-icon" aria-hidden>⚠</span>
              <span className="header-pending-actions-badge">{Object.keys(pendingHumanInput).length}</span>
              <span className="header-pending-actions-label">{t("pending_actions.action_required")}</span>
            </button>
            {pendingNotifOpen && (
              <div className="header-pending-actions-dropdown" role="menu">
                <p className="header-pending-actions-dropdown-title">{t("pending_actions.agents")}</p>
                {Object.entries(pendingHumanInput).map(([taskId, p]) => (
                  <div key={taskId} className="header-pending-actions-item">
                    <p className="header-pending-actions-item-question" title={p.question}>
                      {p.question.slice(0, 80)}{p.question.length > 80 ? "…" : ""}
                    </p>
                    <p className="header-pending-actions-item-task">Tâche #{taskId.slice(-8)}</p>
                    <button
                      type="button"
                      className="header-pending-actions-item-btn"
                      onClick={() => {
                        setHumanInputModalTaskId(taskId);
                        setHumanInputFreeText("");
                        setPendingNotifOpen(false);
                      }}
                    >
                      {t("human_input.reply")}
                    </button>
                  </div>
                ))}
              </div>
            )}
          </div>
        )}
        <nav className="tabs" role="tablist" aria-label="Sections">
          <button
            role="tab"
            aria-selected={tab === "chat"}
            aria-controls="panel-chat"
            id="tab-chat"
            className={tab === "chat" ? "active" : ""}
            onClick={() => setTab("chat")}
          >
{t("tabs.chat")}
            </button>
            <button
            role="tab"
            aria-selected={tab === "router"}
            aria-controls="panel-router"
            id="tab-router"
            className={tab === "router" ? "active" : ""}
            onClick={() => setTab("router")}
          >
{t("tabs.router")}
            </button>
            <button
            role="tab"
            aria-selected={tab === "docs"}
            aria-controls="panel-docs"
            id="tab-docs"
            className={tab === "docs" ? "active" : ""}
            onClick={() => setTab("docs")}
          >
            {t("tabs.docs")}
          </button>
          <button
            role="tab"
            aria-selected={tab === "tasks"}
            aria-controls="panel-tasks"
            id="tab-tasks"
            className={tab === "tasks" ? "active" : ""}
            onClick={() => setTab("tasks")}
          >
{t("tabs.tasks")}
            </button>
            <button
            role="tab"
            aria-selected={tab === "calendar"}
            aria-controls="panel-calendar"
            id="tab-calendar"
            className={tab === "calendar" ? "active" : ""}
            onClick={() => setTab("calendar")}
          >
{t("tabs.calendar")}
            </button>
            <button
            role="tab"
            aria-selected={tab === "memory"}
            aria-controls="panel-memory"
            id="tab-memory"
            className={tab === "memory" ? "active" : ""}
            onClick={() => setTab("memory")}
          >
{t("tabs.memory")}
            </button>
            <button
            role="tab"
            aria-selected={tab === "settings"}
            aria-controls="panel-settings"
            id="tab-settings"
            className={tab === "settings" ? "active" : ""}
            onClick={() => setTab("settings")}
          >
{t("tabs.settings")}
            </button>
        </nav>
      </header>

      <main className="main" id="main-content" tabIndex={-1}>
        {tab === "chat" && (
          <section
            id="panel-chat"
            role="tabpanel"
            aria-labelledby="tab-chat"
            className="panel chat-panel"
          >
            <div className="chat-area">
              {messages.length === 0 ? (
                <div className="chat-placeholder">
                  <p>Écrivez un message pour commencer.</p>
                  <p>
                    Commandes <code>/</code> : <code>/help</code> pour l’aide, <code>/status</code>, <code>/config list</code>, etc.
                  </p>
                  <p>
                    Daemon : <code>akasha start</code>
                  </p>
                </div>
              ) : (
                <>
                  {scheduleReports.map((r, i) => (
                    <div key={`report-${i}`} className="message system report">
                      <span className="role" aria-hidden>Rappel exécuté</span>
                      <div className="text markdown-rendered">
                        <Suspense fallback={<span className="markdown-rendered">…</span>}><LazyMarkdownContent>
                          {`**« ${r.schedule_name} »** — ${r.message}`}
                        </LazyMarkdownContent></Suspense>
                      </div>
                    </div>
                  ))}
                  {messages.map((m, i) => {
                    const askUserData = m.role === "assistant" ? parseAskUserMessage(m.text) : null;
                    return (
                      <div
                        key={i}
                        className={`message ${m.role} ${m.error ? "error" : ""} ${askUserData ? "message-ask-user" : ""}`}
                      >
                        <span className="role" aria-hidden>
                          {m.role === "user" ? "Vous" : m.role === "system" ? "Système" : "Akasha"}
                        </span>
                        {m.role === "system" ? (
                          <div className="text system-text" style={{ whiteSpace: "pre-wrap" }}>
                            {m.text}
                          </div>
                        ) : askUserData ? (
                          <div className="message-ask-user-card">
                            <div className="message-ask-user-question markdown-rendered">
                              <Suspense fallback={<span className="markdown-rendered">…</span>}><LazyMarkdownContent>
                                {askUserData.question}
                              </LazyMarkdownContent></Suspense>
                            </div>
                            {askUserData.context && (
                              <p className="message-ask-user-context">{askUserData.context}</p>
                            )}
                            {askUserData.choices?.length ? (
                              <div className="message-ask-user-choices">
                                {askUserData.choices.map((choice, j) => (
                                  <span key={j} className="message-ask-user-choice-tag">
                                    {choice}
                                  </span>
                                ))}
                              </div>
                            ) : null}
                            <p className="message-ask-user-hint">Répondre dans le formulaire sous le chat ou via « Action requise » sur la tâche.</p>
                          </div>
                        ) : (
                          <div className="text markdown-rendered">
                            <Suspense fallback={<span className="markdown-rendered">…</span>}><LazyMarkdownContent>
                              {m.text}
                            </LazyMarkdownContent></Suspense>
                          </div>
                        )}
                      </div>
                    );
                  })}
                </>
              )}
              {(loading || Object.keys(runningTaskChips).length > 0) && (
                <div className="chat-loading-row" role="status" aria-live="polite">
                  {loading && (
                    <div className="chat-loader" aria-hidden>
                      <span className="chat-loader-spinner" />
                      <span>{t("chat.sending")}</span>
                    </div>
                  )}
                  {Object.keys(runningTaskChips).length > 0 && (
                    <div className="chat-chips">
                      {Object.entries(runningTaskChips).map(([tid, { pct, message }]) => {
                        const chipAskUser = parseAskUserMessage(message ?? "");
                        const chipLabel = chipAskUser
                          ? "Question en attente — répondez ci‑dessous"
                          : (message ?? "en cours");
                        return (
                        <span key={tid} className="task-chip">
                          <span className="task-chip-spinner" aria-hidden />
                          Task #{tid.slice(-8)} {pct != null ? `(${pct}%)` : ""} {chipLabel}
                          {pendingHumanInput[tid] && (
                            <button
                              type="button"
                              className="task-chip-action-required"
                              onClick={() => { setHumanInputModalTaskId(tid); setHumanInputFreeText(""); }}
                              title="Une action de votre part est requise"
                            >
                              ⚠ Action requise
                            </button>
                          )}
                        </span>
                        );
                      })}
                    </div>
                  )}
                </div>
              )}
              {Object.keys(runningTaskChips).length > 0 && (
                <div className="chat-subagents-panel">
                  <button
                    type="button"
                    className="chat-subagents-toggle"
                    onClick={() => setSubAgentPanelCollapsed((c) => !c)}
                    aria-expanded={!subAgentPanelCollapsed}
                    aria-controls="subagents-detail"
                  >
                    <span className="chat-subagents-toggle-icon" aria-hidden>{subAgentPanelCollapsed ? "▶" : "▼"}</span>
                    <span>
                      {subAgentPanelCollapsed
                        ? (() => {
                            const total = Object.values(runningTaskEvents).flat().length;
                            return total > 0
                              ? `Détail des sous-agents (${total} étape(s))`
                              : "Détail des sous-agents (cliquez pour afficher)";
                          })()
                        : "Masquer le détail des sous-agents"}
                    </span>
                  </button>
                  {!subAgentPanelCollapsed && (
                    <div id="subagents-detail" className="chat-subagents-detail" role="region" aria-label="Actions des sous-agents">
                      {Object.entries(runningTaskEvents).filter(([, ev]) => ev.length > 0).length === 0 ? (
                        <p className="chat-subagents-empty">
                          {t("chat.no_events_yet")}
                          </p>
                      ) : (
                        Object.entries(runningTaskEvents).map(([rootTaskId, events]) => {
                          if (events.length === 0) return null;
                          const isCollapsed = collapsedRootTasks[rootTaskId] ?? false;
                          const chip = runningTaskChips[rootTaskId];
                          const pct = chip?.pct ?? 0;
                          return (
                            <div key={rootTaskId} className="chat-subagents-discussion">
                              <button
                                type="button"
                                className="chat-subagents-discussion-toggle"
                                onClick={() => setCollapsedRootTasks((prev) => ({ ...prev, [rootTaskId]: !prev[rootTaskId] }))}
                                aria-expanded={!isCollapsed}
                                aria-controls={`subagents-discussion-${rootTaskId}`}
                              >
                                <span className="chat-subagents-discussion-icon" aria-hidden>{isCollapsed ? "▶" : "▼"}</span>
                                <span className="chat-subagents-discussion-label">
                                  Discussion — Task #{rootTaskId.slice(-8)}
                                  {pct != null && pct < 100 ? ` (${pct}%)` : ""}
                                </span>
                              </button>
                              {!isCollapsed && (
                                <div id={`subagents-discussion-${rootTaskId}`} className="chat-subagents-discussion-body">
                                  {(() => {
                                    const byTask: Record<string, typeof events> = {};
                                    for (const ev of events) {
                                      const tid = ev.task_id ?? rootTaskId;
                                      if (!byTask[tid]) byTask[tid] = [];
                                      byTask[tid].push(ev);
                                    }
                                    return Object.entries(byTask).map(([tid, evs]) => (
                                      <div key={`${rootTaskId}-${tid}`} className="chat-subagents-task">
                                        <div className="chat-subagents-task-id">
                                          {tid === rootTaskId ? `${t("chat.root_task")}${tid.slice(-8)}` : `${t("chat.sub_task")}${tid.slice(-8)}`}
                                        </div>
                                        <ul className="chat-subagents-events">
                                          {evs.map((ev, idx) => (
                                            <li key={`${tid}-${idx}`} className="chat-subagents-event" data-type={ev.event_type}>
                                              <span className="chat-subagents-event-type">{eventLabel(ev.event_type)}</span>
                                              {ev.payload && typeof ev.payload === "object" && "agent" in ev.payload ? (
                                                <span className="chat-subagents-event-agent"> → {String((ev.payload as { agent?: string }).agent ?? "")}</span>
                                              ) : null}
                                              {ev.at && <span className="chat-subagents-event-at"> {ev.at.slice(0, 19)}</span>}
                                            </li>
                                          ))}
                                        </ul>
                                      </div>
                                    ));
                                  })()}
                                </div>
                              )}
                            </div>
                          );
                        })
                      )}
                    </div>
                  )}
                </div>
              )}
              <div ref={chatEndRef} aria-hidden />
            </div>
            {Object.keys(pendingHumanInput).length > 0 && !humanInputModalTaskId && (
              <div className="chat-human-input-banner" role="status">
                Une question vous attend — répondez ci-dessous ou cliquez sur « Action requise » sur la tâche.
              </div>
            )}
            {Object.keys(pendingHumanInput).length > 0 && !humanInputModalTaskId && (() => {
              const pendingTaskId = Object.keys(pendingHumanInput)[0];
              const pending = pendingTaskId ? pendingHumanInput[pendingTaskId] : null;
              if (!pending || !pendingTaskId) return null;
              return (
                <div className="chat-inline-human-reply" role="form" aria-labelledby="inline-reply-label">
                  <h3 id="inline-reply-label" className="chat-inline-human-reply-title">Répondre à l&apos;agent</h3>
                  <p className="chat-inline-human-reply-question">{pending.question}</p>
                  {pending.context && <p className="chat-inline-human-reply-context">{pending.context}</p>}
                  {pending.choices?.length ? (
                    <div className="chat-inline-human-reply-choices">
                      {pending.choices.map((choice, i) => (
                        <button
                          key={i}
                          type="button"
                          className="chat-inline-human-reply-choice-btn"
                          onClick={async () => {
                            try {
                              await invoke("post_task_human_reply", { taskId: pendingTaskId, response: choice, port: DAEMON_PORT });
                              setPendingHumanInput((prev) => { const next = { ...prev }; delete next[pendingTaskId]; return next; });
                              setHumanInputModalTaskId((c) => (c === pendingTaskId ? null : c));
                            } catch (e) {
                              console.error(e);
                            }
                          }}
                        >
                          {choice}
                        </button>
                      ))}
                    </div>
                  ) : (
                    <div className="chat-inline-human-reply-free">
                      <label htmlFor="inline-human-reply-input" className="sr-only">Votre réponse</label>
                      <input
                        id="inline-human-reply-input"
                        type="text"
                        value={inlineHumanReplyText}
                        onChange={(e) => setInlineHumanReplyText(e.target.value)}
                        placeholder="Saisissez votre réponse…"
                        onKeyDown={(e) => e.key === "Enter" && document.getElementById("inline-human-reply-submit")?.click()}
                      />
                      <button
                        id="inline-human-reply-submit"
                        type="button"
                        onClick={async () => {
                          const text = inlineHumanReplyText.trim();
                          if (!text) return;
                          try {
                            await invoke("post_task_human_reply", { taskId: pendingTaskId, response: text, port: DAEMON_PORT });
                            setPendingHumanInput((prev) => { const next = { ...prev }; delete next[pendingTaskId]; return next; });
                            setHumanInputModalTaskId((c) => (c === pendingTaskId ? null : c));
                            setInlineHumanReplyText("");
                          } catch (e) {
                            console.error(e);
                          }
                        }}
                      >
                        Envoyer la réponse
                      </button>
                    </div>
                  )}
                </div>
              );
            })()}
            {attachments.length > 0 && (
              <div className="chat-attachments">
                {attachments.map((a) => (
                  <span key={a.id} className="chat-attachment-chip">
                    {a.name}
                    <button
                      type="button"
                      aria-label={`Retirer ${a.name}`}
                      onClick={() => setAttachments((prev) => prev.filter((x) => x.id !== a.id))}
                    >
                      ×
                    </button>
                  </span>
                ))}
              </div>
            )}
            <div className="input-area">
              <input
                ref={fileInputRef}
                type="file"
                multiple
                accept="image/*,.txt,.md,.pdf,.csv"
                onChange={onAttachFiles}
                className="sr-only"
                aria-hidden
              />
              <button
                type="button"
                onClick={() => fileInputRef.current?.click()}
                aria-label="Joindre un fichier"
                title="Joindre une image ou un document"
              >
                Joindre
              </button>
              <label htmlFor="chat-input" className="sr-only">
                Votre message
              </label>
              <input
                ref={chatInputRef}
                id="chat-input"
                type="text"
                value={message}
                onChange={(e) => setMessage(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && handleSend()}
                placeholder="Votre message…"
                disabled={loading}
                aria-describedby="send-hint"
              />
              <button
                onClick={handleSend}
                disabled={loading || (!message.trim() && attachments.length === 0)}
                aria-label="Envoyer le message"
              >
                Envoyer
              </button>
            </div>
            <p id="send-hint" className="hint sr-only">
              Entrée pour envoyer
            </p>
            {humanInputModalTaskId && pendingHumanInput[humanInputModalTaskId] && (
              <div className="human-input-overlay" role="dialog" aria-labelledby="human-input-title" aria-modal="true">
                <div className="human-input-modal">
                  <h2 id="human-input-title">Action requise</h2>
                  <p className="human-input-question">{pendingHumanInput[humanInputModalTaskId].question}</p>
                  {pendingHumanInput[humanInputModalTaskId].context && (
                    <p className="human-input-context">{pendingHumanInput[humanInputModalTaskId].context}</p>
                  )}
                  {pendingHumanInput[humanInputModalTaskId].choices?.length ? (
                    <div className="human-input-choices">
                      {pendingHumanInput[humanInputModalTaskId].choices!.map((choice, i) => (
                        <button
                          key={i}
                          type="button"
                          className="human-input-choice-btn"
                          onClick={async () => {
                            try {
                              await invoke("post_task_human_reply", { taskId: humanInputModalTaskId, response: choice, port: DAEMON_PORT });
                              setPendingHumanInput((prev) => { const next = { ...prev }; delete next[humanInputModalTaskId!]; return next; });
                              setHumanInputModalTaskId(null);
                            } catch (e) {
                              console.error(e);
                            }
                          }}
                        >
                          {choice}
                        </button>
                      ))}
                    </div>
                  ) : (
                    <div className="human-input-free">
                      <label htmlFor="human-input-text">Votre réponse</label>
                      <input
                        id="human-input-text"
                        type="text"
                        value={humanInputFreeText}
                        onChange={(e) => setHumanInputFreeText(e.target.value)}
                        placeholder="Saisissez votre réponse…"
                        onKeyDown={(e) => e.key === "Enter" && document.getElementById("human-input-submit")?.click()}
                      />
                      <button
                        id="human-input-submit"
                        type="button"
                        onClick={async () => {
                          const text = humanInputFreeText.trim();
                          if (!text) return;
                          try {
                            await invoke("post_task_human_reply", { taskId: humanInputModalTaskId, response: text, port: DAEMON_PORT });
                            setPendingHumanInput((prev) => { const next = { ...prev }; delete next[humanInputModalTaskId!]; return next; });
                            setHumanInputModalTaskId(null);
                            setHumanInputFreeText("");
                          } catch (e) {
                            console.error(e);
                          }
                        }}
                      >
                        Envoyer
                      </button>
                    </div>
                  )}
                  <button type="button" className="human-input-close" onClick={() => setHumanInputModalTaskId(null)} aria-label="Fermer">
                    Fermer
                  </button>
                </div>
              </div>
            )}
          </section>
        )}

        {tab === "router" && (
          <section
            id="panel-router"
            role="tabpanel"
            aria-labelledby="tab-router"
            className="panel router-panel"
          >
            <h2 className="panel-title">Métriques du routeur LLM</h2>
            {routerLoading && (
              <p className="panel-loading" aria-busy="true">
                <span className="panel-loading-spinner" aria-hidden />
                {t("common.loading")}
              </p>
            )}
            {routerError && (
              <div className="error-banner" role="alert">
                {routerError}
              </div>
            )}
            {!routerLoading && !routerError && routerMetrics && (
              <>
                <div className="router-metrics-toolbar">
                  <label htmlFor="router-metrics-period" className="router-metrics-period-label">
                    Période :
                  </label>
                  <select
                    id="router-metrics-period"
                    value={routerMetricsPeriod}
                    onChange={(e) => setRouterMetricsPeriod(e.target.value as MetricsPeriod)}
                    className="router-metrics-period-select"
                    aria-label="Filtrer les métriques par période"
                  >
                    <option value="all">Toutes</option>
                    <option value="day">Jour</option>
                    <option value="week">Semaine</option>
                    <option value="month">Mois</option>
                    <option value="year">Année</option>
                  </select>
                  <button
                    type="button"
                    className="refresh-btn"
                    onClick={fetchRouterMetrics}
                    aria-label="Rafraîchir les métriques"
                  >
                    Rafraîchir
                  </button>
                </div>
                {Object.keys(routerMetrics).length === 0 ? (
                  <p className="empty-state">
                    {t("chat.empty")}
                    pour générer des métriques.
                  </p>
                ) : (
                  <div className="metrics-table-wrap">
                    <table className="metrics-table" role="table">
                      <thead>
                        <tr>
                          <th scope="col">Modèle</th>
                          <th scope="col">Requêtes</th>
                          <th scope="col">Réussies</th>
                          <th scope="col">Échecs</th>
                          <th scope="col">Latence (ms)</th>
                          <th scope="col">Tokens</th>
                          <th scope="col">Fallbacks</th>
                        </tr>
                      </thead>
                      <tbody>
                        {Object.entries(routerMetrics).map(([key, m]) => (
                          <tr key={key}>
                            <td data-label="Modèle">{key}</td>
                            <td data-label="Requêtes">{m.total_requests}</td>
                            <td data-label="Réussies">
                              {m.successful_requests}
                            </td>
                            <td data-label="Échecs">{m.failed_requests}</td>
                            <td data-label="Latence (ms)">
                              {m.total_latency_ms}
                            </td>
                            <td data-label="Tokens">{m.total_tokens}</td>
                            <td data-label="Fallbacks">
                              {m.fallback_triggered} / {m.fallback_success}
                            </td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </div>
                )}
              </>
            )}
          </section>
        )}

        {tab === "docs" && (
          <section
            id="panel-docs"
            role="tabpanel"
            aria-labelledby="tab-docs"
            className="panel docs-panel"
          >
            <h2 className="panel-title">{t("docs.title")}</h2>
            {docLoading && (
              <p className="panel-loading" aria-busy="true">
                <span className="panel-loading-spinner" aria-hidden />
                {t("common.loading")}
              </p>
            )}
            {docError && (
              <div className="error-banner" role="alert">
                {docError}
                <p>Assurez-vous que le daemon est démarré (<code>akasha start</code>).</p>
              </div>
            )}
            {!docLoading && !docError && docContent && (
              <>
                <button
                  type="button"
                  className="refresh-btn"
                  onClick={fetchDocs}
                  aria-label="Rafraîchir la documentation"
                >
                  Rafraîchir
                </button>
                <div className="doc-content doc-markdown">
                  <Suspense fallback={<span className="markdown-rendered">…</span>}><LazyMarkdownContent>
                    {docContent}
                  </LazyMarkdownContent></Suspense>
                </div>
              </>
            )}
          </section>
        )}

        {tab === "tasks" && (
          <section
            id="panel-tasks"
            role="tabpanel"
            aria-labelledby="tab-tasks"
            className="panel activity-panel"
          >
            <h2 className="panel-title">{t("tasks.title")}</h2>
            <button
              type="button"
              className="refresh-btn"
              onClick={fetchTasksList}
              aria-label="Rafraîchir l’activité"
              disabled={tasksLoading}
            >
              Rafraîchir
            </button>
            {tasksLoading && (
              <p className="panel-loading" aria-busy="true">
                <span className="panel-loading-spinner" aria-hidden />
                {t("common.loading")}
              </p>
            )}
            {!tasksLoading && (
              <div className="activity-panel-body">
                <div className="activity-tasks-block">
                  <h3>Liste des tâches</h3>
                  {tasksList.length === 0 ? (
                    <p className="empty-state">{t("tasks.empty")}</p>
                  ) : (
                    <ul className="activity-task-list" role="list">
                      {tasksList.map((t, i) => (
                        <li
                          key={t.id}
                          className={i === tasksSelected ? "selected" : ""}
                          role="button"
                          tabIndex={0}
                          onClick={() => setTasksSelected(i)}
                          onKeyDown={(e) => {
                            if (e.key === "Enter" || e.key === " ") {
                              e.preventDefault();
                              setTasksSelected(i);
                            }
                            if (e.key === "ArrowDown" && i < tasksList.length - 1)
                              setTasksSelected(i + 1);
                            if (e.key === "ArrowUp" && i > 0) setTasksSelected(i - 1);
                          }}
                        >
                          <span className="task-id">{t.id.slice(-8)}</span>{" "}
                          <span className="task-status">{t.status}</span>
                        </li>
                      ))}
                    </ul>
                  )}
                </div>
                <div className="activity-events-block">
                  <h3>Événements</h3>
                  {tasksEvents.length === 0 ? (
                    <p className="empty-state">
                      {tasksList.length > 0 ? t("tasks.no_events") : t("tasks.select_task")}
                    </p>
                  ) : (
                    <ul className="activity-events-list" role="list">
                      {tasksEvents.map((e, i) => (
                        <li key={i}>
                          <strong>{eventLabel(e.event_type)}</strong> @ {e.at}
                          {e.payload != null && (
                            <pre className="event-payload">{JSON.stringify(e.payload, null, 2)}</pre>
                          )}
                        </li>
                      ))}
                    </ul>
                  )}
                </div>
              </div>
            )}
          </section>
        )}

        {tab === "calendar" && (
          <section
            id="panel-calendar"
            role="tabpanel"
            aria-labelledby="tab-calendar"
            className="panel calendar-panel"
          >
            <h2 className="panel-title">{t("calendar.title")}</h2>
            <div className="calendar-subtabs" role="tablist" aria-label={t("calendar.subtabs_label")}>
              <button
                type="button"
                role="tab"
                aria-selected={calendarSubTab === "grid"}
                className={calendarSubTab === "grid" ? "active" : ""}
                onClick={() => setCalendarSubTab("grid")}
              >
                Vue calendrier
              </button>
              <button
                type="button"
                role="tab"
                aria-selected={calendarSubTab === "recent"}
                className={calendarSubTab === "recent" ? "active" : ""}
                onClick={() => setCalendarSubTab("recent")}
              >
                {t("calendar.recent")}
              </button>
              <button
                type="button"
                role="tab"
                aria-selected={calendarSubTab === "schedules"}
                className={calendarSubTab === "schedules" ? "active" : ""}
                onClick={() => setCalendarSubTab("schedules")}
              >
                {t("calendar.recurring")}
              </button>
            </div>
            <button
              type="button"
              className="refresh-btn"
              onClick={fetchCalendar}
              aria-label="Rafraîchir le calendrier"
              disabled={calendarLoading}
            >
              Rafraîchir
            </button>
            {calendarLoading && (
              <p className="loading-inline" aria-busy="true">
                {t("common.loading")}
              </p>
            )}
            {!calendarLoading && calendarSubTab === "grid" && (
              <>
                <h3 className="calendar-grid-header">Vue calendrier (tâches lancées)</h3>
                <div className="calendar-grid-toolbar">
                  <select
                    value={calendarGridView}
                    onChange={(e) => { setCalendarGridView(e.target.value as CalendarGridView); setCalendarGridDate(new Date()); }}
                    className="calendar-grid-select"
                    aria-label="Vue"
                  >
                    <option value="day">Jour (par heure)</option>
                    <option value="week">Semaine (par jour)</option>
                    <option value="month">Mois (par jour)</option>
                  </select>
                  <input
                    type="date"
                    value={calendarGridDate.toISOString().slice(0, 10)}
                    onChange={(e) => setCalendarGridDate(new Date(e.target.value + "T12:00:00"))}
                    className="calendar-grid-date"
                    aria-label="Date"
                  />
                  <button type="button" className="refresh-btn calendar-grid-refresh" onClick={fetchCalendarGridEvents} aria-label="Rafraîchir">Rafraîchir</button>
                </div>
                <div className="calendar-grid-wrap">
                  {(() => {
                    const events = calendarGridEvents;
                    const eventLabel = (ev: { label?: string; task_id: string }) =>
                      (ev.label && ev.label.trim()) ? ev.label : `Tâche …${ev.task_id.slice(-8)}`;
                    if (calendarGridView === "day") {
                      const byHour: Record<number, typeof events> = {};
                      for (let h = 0; h < 24; h++) byHour[h] = [];
                      events.forEach((ev) => {
                        const date = new Date(ev.at);
                        const h = date.getHours();
                        byHour[h].push(ev);
                      });
                      return (
                        <table className="calendar-grid-table calendar-grid-day" role="grid" aria-label="Calendrier jour">
                          <thead>
                            <tr>
                              <th scope="col" className="calendar-grid-col-time">Heure</th>
                              <th scope="col" className="calendar-grid-col-events">Événements</th>
                            </tr>
                          </thead>
                          <tbody>
                            {Array.from({ length: 24 }, (_, h) => (
                              <tr key={h} className="calendar-grid-row">
                                <td className="calendar-grid-cell-time">{h}h00</td>
                                <td className="calendar-grid-cell-events">
                                  <ul className="calendar-grid-slot-events" role="list">
                                    {byHour[h].map((e, i) => (
                                      <li key={i} className="calendar-event-block" title={`${e.type} — ${e.status}`}>
                                        <span className="calendar-event-label">{eventLabel(e)}</span>
                                        <span className="calendar-event-time">{new Date(e.at).toLocaleTimeString("fr-FR", { hour: "2-digit", minute: "2-digit" })}</span>
                                      </li>
                                    ))}
                                  </ul>
                                </td>
                              </tr>
                            ))}
                          </tbody>
                        </table>
                      );
                    }
                    if (calendarGridView === "week") {
                      const d = calendarGridDate;
                      const day = d.getDay();
                      const monday = new Date(d);
                      monday.setDate(d.getDate() - (day === 0 ? 6 : day - 1));
                      const byDay: Record<string, typeof events> = {};
                      const dayKeys: string[] = [];
                      for (let i = 0; i < 7; i++) {
                        const date = new Date(monday);
                        date.setDate(monday.getDate() + i);
                        const key = date.toISOString().slice(0, 10);
                        byDay[key] = [];
                        dayKeys.push(key);
                      }
                      events.forEach((ev) => {
                        const key = new Date(ev.at).toISOString().slice(0, 10);
                        if (byDay[key]) byDay[key].push(ev);
                      });
                      const weekDayNames = ["Lun", "Mar", "Mer", "Jeu", "Ven", "Sam", "Dim"];
                      return (
                        <table className="calendar-grid-table calendar-grid-week" role="grid" aria-label={t("calendar.grid_week")}>
                          <thead>
                            <tr>
                              <th scope="col" className="calendar-grid-col-hour">Heure</th>
                              {dayKeys.map((key) => {
                                const dayNum = new Date(key + "T12:00:00").getDay();
                                const nameIndex = dayNum === 0 ? 6 : dayNum - 1;
                                return (
                                  <th key={key} scope="col" className="calendar-grid-col-day">
                                    {weekDayNames[nameIndex]}
                                    <br />
                                    <span className="calendar-grid-day-num">{new Date(key + "T12:00:00").getDate()}</span>
                                  </th>
                                );
                              })}
                            </tr>
                          </thead>
                          <tbody>
                            {Array.from({ length: 24 }, (_, hour) => (
                              <tr key={hour} className="calendar-grid-row">
                                <td className="calendar-grid-cell-hour">{hour}h</td>
                                {dayKeys.map((key) => (
                                  <td key={key} className="calendar-grid-cell-day">
                                    <ul className="calendar-grid-slot-events" role="list">
                                      {(byDay[key] ?? []).filter((e) => new Date(e.at).getHours() === hour).map((e, i) => (
                                        <li key={i} className="calendar-event-block" title={`${e.type} — ${e.status}`}>
                                          <span className="calendar-event-label">{eventLabel(e)}</span>
                                          <span className="calendar-event-time">{new Date(e.at).toLocaleTimeString("fr-FR", { hour: "2-digit", minute: "2-digit" })}</span>
                                        </li>
                                      ))}
                                    </ul>
                                  </td>
                                ))}
                              </tr>
                            ))}
                          </tbody>
                        </table>
                      );
                    }
                    const byDay: Record<string, typeof events> = {};
                    events.forEach((ev) => {
                      const key = new Date(ev.at).toISOString().slice(0, 10);
                      if (!byDay[key]) byDay[key] = [];
                      byDay[key].push(ev);
                    });
                    const d = calendarGridDate;
                    const firstDay = new Date(d.getFullYear(), d.getMonth(), 1);
                    const lastDay = new Date(d.getFullYear(), d.getMonth() + 1, 0);
                    const startWeekday = firstDay.getDay() === 0 ? 6 : firstDay.getDay() - 1;
                    const daysInMonth = lastDay.getDate();
                    const weeks: string[][] = [];
                    let week: string[] = [];
                    for (let i = 0; i < startWeekday; i++) week.push("");
                    for (let day = 1; day <= daysInMonth; day++) {
                      const date = new Date(d.getFullYear(), d.getMonth(), day);
                      week.push(date.toISOString().slice(0, 10));
                      if (week.length === 7) {
                        weeks.push(week);
                        week = [];
                      }
                    }
                    if (week.length) {
                      while (week.length < 7) week.push("");
                      weeks.push(week);
                    }
                    const weekDayNames = ["Lun", "Mar", "Mer", "Jeu", "Ven", "Sam", "Dim"];
                    return (
                      <table className="calendar-grid-table calendar-grid-month" role="grid" aria-label="Calendrier mois">
                        <thead>
                          <tr>
                            {weekDayNames.map((wd) => (
                              <th key={wd} scope="col" className="calendar-grid-col-weekday">{wd}</th>
                            ))}
                          </tr>
                        </thead>
                        <tbody>
                          {weeks.map((weekRow, wi) => (
                            <tr key={wi} className="calendar-grid-row">
                              {weekRow.map((key, di) => (
                                <td key={`${wi}-${di}`} className="calendar-grid-cell-month">
                                  {key ? (
                                    <>
                                      <span className="calendar-grid-day-num">{new Date(key + "T12:00:00").getDate()}</span>
                                      <ul className="calendar-grid-slot-events" role="list">
                                        {(byDay[key] ?? []).map((e, i) => (
                                          <li key={i} className="calendar-event-block" title={`${e.type} — ${e.status}`}>
                                            <span className="calendar-event-label">{eventLabel(e)}</span>
                                            <span className="calendar-event-time">{new Date(e.at).toLocaleTimeString("fr-FR", { hour: "2-digit", minute: "2-digit" })}</span>
                                          </li>
                                        ))}
                                      </ul>
                                    </>
                                  ) : null}
                                </td>
                              ))}
                            </tr>
                          ))}
                        </tbody>
                      </table>
                    );
                  })()}
                </div>
              </>
            )}
            {!calendarLoading && calendarSubTab === "recent" && (
              <div className="calendar-recent-panel">
                <h3 className="calendar-runs-header">Lancements récents</h3>
                {taskRuns.length === 0 ? (
                  <p className="empty-state">{t("calendar.no_runs")}</p>
                ) : (
                  <div className="calendar-runs-list-wrap">
                    {(() => {
                      const byParent = new Map<string, typeof taskRuns>();
                      for (const r of taskRuns) {
                        const key = r.schedule_id ?? "__none__";
                        if (!byParent.has(key)) byParent.set(key, []);
                        byParent.get(key)!.push(r);
                      }
                      const groups: Array<{ key: string; label: string; runs: typeof taskRuns }> = [];
                      byParent.forEach((runs, key) => {
                        const label = key === "__none__" ? "Sans récurrence" : (schedules.find((s) => s.id === key)?.name || key.slice(0, 8));
                        groups.push({ key, label, runs });
                      });
                      const runLabel = (r: { label?: string; task_id: string }) => (r.label && r.label.trim()) ? r.label : `Tâche …${r.task_id.slice(-8)}`;
                      return (
                        <ul className="calendar-runs-list" role="list">
                          {groups.map(({ key, label, runs }) => (
                            <li key={key} className="calendar-runs-group">
                              <div className="calendar-runs-group-label">{label}</div>
                              {runs.map((r) => {
                                const endedAt = r.ended_at ? new Date(r.ended_at) : null;
                                const startedAt = r.started_at ? new Date(r.started_at) : null;
                                const durationSec = endedAt && startedAt ? (endedAt.getTime() - startedAt.getTime()) / 1000 : null;
                                return (
                                  <div
                                    key={r.id}
                                    role="button"
                                    tabIndex={0}
                                    onClick={() => setCalendarSelectedTaskId(r.task_id)}
                                    onKeyDown={(e) => {
                                      if (e.key === "Enter" || e.key === " ") {
                                        e.preventDefault();
                                        setCalendarSelectedTaskId(r.task_id);
                                      }
                                    }}
                                    className={`calendar-run-item ${calendarSelectedTaskId === r.task_id ? "selected" : ""}`}
                                  >
                                    <strong className="calendar-run-item-title">{runLabel(r)}</strong>
                                    <span className="run-id">#{r.id.slice(-8)}</span> — {r.status}
                                    {r.planned_for && (
                                      <> — prévu: {new Date(r.planned_for).toLocaleString()}</>
                                    )}
                                    {endedAt && (
                                      <div className="run-meta">
                                        Terminé à {endedAt.toLocaleString()}
                                        {durationSec != null && durationSec > 0 && ` · Durée: ${formatDurationSec(durationSec)}`}
                                      </div>
                                    )}
                                    <div className="run-meta">task: {r.task_id.slice(-8)}</div>
                                  </div>
                                );
                              })}
                            </li>
                          ))}
                        </ul>
                      );
                    })()}
                  </div>
                )}
              </div>
            )}
            {!calendarLoading && calendarSubTab === "schedules" && (
              <div className="calendar-schedules-panel">
                <h3>{t("calendar.recurring")}</h3>
                {schedules.length === 0 ? (
                  <p className="empty-state">{t("calendar.no_schedules")}</p>
                ) : (
                  <ul className="calendar-schedule-list" role="list">
                    {schedules.map((s) => (
                      <li
                        key={s.id}
                        role="button"
                        tabIndex={0}
                        onClick={() => setCalendarSelectedScheduleId(s.id)}
                        onKeyDown={(e) => {
                          if (e.key === "Enter" || e.key === " ") {
                            e.preventDefault();
                            setCalendarSelectedScheduleId(s.id);
                          }
                        }}
                      >
                        <strong>{s.name || s.id.slice(0, 8)}</strong>{" "}
                        {s.enabled ? "(activée)" : "(en pause)"}
                        {s.interval_seconds != null && ` — toutes les ${s.interval_seconds}s`}
                      </li>
                    ))}
                  </ul>
                )}
              </div>
            )}
            {calendarSelectedTaskId && (
                  <div
                    className="calendar-detail-modal-overlay"
                    role="dialog"
                    aria-modal="true"
                    aria-labelledby="calendar-detail-title"
                    onClick={() => setCalendarSelectedTaskId(null)}
                  >
                    <div
                      className="calendar-detail-modal"
                      onClick={(e) => e.stopPropagation()}
                    >
                      <div className="calendar-detail-modal-header">
                        <h2 id="calendar-detail-title">Détail tâche {calendarSelectedTaskId.slice(-8)}</h2>
                        <button
                          type="button"
                          className="calendar-detail-modal-close"
                          onClick={() => setCalendarSelectedTaskId(null)}
                          aria-label="Fermer"
                        >
                          ×
                        </button>
                      </div>
                      <div className="calendar-detail-modal-body">
                        {calendarTaskDetail ? (
                          <>
                            {(() => {
                              const run = taskRuns.find((r) => r.task_id === calendarSelectedTaskId);
                              const runStatus = run?.status ?? "?";
                              const taskStatus = calendarTaskDetail.status;
                              const runDone = runStatus === "completed" || runStatus === "failed" || runStatus === "cancelled" || runStatus === "skipped";
                              const statusLabel = runStatus === "completed" ? "Terminé" : runStatus === "failed" ? "Échec" : runStatus === "cancelled" ? "Annulé" : runStatus === "running" ? "En cours" : runStatus === "queued" ? "En attente" : runStatus;
                              return (
                                <>
                                  <p><strong>Exécution planifiée (run):</strong> {statusLabel}</p>
                                  {runDone && taskStatus === "running" && (
                                    <p className="task-detail-hint">
                                      Le run est marqué « {statusLabel} » mais la tâche côté orchestrateur affiche encore « running ». Cela peut indiquer un décalage de mise à jour ou une tâche bloquée.
                                    </p>
                                  )}
                                  <p><strong>Statut tâche (orchestrateur):</strong> {taskStatus}</p>
                                </>
                              );
                            })()}
                            {(() => {
                              const run = taskRuns.find((r) => r.task_id === calendarSelectedTaskId);
                              const parent = run?.schedule_id ? schedules.find((s) => s.id === run.schedule_id) : null;
                              return parent ? <p><strong>{t("calendar.parent_schedule")}</strong> {parent.name || parent.id.slice(0, 8)}</p> : null;
                            })()}
                            {calendarTaskDetail.updated_at && (
                              <p><strong>Dernière mise à jour:</strong> {new Date(calendarTaskDetail.updated_at).toLocaleString()}</p>
                            )}
                            {(() => {
                              const run = taskRuns.find((r) => r.task_id === calendarSelectedTaskId);
                              return run?.ended_at ? <p><strong>Terminé à:</strong> {new Date(run.ended_at).toLocaleString()}</p> : null;
                            })()}
                            {(() => {
                              const run = taskRuns.find((r) => r.task_id === calendarSelectedTaskId);
                              if (!run?.started_at || !run?.ended_at) return null;
                              const sec = (new Date(run.ended_at).getTime() - new Date(run.started_at).getTime()) / 1000;
                              return sec > 0 ? <p><strong>Durée:</strong> {formatDurationSec(sec)}</p> : null;
                            })()}
                            {calendarTaskDetail.progress && calendarTaskDetail.progress.length > 0 && (() => {
                              const last = calendarTaskDetail.progress[calendarTaskDetail.progress.length - 1]?.message;
                              return last ? (
                                <div className="task-detail-reply">
                                  <strong>Réponse de l'agent:</strong>
                                  <div className="task-detail-reply-content markdown-rendered">
                                    <Suspense fallback={<span className="markdown-rendered">…</span>}><LazyMarkdownContent>{last}</LazyMarkdownContent></Suspense>
                                  </div>
                                </div>
                              ) : null;
                            })()}
                            {calendarTaskDetail.progress && calendarTaskDetail.progress.length > 0 && (
                              <div className="task-detail-progress">
                                <strong>{t("calendar.progression_steps")}</strong>
                                <p className="task-detail-progress-hint">
                                  Les lignes « État » sont des étapes intermédiaires ; le pourcentage indique l’avancement.
                                </p>
                                <ul>
                                  {calendarTaskDetail.progress.map((p, i) => (
                                    <li key={i}>
                                      {p.progress_pct != null && p.progress_pct > 0
                                        ? `${p.progress_pct}% — `
                                        : "État: "}
                                      <div className="markdown-rendered progress-message">
                                        <Suspense fallback={<span className="markdown-rendered">…</span>}><LazyMarkdownContent>
                                          {p.message ?? ""}
                                        </LazyMarkdownContent></Suspense>
                                      </div>
                                    </li>
                                  ))}
                                </ul>
                              </div>
                            )}
                          </>
                        ) : (
                          <p className="loading-inline">{t("common.loading")}</p>
                        )}
                      </div>
                    </div>
                  </div>
                )}
                {calendarSelectedScheduleId && (
                  <div
                    className="calendar-detail-modal-overlay"
                    role="dialog"
                    aria-modal="true"
                    aria-labelledby="schedule-detail-title"
                    onClick={() => setCalendarSelectedScheduleId(null)}
                  >
                    <div
                      className="calendar-detail-modal"
                      onClick={(e) => e.stopPropagation()}
                    >
                      <div className="calendar-detail-modal-header">
                        <h2 id="schedule-detail-title">Détail récurrence</h2>
                        <button
                          type="button"
                          className="calendar-detail-modal-close"
                          onClick={() => setCalendarSelectedScheduleId(null)}
                          aria-label="Fermer"
                        >
                          ×
                        </button>
                      </div>
                      <div className="calendar-detail-modal-body">
                        {scheduleDetailError ? (
                          <p className="error-inline" role="alert">
                            Impossible de charger le détail : {scheduleDetailError}
                            <br />
                            <small>Vérifiez que le daemon tourne et que l’app est lancée via Tauri (pas uniquement en navigateur).</small>
                          </p>
                        ) : scheduleDetail ? (
                          <>
                            <p><strong>ID (pour supprimer):</strong>{" "}
                              <code className="schedule-id-copy">{scheduleDetail.id}</code>
                              <br />
                              <small className="muted">Commande : /schedule delete {scheduleDetail.id}</small>
                            </p>
                            <p><strong>Nom:</strong> {scheduleDetail.name}</p>
                            <p><strong>État:</strong> {scheduleDetail.enabled ? "Activée" : "En pause"}</p>
                            {scheduleDetail.interval_seconds != null && (
                              <p><strong>Intervalle:</strong> toutes les {scheduleDetail.interval_seconds} s</p>
                            )}
                            {scheduleDetail.timezone && (
                              <p><strong>Fuseau:</strong> {scheduleDetail.timezone}</p>
                            )}
                            {(scheduleDetail.channel_context ?? scheduleDetail.description) ? (
                              <div className="task-detail-reply">
                                <strong>{t("calendar.channel_context")}</strong>
                                <div className="task-detail-reply-content markdown-rendered">
                                  <Suspense fallback={<span className="markdown-rendered">…</span>}><LazyMarkdownContent>
                                    {scheduleDetail.channel_context ?? scheduleDetail.description}
                                  </LazyMarkdownContent></Suspense>
                                </div>
                              </div>
                            ) : (
                              <p className="muted">{t("calendar.no_channel")}</p>
                            )}
                            {scheduleDetail.rrule && (
                              <p className="schedule-rrule"><strong>Règle:</strong> <code>{scheduleDetail.rrule}</code></p>
                            )}
                          </>
                        ) : (
                          <p className="loading-inline">{t("common.loading")}</p>
                        )}
                      </div>
                    </div>
                  </div>
                )}
          </section>
        )}

        {tab === "memory" && (
          <section
            id="panel-memory"
            role="tabpanel"
            aria-labelledby="tab-memory"
            className="panel memory-panel"
          >
            <h2 className="panel-title">{t("memory.title")}</h2>
            <button
              type="button"
              className="refresh-btn"
              onClick={fetchMemory}
              aria-label="Rafraîchir la mémoire"
              disabled={memoryLoading}
            >
              Rafraîchir
            </button>
            {memoryError && (
              <p className="error-inline" role="alert">
                {memoryError}
              </p>
            )}
            {memoryLoading && (
              <p className="panel-loading" aria-busy="true">
                <span className="panel-loading-spinner" aria-hidden />
                {t("common.loading")}
              </p>
            )}
            {!memoryLoading && !memoryError && (
              <div className="memory-content-wrap">
                <div className="memory-subtabs" role="tablist" aria-label="Type de mémoire">
                  <button
                    type="button"
                    role="tab"
                    aria-selected={memorySubTab === "short"}
                    aria-controls="memory-content-short"
                    id="memory-tab-short"
                    className={"memory-subtab" + (memorySubTab === "short" ? " active" : "")}
                    onClick={() => setMemorySubTab("short")}
                  >
                    Court terme
                  </button>
                  <button
                    type="button"
                    role="tab"
                    aria-selected={memorySubTab === "long"}
                    aria-controls="memory-content-long"
                    id="memory-tab-long"
                    className={"memory-subtab" + (memorySubTab === "long" ? " active" : "")}
                    onClick={() => setMemorySubTab("long")}
                  >
                    Long terme
                  </button>
                </div>
                {memorySubTab === "short" && (
                  <div
                    id="memory-content-short"
                    role="tabpanel"
                    aria-labelledby="memory-tab-short"
                    className="memory-subpanel"
                  >
                    <p className="muted">
                  Derniers échanges de la session courante (utilisée par l’orchestrateur pour le contexte).
                </p>
                {memoryShortTerm.length === 0 ? (
                  <p className="empty-state">Aucun tour en mémoire court terme.</p>
                ) : (
                  <div className="memory-list-scroll">
                    <ul className="memory-turns-list">
                      {memoryShortTerm.map((t, i) => (
                        <li key={i} className={"memory-turn memory-turn-" + t.role}>
                          <span className="memory-turn-role">{t.role}</span>
                          <div className="memory-turn-content">{t.content}</div>
                        </li>
                      ))}
                    </ul>
                  </div>
                )}
                  </div>
                )}
                {memorySubTab === "long" && (
                  <div
                    id="memory-content-long"
                    role="tabpanel"
                    aria-labelledby="memory-tab-long"
                    className="memory-subpanel"
                  >
                    {!memoryLongTermAvailable ? (
                      <p className="muted">{t("memory.long_unavailable")}</p>
                    ) : memoryLongTerm.length === 0 ? (
                      <p className="empty-state">{t("memory.long_empty")}</p>
                    ) : (
                      <div className="memory-list-scroll">
                        <ul className="memory-long-term-list">
                          {memoryLongTerm.map((e, i) => (
                      <li key={e.id ?? `entry-${i}`} className="memory-long-term-item">
                        <div className="memory-long-term-body">
                          <div className="memory-long-term-content">{e.content}</div>
                          <div className="memory-long-term-meta">
                            {e.created_at} {e.source ? ` · ${e.source}` : ""}
                          </div>
                        </div>
                        {e.id != null && (
                          <button
                            type="button"
                            className="memory-long-term-delete"
                            onClick={async () => {
                              try {
                                await invoke("delete_memory_long_term", { id: e.id, port: DAEMON_PORT });
                                fetchMemory();
                              } catch (err) {
                                setMemoryError(String(err));
                              }
                            }}
                            aria-label="Supprimer cette entrée"
                            title="Supprimer de la mémoire long terme"
                          >
                            Supprimer
                          </button>
                        )}
                          </li>
                          ))}
                        </ul>
                      </div>
                    )}
                  </div>
                )}
              </div>
            )}
          </section>
        )}

        {tab === "settings" && (
          <section
            id="panel-settings"
            role="tabpanel"
            aria-labelledby="tab-settings"
            className="panel settings-panel"
          >
            <h2 className="panel-title">{t("settings.title")}</h2>
            <dl className="settings-list">
              <dt>{t("settings.theme")}</dt>
              <dd>
                <select
                  aria-label="Choisir le thème d’affichage"
                  className="settings-theme-select"
                  value={theme}
                  onChange={(e) => setThemeAndSave(e.target.value as ThemeId)}
                >
                  {themes.map((th) => (
                    <option key={th.id} value={th.id}>
                      {th.label}
                    </option>
                  ))}
                </select>
                <span className="settings-theme-hint">{t("settings.theme_saved")}</span>
              </dd>
              <dt>{t("settings.daemon_port")}</dt>
              <dd>
                <code>{DAEMON_PORT}</code> ({t("settings.daemon_default")})
              </dd>
              <dt>{t("settings.language")}</dt>
              <dd>
                <select
                  aria-label={t("settings.language")}
                  className="settings-theme-select"
                  value={locale}
                  onChange={(e) => setLocale(e.target.value as "fr" | "en")}
                >
                  <option value="fr">Français</option>
                  <option value="en">English</option>
                </select>
              </dd>
              <dt>{t("settings.data_dir")}</dt>
              <dd>
                <code>%LOCALAPPDATA%\akasha</code> (Windows) ou{" "}
                <code>~/.local/share/akasha</code> (Linux/macOS)
              </dd>
            </dl>
            <h3 className="settings-subtitle">{t("settings.user_rag_title")}</h3>
            <p className="settings-doc muted">
              {t("settings.user_rag_desc")}
            </p>
            {userRagError && (
              <p className="error-inline" role="alert">{userRagError}</p>
            )}
            <input
              ref={userRagFileInputRef}
              type="file"
              accept=".txt,.md,.csv,.json,text/*"
              className="sr-only"
              aria-hidden
              onChange={async (e) => {
                const file = e.target.files?.[0];
                if (!file) return;
                try {
                  const { content_base64, mime_type } = await readFileAsBase64(file);
                  await invoke("add_user_rag_document", {
                    name: file.name,
                    content_base64,
                    mime_type,
                    port: DAEMON_PORT,
                  });
                  fetchUserRagDocuments();
                } catch (err) {
                  setUserRagError(String(err));
                }
                e.target.value = "";
              }}
            />
            <button
              type="button"
              className="refresh-btn"
              onClick={() => userRagFileInputRef.current?.click()}
              disabled={userRagLoading}
            >
              {t("settings.add_document")}
            </button>
            {userRagLoading && <p className="panel-loading" aria-busy="true">{t("common.loading")}</p>}
            {!userRagLoading && userRagDocuments.length === 0 && (
              <p className="empty-state">{t("settings.no_documents")}</p>
            )}
            {!userRagLoading && userRagDocuments.length > 0 && (
              <ul className="settings-doc-list" role="list">
                {userRagDocuments.map((d) => (
                  <li key={d.id} className="settings-doc-item">
                    <span className="settings-doc-name">{d.name}</span>
                    <span className="settings-doc-meta">{d.added_at.slice(0, 10)}</span>
                    <button
                      type="button"
                      className="settings-doc-delete"
                      aria-label={`Supprimer ${d.name}`}
                      onClick={async () => {
                        try {
                          await invoke("delete_user_rag_document", { id: d.id, port: DAEMON_PORT });
                          fetchUserRagDocuments();
                        } catch (err) {
                          setUserRagError(String(err));
                        }
                      }}
                    >
                      {t("settings.delete")}
                    </button>
                  </li>
                ))}
              </ul>
            )}
            <p className="settings-doc">
              {t("settings.config_note")} </p>
          </section>
        )}
      </main>
    </div>
  );
}

export default App;
