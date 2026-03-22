import { useState, useEffect, useCallback, useRef, lazy, Suspense, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import RelationGraph from "relation-graph/react";
import type { RGJsonData, RGOptions, RGNode, RelationGraphComponent } from "relation-graph/react";
import { preprocessDataUrlImages } from "./preprocessDataUrlImages";
import { preprocessMessagePaths } from "./preprocessMessagePaths";
import { getCached, setCached } from "./useTabCache";
import { useI18n } from "./useI18n";

const LazyMarkdownContent = lazy(() => import("./MarkdownContent").then((m) => ({ default: m.default })));

const DAEMON_PORT = 3876;
const THEME_STORAGE_KEY = "akasha_theme";
const AKASHA_SESSION_ID_KEY = "akasha_session_id";

export type ThemeId = "dark_akasha" | "dark" | "dark_nord" | "light" | "light_latte";

const THEME_IDS: ThemeId[] = ["dark_akasha", "dark", "dark_nord", "light", "light_latte"];

type ChatMessageRow = {
  role: "user" | "assistant" | "system";
  text: string;
  error?: boolean;
  streaming?: boolean;
  taskId?: string;
};

function shouldChatStreamProgress(message: string): boolean {
  const m = message?.trim() ?? "";
  if (!m) return false;
  if (m.includes("Analyzing your request") || m.includes("Analyse de votre demande")) return false;
  if (/^\s*TOOL\s*:/im.test(m)) return false;
  if (/\n\s*TOOL\s*:/i.test(m)) return false;
  return true;
}

function isChatStreamToolPhase(message: string): boolean {
  const s = message ?? "";
  return /^\s*TOOL\s*:/im.test(s.trim()) || /\n\s*TOOL\s*:/i.test(s);
}

/** Graph colors per theme (aligned with styles.css [data-theme]) so the memory graph respects dark/light. */
const GRAPH_THEME_COLORS: Record<
  ThemeId,
  {
    backgroundColor: string;
    defaultNodeColor: string;
    defaultNodeFontColor: string;
    defaultNodeBorderColor: string;
    defaultLineColor: string;
    defaultLineWidth: number;
    defaultLineFontColor: string;
    defaultShowLineLabel: boolean;
    checkedLineColor: string;
    /** Per-type node colors for better contrast and distinction (entry, related, selected). */
    nodeType: {
      entry: { color: string; fontColor: string };
      related: { color: string; fontColor: string };
      selected: { color: string; fontColor: string; borderColor: string };
    };
  }
> = {
  dark_akasha: {
    backgroundColor: "#0b0f17",
    defaultNodeColor: "#1a2233",
    defaultNodeFontColor: "#e4e4e7",
    defaultNodeBorderColor: "#2a3448",
    defaultLineColor: "#6b7280",
    defaultLineWidth: 2,
    defaultLineFontColor: "#a1a1aa",
    defaultShowLineLabel: true,
    checkedLineColor: "#7c8cff",
    nodeType: {
      entry: { color: "#1e3a5f", fontColor: "#e4e4e7" },
      related: { color: "#334155", fontColor: "#cbd5e1" },
      selected: { color: "#312e81", fontColor: "#e4e4e7", borderColor: "#7c8cff" },
    },
  },
  dark: {
    backgroundColor: "#0f0f12",
    defaultNodeColor: "#27272a",
    defaultNodeFontColor: "#e4e4e7",
    defaultNodeBorderColor: "#27272a",
    defaultLineColor: "#71717a",
    defaultLineWidth: 2,
    defaultLineFontColor: "#a1a1aa",
    defaultShowLineLabel: true,
    checkedLineColor: "#6366f1",
    nodeType: {
      entry: { color: "#1e1b4b", fontColor: "#e4e4e7" },
      related: { color: "#3f3f46", fontColor: "#d4d4d8" },
      selected: { color: "#312e81", fontColor: "#e4e4e7", borderColor: "#6366f1" },
    },
  },
  dark_nord: {
    backgroundColor: "#2e3440",
    defaultNodeColor: "#434c5e",
    defaultNodeFontColor: "#eceff4",
    defaultNodeBorderColor: "#434c5e",
    defaultLineColor: "#5e6778",
    defaultLineWidth: 2,
    defaultLineFontColor: "#d8dee9",
    defaultShowLineLabel: true,
    checkedLineColor: "#88c0d0",
    nodeType: {
      entry: { color: "#3b4252", fontColor: "#eceff4" },
      related: { color: "#4c566a", fontColor: "#d8dee9" },
      selected: { color: "#2e3440", fontColor: "#eceff4", borderColor: "#88c0d0" },
    },
  },
  light: {
    backgroundColor: "#f4f4f5",
    defaultNodeColor: "#ffffff",
    defaultNodeFontColor: "#18181b",
    defaultNodeBorderColor: "#d4d4d8",
    defaultLineColor: "#71717a",
    defaultLineWidth: 2,
    defaultLineFontColor: "#52525b",
    defaultShowLineLabel: true,
    checkedLineColor: "#4f46e5",
    nodeType: {
      entry: { color: "#dbeafe", fontColor: "#1e3a8a" },
      related: { color: "#f1f5f9", fontColor: "#475569" },
      selected: { color: "#eef2ff", fontColor: "#18181b", borderColor: "#4f46e5" },
    },
  },
  light_latte: {
    backgroundColor: "#eff1f5",
    defaultNodeColor: "#e6e9ef",
    defaultNodeFontColor: "#4c4f69",
    defaultNodeBorderColor: "#bcc0cc",
    defaultLineColor: "#8c8fa1",
    defaultLineWidth: 2,
    defaultLineFontColor: "#6c6f85",
    defaultShowLineLabel: true,
    checkedLineColor: "#8839ef",
    nodeType: {
      entry: { color: "#e6e9ef", fontColor: "#4c4f69" },
      related: { color: "#ccd0da", fontColor: "#5c5f77" },
      selected: { color: "#e0e0ea", fontColor: "#4c4f69", borderColor: "#8839ef" },
    },
  },
};

/** Line color for graph edges (visible on all themes). */
const GRAPH_LINE_COLOR: Record<ThemeId, string> = {
  dark_akasha: "#94a3b8",
  dark: "#94a3b8",
  dark_nord: "#88c0d0",
  light: "#64748b",
  light_latte: "#6c6f85",
};

function loadSavedTheme(): ThemeId {
  try {
    const s = localStorage.getItem(THEME_STORAGE_KEY);
    if (s && THEME_IDS.includes(s as ThemeId)) return s as ThemeId;
  } catch {
    /* ignore */
  }
  return "dark_akasha";
}

type Tab = "chat" | "scheduled" | "router" | "settings" | "docs" | "tasks" | "calendar" | "memory";

type SettingsSection = "display" | "system" | "agent" | "user" | "data";
type AgentProfileSubTab = "identity" | "personality" | "traits" | "rules" | "can_do" | "cannot_do";

const TRAIT_KEYS = ["verbosity", "warmth", "pedagogy", "rigor", "humor", "proactivity", "cautiousness", "initiative"] as const;
const PREFERRED_MODES = ["assistant", "operator", "architect", "onboarding"] as const;

const AGENT_PROFILE_LIMITS = {
  name: 128,
  role: 128,
  personality: 2000,
  ruleLength: 500,
  ruleCount: 30,
  canDoLength: 300,
  canDoCount: 30,
  cannotDoLength: 300,
  cannotDoCount: 30,
} as const;

const AGENT_PROFILE_TEMPLATES: Array<{ label: string; name: string; role?: string; personality: string; rules: string[]; can_do: string[]; cannot_do: string[] }> = [
  { label: "Neutral / versatile — professional, adaptable", name: "Akasha", role: "neutral professional assistant", personality: "You are a neutral, professional assistant. Clear, adaptable tone. Adapt to the request (technical, writing, advice). No superfluous preambles — get to the point.", rules: [], can_do: [], cannot_do: [] },
  { label: "Kind / coach — encouraging, pedagogical", name: "Akasha", role: "kind encouraging assistant", personality: "You are a kind, encouraging assistant (coach style). Explain with pedagogy, rephrase to check understanding. Value progress and suggest clear steps. Stay attentive, non-judgmental. Suggest options rather than imposing one solution.", rules: ["Stay attentive and non-judgmental.", "Suggest options rather than imposing a single solution."], can_do: [], cannot_do: [] },
  { label: "Concise / technical — short, precise, dev & sysadmin", name: "Akasha", role: "concise technical assistant", personality: "You are a concise, technical assistant. Short, precise answers focused on development and system administration. Get to the point: commands, code snippets, paths. No long intros or unnecessary politeness.", rules: ["Prioritize concrete output: commands, code snippets, paths.", "Avoid long introductions."], can_do: [], cannot_do: [] },
  { label: "Creative / writer — free, creative, for writing and ideas", name: "Akasha", role: "creative open-minded assistant", personality: "You are a creative, open-minded assistant. Help structure ideas, write, brainstorm. Offer multiple phrasings or angles. Accept slightly unusual requests. Suggest variants and unexpected directions.", rules: [], can_do: ["Propose rephrasing and variants.", "Suggest complementary angles or ideas."], cannot_do: [] },
  { label: "Strict / security-aware — no code run without confirmation", name: "Akasha", role: "careful security-aware assistant", personality: "You are a careful, security-aware assistant. Explain risks clearly before any action. Never suggest running code or commands without explicit confirmation. Always: what, why, then how. When in doubt about security, warn and suggest a safer alternative.", rules: ["Never run code or commands without explicit user confirmation.", "Always explain « what » and « why » before « how ».", "When in doubt about security, warn and suggest a safer alternative."], can_do: ["Explain and detail steps.", "Propose commands or scripts to copy-paste after confirmation."], cannot_do: ["Run code or commands without confirmation.", "Modify sensitive files without a clear request."] },
  { label: "Joyful & fun — upbeat, light humor", name: "Akasha", role: "joyful fun assistant", personality: "You are a joyful, fun assistant. Upbeat, light humor, emojis when appropriate. Keep responses helpful but entertaining.", rules: [], can_do: [], cannot_do: [] },
  { label: "Friendly advisor — warm, good counsel", name: "Akasha", role: "friendly advisor", personality: "You are a friendly advisor. Warm, good counsel, supportive. Give clear advice while staying approachable.", rules: [], can_do: [], cannot_do: [] },
  { label: "Geek & nerdy — tech-loving, precise", name: "Akasha", role: "geeky nerdy assistant", personality: "You are a geeky, nerdy assistant. Love tech, references, and precise details. Helpful and enthusiastic about technical topics.", rules: [], can_do: [], cannot_do: [] },
];

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
  const [rightSidebarOpen, setRightSidebarOpen] = useState(false);
  const [theme, setTheme] = useState<ThemeId>(loadSavedTheme);
  const [showOnboarding, setShowOnboarding] = useState(() => {
    try {
      return localStorage.getItem("akasha_onboarding_dismissed") !== "1";
    } catch {
      return true;
    }
  });
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
  const [messages, setMessages] = useState<ChatMessageRow[]>([]);
  const [loading, setLoading] = useState(false);
  const [routerMetrics, setRouterMetrics] = useState<RouterMetrics | null>(null);
  const [routerLoading, setRouterLoading] = useState(false);
  const [routerError, setRouterError] = useState<string | null>(null);
  const [docContent, setDocContent] = useState<string | null>(null);
  const [docLoading, setDocLoading] = useState(false);
  const [docError, setDocError] = useState<string | null>(null);
  type TaskListItem = { id: string; status: string; label?: string; created_at?: string; parent_task_id?: string; assigned_agent?: string };
  const [tasksList, setTasksList] = useState<Array<TaskListItem>>([]);
  const [taskListFilter, setTaskListFilter] = useState<"active" | "completed">("active");
  const [taskSearchQuery, setTaskSearchQuery] = useState("");
  const [tasksSelected, setTasksSelected] = useState(0);
  const [tasksEvents, setTasksEvents] = useState<Array<{ event_type: string; payload?: unknown; at: string }>>([]);
  const [tasksLoading, setTasksLoading] = useState(false);
  /** Tâches : sections pliables (liste / étapes / événements), mémorisées localement. */
  const [taskPanelSections, setTaskPanelSections] = useState(() => {
    try {
      const raw = localStorage.getItem("akasha_task_panel_sections");
      if (raw) {
        const j = JSON.parse(raw) as { list?: boolean; steps?: boolean; events?: boolean };
        return {
          list: j.list !== false,
          steps: j.steps !== false,
          events: j.events !== false,
        };
      }
    } catch {
      /* ignore */
    }
    return { list: true, steps: true, events: true };
  });
  const toggleTaskPanelSection = useCallback((key: "list" | "steps" | "events") => {
    setTaskPanelSections((prev) => {
      const next = { ...prev, [key]: !prev[key] };
      try {
        localStorage.setItem("akasha_task_panel_sections", JSON.stringify(next));
      } catch {
        /* ignore */
      }
      return next;
    });
  }, []);
  const [taskStepsTodos, setTaskStepsTodos] = useState<Array<{ id?: string | null; title: string; status: string }>>([]);
  const selectedTaskIdForTodosRef = useRef<string | null>(null);
  const fetchTaskStepsRef = useRef<(taskId: string) => Promise<void>>(async () => {});
  const [runningTaskChips, setRunningTaskChips] = useState<Record<string, { pct?: number; message?: string }>>({});
  /** Events (sub_agent_spawned, progress_update, etc.) per running task for collapsible sub-agent panel. Each event may have task_id (root or child). */
  const [runningTaskEvents, setRunningTaskEvents] = useState<Record<string, Array<{ event_type: string; payload?: unknown; at: string; task_id?: string }>>>({});
  /** Human in the loop: when the agent asks for user input, we store question/context/choices per task_id. */
  const [pendingHumanInput, setPendingHumanInput] = useState<Record<string, { question: string; context: string; choices?: string[] }>>({});
  /** Task id for which the human-input modal is open (null = closed). */
  const [humanInputModalTaskId, setHumanInputModalTaskId] = useState<string | null>(null);
  /** Device bridge: pending request from agent (camera, mic, etc.) for UI to fulfill. */
  const [devicePendingRequest, setDevicePendingRequest] = useState<{
    request_id: string;
    interface: string;
    device_id: string;
    action: string;
    params: unknown;
  } | null>(null);
  /** Voice (TTS/STT): whether STT is configured (show "message vocal" button). */
  const [voiceStatus, setVoiceStatus] = useState<{ stt_configured?: boolean; tts_configured?: boolean } | null>(null);
  /** Voice: recording in progress for message vocal. */
  const [voiceRecording, setVoiceRecording] = useState(false);
  const voiceMediaRecorderRef = useRef<MediaRecorder | null>(null);
  const voiceChunksRef = useRef<Blob[]>([]);
  /** When true, the next completed task reply should be played via TTS (question was sent by voice). */
  const replyWithTtsRef = useRef(false);
  /** Ref to handleSend so handleVoiceMessageToggle can call it without being declared after. */
  const handleSendRef = useRef<(overrideMessage?: string, fromVoice?: boolean) => Promise<void>>(() => Promise.resolve());
  /** Dernière tâche chat : seule elle met à jour la bulle assistant (stream + réponse finale). */
  const lastChatTaskIdRef = useRef<string | null>(null);
  const ackTextByTaskRef = useRef<Record<string, string>>({});
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
  const [calendarTaskDetailError, setCalendarTaskDetailError] = useState<string | null>(null);
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
  const [scheduleEditPrompt, setScheduleEditPrompt] = useState("");
  const [schedulePromptSaving, setSchedulePromptSaving] = useState(false);
    const [calendarSchedulesSelectedForDelete, setCalendarSchedulesSelectedForDelete] = useState<Set<string>>(new Set());
    const [scheduleDeleting, setScheduleDeleting] = useState(false);
    const [_calendarRunsCollapsed, _setCalendarRunsCollapsed] = useState(false);
  type CalendarGridView = "day" | "week" | "month";
  const [calendarGridView, setCalendarGridView] = useState<CalendarGridView>("week");
  type CalendarGridEvent = { at: string; task_id: string; type: string; status: string; label?: string; schedule_id?: string | null };
  const [calendarGridEvents, setCalendarGridEvents] = useState<CalendarGridEvent[]>([]);
  const [calendarGridDate, setCalendarGridDate] = useState(() => new Date());
  const [calendarCellDetail, setCalendarCellDetail] = useState<{ slotKey: string; slotLabel: string; events: CalendarGridEvent[] } | null>(null);
  type CalendarSubTab = "grid" | "recent" | "schedules";
  const [calendarSubTab, setCalendarSubTab] = useState<CalendarSubTab>("grid");
  const calendarEventLabel = (ev: { label?: string; task_id: string }) => (ev.label && ev.label.trim()) ? ev.label : `Tâche …${ev.task_id.slice(-8)}`;
  const calendarGetParentKey = (ev: CalendarGridEvent) => ev.schedule_id ?? `task_${ev.task_id}`;
  const calendarGetEventStatusClass = (status: string) => {
    const s = (status ?? "").toLowerCase();
    if (s === "completed") return "calendar-event--completed";
    if (s === "running") return "calendar-event--running";
    if (s === "queued" || s === "skipped") return "calendar-event--upcoming";
    if (s === "failed" || s === "cancelled") return "calendar-event--failed";
    return "calendar-event--upcoming";
  };
  const calendarDedupeByParent = (list: CalendarGridEvent[]): { representative: CalendarGridEvent; count: number }[] => {
    const byParent = new Map<string, CalendarGridEvent[]>();
    list.forEach((ev) => {
      const key = calendarGetParentKey(ev);
      if (!byParent.has(key)) byParent.set(key, []);
      byParent.get(key)!.push(ev);
    });
    return Array.from(byParent.entries()).map(([, arr]) => {
      const sorted = [...arr].sort((a, b) => new Date(a.at).getTime() - new Date(b.at).getTime());
      return { representative: sorted[0], count: sorted.length };
    });
  };
  // Precompute deduped event groups per slot to avoid per-cell recomputation during render.
  const calendarGridDedupedBySlot = useMemo(() => {
    const getParentKey = (ev: CalendarGridEvent) => ev.schedule_id ?? `task_${ev.task_id}`;
    const dedupe = (evs: CalendarGridEvent[]) => {
      const byParent = new Map<string, CalendarGridEvent[]>();
      evs.forEach((ev) => {
        const key = getParentKey(ev);
        if (!byParent.has(key)) byParent.set(key, []);
        byParent.get(key)!.push(ev);
      });
      return Array.from(byParent.entries()).map(([, arr]) => {
        const sorted = [...arr].sort((a, b) => new Date(a.at).getTime() - new Date(b.at).getTime());
        return { representative: sorted[0], count: sorted.length };
      });
    };
    const byHourSlot = new Map<string, CalendarGridEvent[]>();
    const byDateSlot = new Map<string, CalendarGridEvent[]>();
    calendarGridEvents.forEach((ev) => {
      const d = new Date(ev.at);
      const dateKey = `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
      const hourSlot = `${dateKey}-${d.getHours()}`;
      if (!byHourSlot.has(hourSlot)) byHourSlot.set(hourSlot, []);
      byHourSlot.get(hourSlot)!.push(ev);
      if (!byDateSlot.has(dateKey)) byDateSlot.set(dateKey, []);
      byDateSlot.get(dateKey)!.push(ev);
    });
    const byHour = new Map<string, { representative: CalendarGridEvent; count: number }[]>();
    byHourSlot.forEach((evs, key) => byHour.set(key, dedupe(evs)));
    const byDate = new Map<string, { representative: CalendarGridEvent; count: number }[]>();
    byDateSlot.forEach((evs, key) => byDate.set(key, dedupe(evs)));
    return { byHour, byDate };
  }, [calendarGridEvents]);
  const [memoryShortTerm, setMemoryShortTerm] = useState<Array<{ role: string; content: string }>>([]);
  type MemoryLongTermEntry = { id?: string; content: string; created_at: string; source: string; related?: Array<{ id: string; kind?: string }> };
  const [memoryLongTerm, setMemoryLongTerm] = useState<MemoryLongTermEntry[]>([]);
  const [memoryLongTermTotal, setMemoryLongTermTotal] = useState(0);
  const [memoryLongTermAvailable, setMemoryLongTermAvailable] = useState(false);
  const [memoryLongTermLoadingMore, setMemoryLongTermLoadingMore] = useState(false);
  const [memoryLoading, setMemoryLoading] = useState(false);
  const [memoryError, setMemoryError] = useState<string | null>(null);
  type MemorySubTab = "short" | "long";
  const [memorySubTab, setMemorySubTab] = useState<MemorySubTab>("short");
  const [memorySearchQuery, setMemorySearchQuery] = useState("");
  const [memorySearchResults, setMemorySearchResults] = useState<Array<{ id: string; content: string }>>([]);
  const [memorySearchActive, setMemorySearchActive] = useState(false);
  const [memorySearchLoading, setMemorySearchLoading] = useState(false);
  const [memoryViewGraph, setMemoryViewGraph] = useState(false);
  const [memoryLongTermSelected, setMemoryLongTermSelected] = useState(0);
  const [memoryRebuildLoading, setMemoryRebuildLoading] = useState(false);
  const [memoryRebuildMessage, setMemoryRebuildMessage] = useState<string | null>(null);
  /** When set, show modal with full entry content and "Voir dans la liste" (index >= 0). */
  const [memoryGraphDetail, setMemoryGraphDetail] = useState<{ entry: MemoryLongTermEntry; index: number } | null>(null);
  const memoryGraphRef = useRef<RelationGraphComponent | null>(null);
  /** Track last node click for double-click detection: single = recenter, double = open detail modal. */
  const memoryGraphLastClickRef = useRef<{ nodeId: string; at: number } | null>(null);

  function buildMemoryGraphData(
    entries: MemoryLongTermEntry[],
    selectedIndex: number,
    nodePalette: typeof GRAPH_THEME_COLORS.dark_akasha.nodeType,
    lineColor: string
  ): RGJsonData | null {
    const idToEntry = new Map<string, MemoryLongTermEntry>();
    for (const e of entries) {
      if (e.id) idToEntry.set(e.id, e);
    }
    const nodeIds = new Set<string>(idToEntry.keys());
    for (const e of entries) {
      for (const r of e.related ?? []) {
        nodeIds.add(r.id);
      }
    }
    if (nodeIds.size === 0) return null;
    const selectedEntry = entries[selectedIndex];
    const selectedId = selectedEntry?.id && nodeIds.has(selectedEntry.id) ? selectedEntry.id : null;
    const nodes = Array.from(nodeIds).map((id) => {
      const entry = idToEntry.get(id);
      const text = entry
        ? `${entry.content.slice(0, 40)}${entry.content.length > 40 ? "…" : ""}`
        : id.slice(0, 8);
      const isSelected = id === selectedId;
      const isEntry = !!entry;
      const style = isSelected
        ? nodePalette.selected
        : isEntry
          ? nodePalette.entry
          : nodePalette.related;
      return {
        id,
        text,
        color: style.color,
        fontColor: style.fontColor,
        ...(isSelected && "borderColor" in style
          ? { borderColor: (style as { borderColor: string }).borderColor, borderWidth: 2 }
          : {}),
      };
    });
    const lines: Array<{ id: string; from: string; to: string; text?: string; color?: string; lineWidth?: number }> = [];
    for (const e of entries) {
      if (!e.id) continue;
      for (const r of e.related ?? []) {
        const kind = r.kind ?? "related";
        lines.push({
          id: `${e.id}-${r.id}`,
          from: e.id,
          to: r.id,
          text: kind,
          color: lineColor,
          lineWidth: 2,
        });
      }
    }
    const rootId = selectedId ?? (nodes[0]?.id ?? undefined);
    return { nodes, lines, rootId };
  }

  const [scheduleReports, setScheduleReports] = useState<Array<{ schedule_name: string; message: string; ended_at?: string }>>([]);
  const [sessionId, setSessionId] = useState<string | null>(() => {
    try {
      return localStorage.getItem(AKASHA_SESSION_ID_KEY);
    } catch {
      return null;
    }
  });
  const [userRagDocuments, setUserRagDocuments] = useState<Array<{ id: string; name: string; mime_type: string; added_at: string }>>([]);
  const [userRagLoading, setUserRagLoading] = useState(false);
  const [userRagError, setUserRagError] = useState<string | null>(null);
  const userRagFileInputRef = useRef<HTMLInputElement>(null);
  const agentAvatarFileInputRef = useRef<HTMLInputElement>(null);
  const userAvatarFileInputRef = useRef<HTMLInputElement>(null);
  /** Agent profile (name, role, gender, avatar, personality, rules, can_do, cannot_do, traits_override, preferred_mode) for Settings panel. */
  const [agentProfile, setAgentProfile] = useState<{
    name: string;
    role: string;
    gender: string;
    avatar: string;
    personality: string;
    rules: string[];
    can_do: string[];
    cannot_do: string[];
    traits_override: Record<string, number>;
    preferred_mode: string;
  }>({
    name: "",
    role: "",
    gender: "",
    avatar: "",
    personality: "",
    rules: [],
    can_do: [],
    cannot_do: [],
    traits_override: {},
    preferred_mode: "",
  });
  /** User avatar (data URL) for chat display. Stored in localStorage. */
  const [userAvatar, setUserAvatar] = useState<string>(() => {
    try {
      return localStorage.getItem("akasha_user_avatar") ?? "";
    } catch {
      return "";
    }
  });
  const [agentProfileLoading, setAgentProfileLoading] = useState(false);
  const [agentProfileSaving, setAgentProfileSaving] = useState(false);
  const [agentProfileError, setAgentProfileError] = useState<string | null>(null);
  /** User profile (first name, how to call, proactive check-in) for Settings panel. */
  const [userProfile, setUserProfile] = useState<{
    first_name: string;
    last_name: string;
    how_to_call: string;
    onboarding_completed: boolean;
    proactive_check_in_enabled: boolean;
    proactive_check_in_interval_days: number;
  }>({ first_name: "", last_name: "", how_to_call: "", onboarding_completed: false, proactive_check_in_enabled: false, proactive_check_in_interval_days: 0 });
  const [userProfileLoading, setUserProfileLoading] = useState(false);
  const [userProfileSaving, setUserProfileSaving] = useState(false);
  const [userProfileError, setUserProfileError] = useState<string | null>(null);
  const [settingsSection, setSettingsSection] = useState<SettingsSection>("display");
  const [agentProfileSubTab, setAgentProfileSubTab] = useState<AgentProfileSubTab>("identity");
  const [rulesDraft, setRulesDraft] = useState("");
  const [canDoDraft, setCanDoDraft] = useState("");
  const [cannotDoDraft, setCannotDoDraft] = useState("");
  /** Attachments for the next message: images (vision) and documents (text appended to message). */
  const [attachments, setAttachments] = useState<Array<{ id: string; name: string; typ: "image" | "document"; content_base64: string; mime_type: string }>>([]);
  const chatEndRef = useRef<HTMLDivElement>(null);
  const chatInlineReplyRef = useRef<HTMLDivElement>(null);
  const chatInputRef = useRef<HTMLInputElement>(null);
  /** True when we loaded with existing messages (reconnect during the day); send once then clear. */
  const firstMessageSinceLoadRef = useRef(false);
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

  // Voice (TTS/STT): fetch status when daemon is up so we can show "message vocal" button when STT is configured.
  useEffect(() => {
    if (!health?.ok) {
      setVoiceStatus(null);
      return;
    }
    let cancelled = false;
    (async () => {
      try {
        const status = await invoke<{ tts_configured?: boolean; stt_configured?: boolean }>("get_voice_status", {
          port: DAEMON_PORT,
        });
        if (!cancelled) setVoiceStatus(status ?? null);
      } catch {
        if (!cancelled) setVoiceStatus(null);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [health?.ok]);

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

  // Device bridge: poll for pending device requests (camera, mic, etc.) when daemon is healthy.
  const fetchDevicePending = useCallback(async () => {
    if (!health?.ok || devicePendingRequest != null) return;
    try {
      const data = await invoke<{ pending?: boolean; request_id?: string; interface?: string; device_id?: string; action?: string; params?: unknown }>(
        "get_device_pending",
        { port: DAEMON_PORT }
      );
      if (data?.request_id && data?.interface != null && data?.action != null) {
        setDevicePendingRequest({
          request_id: data.request_id,
          interface: data.interface,
          device_id: data.device_id ?? "",
          action: data.action,
          params: data.params ?? {},
        });
      }
    } catch {
      /* ignore */
    }
  }, [health?.ok, devicePendingRequest]);

  useEffect(() => {
    fetchDevicePending();
    const id = setInterval(fetchDevicePending, 2500);
    return () => clearInterval(id);
  }, [fetchDevicePending]);

  // Load conversation history on mount (use persisted session_id so it survives UI restart).
  // If session is empty, fetch user profile and optionally first-message (onboarding or daily greeting).
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const data = await invoke<{ session_id?: string; turns?: Array<{ role: string; content: string }> }>(
          "get_memory_short_term",
          { sessionId: sessionId ?? undefined, port: DAEMON_PORT }
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
          firstMessageSinceLoadRef.current = true;
          try {
            localStorage.setItem(AKASHA_SESSION_ID_KEY, data.session_id);
          } catch {
            /* ignore */
          }
        } else if (data?.session_id) {
          setSessionId(data.session_id);
          try {
            localStorage.setItem(AKASHA_SESSION_ID_KEY, data.session_id);
          } catch {
            /* ignore */
          }
          // Session empty: show onboarding, or proactive check-in, or first-today greeting
          if ((data.turns?.length ?? 0) === 0) {
            try {
              const profile = await invoke<{ how_to_call?: string; onboarding_completed?: boolean }>("get_user_profile", { port: DAEMON_PORT });
              if (cancelled) return;
              const needsOnboarding = !profile?.how_to_call?.trim() || !profile?.onboarding_completed;
              if (needsOnboarding) {
                const first = await invoke<{ message?: string; session_id?: string }>("get_first_message", { context: "onboarding", port: DAEMON_PORT });
                if (cancelled) return;
                const msg = first?.message?.trim();
                if (msg && first?.session_id) {
                  setMessages([{ role: "assistant", text: msg }]);
                  setSessionId(first.session_id);
                  try {
                    localStorage.setItem(AKASHA_SESSION_ID_KEY, first.session_id);
                  } catch {
                    /* ignore */
                  }
                }
              } else {
                const proactive = await invoke<{ message?: string; session_id?: string }>("get_first_message", { context: "proactive", port: DAEMON_PORT });
                if (cancelled) return;
                let msg = proactive?.message?.trim();
                let sid = proactive?.session_id;
                if (!msg && sid) {
                  const firstToday = await invoke<{ message?: string; session_id?: string }>("get_first_message", { context: "first_today", port: DAEMON_PORT });
                  if (cancelled) return;
                  msg = firstToday?.message?.trim();
                  sid = firstToday?.session_id;
                }
                if (msg && sid) {
                  setMessages([{ role: "assistant", text: msg }]);
                  setSessionId(sid);
                  try {
                    localStorage.setItem(AKASHA_SESSION_ID_KEY, sid);
                  } catch {
                    /* ignore */
                  }
                }
              }
            } catch {
              /* ignore */
            }
          }
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

  // Scroll chat to bottom when opening the chat tab or when messages/loading change
  useEffect(() => {
    if (tab === "chat") {
      requestAnimationFrame(() => {
        chatEndRef.current?.scrollIntoView({ behavior: "smooth" });
      });
    }
  }, [messages, loading, tab]);
  // When a reply is pending and modal is not open, scroll the inline reply form into view
  const pendingHumanInputKeys = Object.keys(pendingHumanInput);
  useEffect(() => {
    if (tab === "chat" && pendingHumanInputKeys.length > 0 && !humanInputModalTaskId) {
      requestAnimationFrame(() => {
        chatInlineReplyRef.current?.scrollIntoView({ behavior: "smooth", block: "nearest" });
      });
    }
  }, [tab, humanInputModalTaskId, pendingHumanInputKeys.length]);
  useEffect(() => {
    if (tab === "chat") chatInputRef.current?.focus();
  }, [tab]);

  // Restore focus on chat input when loading finishes (task completed, failed, or slash command done)
  const prevLoadingRef = useRef(loading);
  useEffect(() => {
    if (prevLoadingRef.current === true && loading === false && tab === "chat") {
      requestAnimationFrame(() => {
        chatInputRef.current?.focus();
      });
    }
    prevLoadingRef.current = loading;
  }, [loading, tab]);

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
  const tabsByIndex: Tab[] = ["chat", "scheduled", "router", "docs", "tasks", "calendar", "memory", "settings"];
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (humanInputModalTaskId != null) return;
      const target = e.target as HTMLElement;
      if (target?.closest("input") || target?.closest("textarea") || target?.closest("[role='dialog']")) return;
      const n = e.key === "1" ? 1 : e.key === "2" ? 2 : e.key === "3" ? 3 : e.key === "4" ? 4 : e.key === "5" ? 5 : e.key === "6" ? 6 : e.key === "7" ? 7 : e.key === "8" ? 8 : 0;
      if (n >= 1 && n <= 8) {
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
      const data = await invoke<{ tasks?: Array<{ id?: string; status?: string; label?: string; created_at?: string; parent_task_id?: string; assigned_agent?: string }> }>("get_tasks", {
        port: DAEMON_PORT,
      });
      const list = data?.tasks ?? [];
      const tasks: Array<TaskListItem> = list
        .map((t) => ({
          id: t.id ?? "",
          status: t.status ?? "?",
          label: t.label,
          created_at: t.created_at,
          parent_task_id: t.parent_task_id,
          assigned_agent: t.assigned_agent,
        }))
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

  const filteredTasksList = useMemo(() => {
    let list = tasksList;
    if (taskListFilter === "active") {
      list = list.filter((t) => t.status === "pending" || t.status === "running");
    } else {
      list = list.filter((t) => t.status === "completed" || t.status === "failed");
    }
    const q = taskSearchQuery.trim().toLowerCase();
    if (q) {
      list = list.filter(
        (t) =>
          (t.label && t.label.toLowerCase().includes(q)) ||
          (t.id && t.id.toLowerCase().includes(q))
      );
    }
    return list;
  }, [tasksList, taskListFilter, taskSearchQuery]);

  const taskDisplayLabel = (task: TaskListItem) => (task.label && task.label.trim() ? task.label.trim() : t("tasks.task_unnamed") + task.id.slice(-8));

  const fetchTasksEvents = useCallback(async (taskId: string) => {
    try {
      const data = await invoke<{ events?: Array<{ event_type?: string; payload?: unknown; at?: string }> }>(
        "get_task_events",
        { taskId, port: DAEMON_PORT }
      );
      const list = data?.events ?? [];
      const planEvent = list.find((e) => (e.event_type === "plan_proposed" || e.event_type === "plan_committed") && e.payload && typeof e.payload === "object" && "steps" in e.payload);
      const planStepsCount =
        planEvent && planEvent.payload && typeof planEvent.payload === "object" && Array.isArray((planEvent.payload as { steps?: unknown }).steps)
          ? (planEvent.payload as { steps: unknown[] }).steps.length
          : 0;
      // #region agent log
      fetch('http://127.0.0.1:7790/ingest/83a7f7de-74a3-4ba3-8a97-b0169801051e',{method:'POST',headers:{'Content-Type':'application/json','X-Debug-Session-Id':'905d69'},body:JSON.stringify({sessionId:'905d69',runId:'pre-fix-restore',hypothesisId:'H9',location:'App.tsx:fetchTasksEvents',message:'Fetched task events and extracted plan metadata',data:{taskId,eventsCount:list.length,planStepsCount},timestamp:Date.now()})}).catch(()=>{});
      // #endregion
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

  const fetchTaskSteps = useCallback(async (taskId: string) => {
    try {
      const raw = await invoke<string>("get_task_status", { taskId, port: DAEMON_PORT });
      const j = JSON.parse(raw) as {
        todos?: Array<{ id?: string | null; title?: string; status?: string }>;
      };
      const rows = (j.todos ?? []).map((x) => ({
        id: x.id,
        title: x.title ?? "",
        status: (x.status ?? "pending").toLowerCase(),
      }));
      // #region agent log
      fetch('http://127.0.0.1:7790/ingest/83a7f7de-74a3-4ba3-8a97-b0169801051e',{method:'POST',headers:{'Content-Type':'application/json','X-Debug-Session-Id':'905d69'},body:JSON.stringify({sessionId:'905d69',runId:'pre-fix-restore',hypothesisId:'H10',location:'App.tsx:fetchTaskSteps',message:'Fetched task todos used by steps panel',data:{taskId,todosCount:rows.length},timestamp:Date.now()})}).catch(()=>{});
      // #endregion
      if (selectedTaskIdForTodosRef.current === taskId) setTaskStepsTodos(rows);
    } catch {
      if (selectedTaskIdForTodosRef.current === taskId) setTaskStepsTodos([]);
    }
  }, []);

  useEffect(() => {
    fetchTaskStepsRef.current = fetchTaskSteps;
  }, [fetchTaskSteps]);

  useEffect(() => {
    selectedTaskIdForTodosRef.current = tasksList[tasksSelected]?.id ?? null;
  }, [tasksList, tasksSelected]);

  useEffect(() => {
    const id = tasksList[tasksSelected]?.id;
    if (!id) {
      setTaskStepsTodos([]);
      return;
    }
    void fetchTaskSteps(id);
  }, [tasksList, tasksSelected, fetchTaskSteps]);

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
      const data = await invoke<{ events?: Array<{ at: string; task_id: string; type: string; status: string; label?: string; schedule_id?: string | null }> }>("get_calendar_events", {
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
    const cached = getCached<Array<TaskListItem>>("tasks");
    if (cached != null) {
      setTasksList(cached);
      setTasksSelected((prev) => (prev >= cached.length && cached.length > 0 ? cached.length - 1 : prev));
      setTasksLoading(false);
      return;
    }
    fetchTasksList();
  }, [tab, fetchTasksList]);

  const applyChatStreamProgress = useCallback((taskId: string, msg: string) => {
    if (!taskId || taskId !== lastChatTaskIdRef.current) return;
    if (!msg.trim()) return;
    if (isChatStreamToolPhase(msg)) {
      setMessages((prev) => {
        const idx = prev.findIndex((m) => m.role === "assistant" && m.taskId === taskId);
        if (idx < 0) return prev;
        const next = [...prev];
        next[idx] = {
          role: "assistant",
          text: ackTextByTaskRef.current[taskId] ?? next[idx].text,
          taskId,
          streaming: false,
        };
        return next;
      });
      return;
    }
    if (shouldChatStreamProgress(msg)) {
      setMessages((prev) => {
        const idx = prev.findIndex((m) => m.role === "assistant" && m.taskId === taskId);
        if (idx < 0) return prev;
        const next = [...prev];
        next[idx] = { role: "assistant", text: msg, taskId, streaming: true };
        return next;
      });
    }
  }, []);

  // SSE: subscribe to daemon events for real-time updates (< 1s) when daemon is healthy.
  useEffect(() => {
    if (!health?.ok) return;
    const url = `http://127.0.0.1:${health.port ?? DAEMON_PORT}/api/events`;
    let es: EventSource | null = null;
    try {
      es = new EventSource(url);
      es.onmessage = (msgEv) => {
        fetchTasksList();
        fetchPendingHumanInput();
        try {
          const d = JSON.parse(msgEv.data) as {
            event_type?: string;
            payload?: { task_id?: string; message?: string } | Record<string, unknown>;
            correlation_id?: string | null;
          };
          if (d.event_type === "progress_update" && d.payload && typeof d.payload === "object") {
            const p = d.payload as Record<string, unknown>;
            const tid = typeof p.task_id === "string" ? p.task_id : "";
            const streamMsg = typeof p.message === "string" ? p.message : "";
            if (tid && streamMsg) applyChatStreamProgress(tid, streamMsg);
          }
          if (d.event_type === "todo_list_updated") {
            const tid =
              (typeof d.payload?.task_id === "string" && d.payload.task_id) ||
              (typeof d.correlation_id === "string" ? d.correlation_id : "") ||
              "";
            if (tid && tid === selectedTaskIdForTodosRef.current) {
              void fetchTaskStepsRef.current(tid);
            }
          }
        } catch {
          /* ignore */
        }
      };
      es.onerror = () => {
        es?.close();
        es = null;
      };
    } catch {
      /* ignore */
    }
    return () => {
      es?.close();
    };
  }, [health?.ok, health?.port, fetchTasksList, fetchPendingHumanInput, applyChatStreamProgress]);

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

  const MEMORY_PAGE_SIZE = 200;

  const fetchMemory = useCallback(async () => {
    setMemoryLoading(true);
    setMemoryError(null);
    try {
      const [shortRes, longRes] = await Promise.all([
        invoke<{ session_id?: string; turns?: Array<{ role: string; content: string }> }>("get_memory_short_term", {
          sessionId: sessionId ?? undefined,
          port: DAEMON_PORT,
        }),
        invoke<{ entries?: MemoryLongTermEntry[]; total?: number; long_term_available?: boolean }>("get_memory_long_term", {
          limit: MEMORY_PAGE_SIZE,
          offset: 0,
          port: DAEMON_PORT,
        }),
      ]);
      const short = shortRes?.turns ?? [];
      const long = (longRes?.entries ?? []) as MemoryLongTermEntry[];
      const total = typeof longRes?.total === "number" ? longRes.total : long.length;
      setMemoryShortTerm(short);
      setMemoryLongTerm(long);
      setMemoryLongTermTotal(total);
      setMemoryLongTermAvailable(longRes?.long_term_available ?? false);
      setCached("memory", { short, long, longTermAvailable: longRes?.long_term_available ?? false, total });
    } catch (e) {
      setMemoryError(String(e));
      setMemoryShortTerm([]);
      setMemoryLongTerm([]);
      setMemoryLongTermTotal(0);
      setMemoryLongTermAvailable(false);
    } finally {
      setMemoryLoading(false);
    }
  }, [sessionId]);

  const loadMoreMemoryLongTerm = useCallback(async () => {
    if (memoryLongTermLoadingMore || memoryLongTerm.length >= memoryLongTermTotal) return;
    setMemoryLongTermLoadingMore(true);
    try {
      const res = await invoke<{ entries?: MemoryLongTermEntry[]; total?: number }>("get_memory_long_term", {
        limit: MEMORY_PAGE_SIZE,
        offset: memoryLongTerm.length,
        port: DAEMON_PORT,
      });
      const next = (res?.entries ?? []) as MemoryLongTermEntry[];
      const total = typeof res?.total === "number" ? res.total : memoryLongTermTotal;
      setMemoryLongTerm((prev) => [...prev, ...next]);
      setMemoryLongTermTotal(total);
    } catch {
      // keep current state
    } finally {
      setMemoryLongTermLoadingMore(false);
    }
  }, [memoryLongTerm.length, memoryLongTermTotal, memoryLongTermLoadingMore]);

  const rebuildMemoryRelations = useCallback(async () => {
    setMemoryRebuildLoading(true);
    setMemoryRebuildMessage(null);
    try {
      const res = await invoke<{ inserted?: number; ok?: boolean; error?: string }>("rebuild_memory_relations", { port: DAEMON_PORT });
      if (res?.ok && typeof res.inserted === "number") {
        setMemoryRebuildMessage(t("memory.rebuild_success").replace("{{count}}", String(res.inserted)));
        fetchMemory();
      } else {
        setMemoryRebuildMessage(res?.error ?? t("memory.rebuild_error"));
      }
    } catch (e) {
      setMemoryRebuildMessage(String(e));
    } finally {
      setMemoryRebuildLoading(false);
    }
    setTimeout(() => setMemoryRebuildMessage(null), 5000);
  }, [fetchMemory, t]);

  const runMemorySearch = useCallback(async () => {
    const q = memorySearchQuery.trim();
    if (!q) return;
    setMemorySearchLoading(true);
    try {
      const res = await invoke<{ results?: Array<{ id: string; content: string }>; long_term_available?: boolean }>("get_memory_search", {
        q,
        top_k: 20,
        port: DAEMON_PORT,
      });
      setMemorySearchResults(res?.results ?? []);
    } catch (e) {
      setMemoryError(String(e));
      setMemorySearchResults([]);
    } finally {
      setMemorySearchLoading(false);
    }
  }, [memorySearchQuery]);

  useEffect(() => {
    if (tab !== "memory") return;
    const cached = getCached<{ short: Array<{ role: string; content: string }>; long: MemoryLongTermEntry[]; longTermAvailable: boolean; total?: number }>("memory");
    if (cached != null) {
      setMemoryShortTerm(cached.short);
      setMemoryLongTerm(cached.long);
      setMemoryLongTermTotal(cached.total ?? cached.long.length);
      setMemoryLongTermAvailable(cached.longTermAvailable);
      setMemoryLoading(false);
      setMemoryError(null);
      return;
    }
    fetchMemory();
  }, [tab, fetchMemory]);

  useEffect(() => {
    if (memoryLongTerm.length > 0 && memoryLongTermSelected >= memoryLongTerm.length) {
      setMemoryLongTermSelected(memoryLongTerm.length - 1);
    }
  }, [memoryLongTerm.length, memoryLongTermSelected]);

  const memoryGraphOptions = useMemo<RGOptions>(() => {
    const colors = GRAPH_THEME_COLORS[theme];
    const lineColor = GRAPH_LINE_COLOR[theme];
    return {
      defaultJunctionPoint: "border",
      layout: { layoutName: "force" },
      disableZoom: false,
      disableDragNode: false,
      backgroundColor: colors.backgroundColor,
      defaultNodeColor: colors.defaultNodeColor,
      defaultNodeFontColor: colors.defaultNodeFontColor,
      defaultNodeBorderColor: colors.defaultNodeBorderColor,
      defaultLineColor: lineColor,
      defaultLineWidth: colors.defaultLineWidth,
      defaultLineFontColor: colors.defaultLineFontColor,
      defaultShowLineLabel: colors.defaultShowLineLabel,
      checkedLineColor: colors.checkedLineColor,
    };
  }, [theme]);

  useEffect(() => {
    if (!memoryViewGraph) return;
    const data = buildMemoryGraphData(memoryLongTerm, memoryLongTermSelected, GRAPH_THEME_COLORS[theme].nodeType, GRAPH_LINE_COLOR[theme]);
    if (!data) return;
    const t = setTimeout(() => {
      memoryGraphRef.current?.setJsonData(data, true, () => {});
    }, 0);
    return () => clearTimeout(t);
  }, [memoryViewGraph, memoryLongTerm, memoryLongTermSelected, theme]);

  // Re-apply graph options when theme changes so canvas/edges/nodes use the new colors (setOptions is on the instance, not the ref)
  useEffect(() => {
    if (!memoryViewGraph || !memoryGraphRef.current) return;
    const instance = memoryGraphRef.current.getInstance?.();
    if (instance) void (instance as { setOptions: (opts: RGOptions) => void | Promise<void> }).setOptions(memoryGraphOptions);
  }, [theme, memoryViewGraph, memoryGraphOptions]);

  const fetchScheduleReports = useCallback(async () => {
    try {
      const data = await invoke<{ reports?: Array<{ schedule_name?: string; message?: string; ended_at?: string }> }>("get_schedule_run_reports", { port: DAEMON_PORT });
      setScheduleReports((data?.reports ?? []).map((r) => ({ schedule_name: r.schedule_name ?? "", message: r.message ?? "Exécuté.", ended_at: r.ended_at })));
    } catch {
      setScheduleReports([]);
    }
  }, []);

  useEffect(() => {
    if (tab === "scheduled") fetchScheduleReports();
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

  const fetchAgentProfile = useCallback(async () => {
    setAgentProfileLoading(true);
    setAgentProfileError(null);
    try {
      const data = await invoke<{
        name?: string | null;
        personality?: string | null;
        role?: string | null;
        gender?: string | null;
        avatar?: string | null;
        rules?: string[];
        can_do?: string[];
        cannot_do?: string[];
        traits_override?: Record<string, number> | null;
        preferred_mode?: string | null;
      }>("get_agent_profile", { port: DAEMON_PORT });
      const to = data?.traits_override;
      setAgentProfile({
        name: data?.name ?? "",
        personality: data?.personality ?? "",
        role: data?.role ?? "",
        gender: data?.gender ?? "",
        avatar: data?.avatar ?? "",
        rules: Array.isArray(data?.rules) ? data.rules : [],
        can_do: Array.isArray(data?.can_do) ? data.can_do : [],
        cannot_do: Array.isArray(data?.cannot_do) ? data.cannot_do : [],
        traits_override: to && typeof to === "object" ? { ...to } : {},
        preferred_mode: data?.preferred_mode ?? "",
      });
    } catch (e) {
      setAgentProfileError(String(e));
    } finally {
      setAgentProfileLoading(false);
    }
  }, []);

  const fetchUserProfile = useCallback(async () => {
    setUserProfileLoading(true);
    setUserProfileError(null);
    try {
      const data = await invoke<{
        first_name?: string | null;
        last_name?: string | null;
        how_to_call?: string | null;
        onboarding_completed?: boolean;
        proactive_check_in_enabled?: boolean;
        proactive_check_in_interval_days?: number;
      }>("get_user_profile", { port: DAEMON_PORT });
      setUserProfile({
        first_name: data?.first_name ?? "",
        last_name: data?.last_name ?? "",
        how_to_call: data?.how_to_call ?? "",
        onboarding_completed: data?.onboarding_completed ?? false,
        proactive_check_in_enabled: data?.proactive_check_in_enabled ?? false,
        proactive_check_in_interval_days: typeof data?.proactive_check_in_interval_days === "number" ? data.proactive_check_in_interval_days : 0,
      });
    } catch (e) {
      setUserProfileError(String(e));
    } finally {
      setUserProfileLoading(false);
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
    if (tab === "settings") fetchAgentProfile();
  }, [tab, fetchAgentProfile]);

  useEffect(() => {
    if (tab === "settings" && settingsSection === "user") fetchUserProfile();
  }, [tab, settingsSection, fetchUserProfile]);

  useEffect(() => {
    if (!calendarSelectedTaskId) {
      setCalendarTaskDetail(null);
      setCalendarTaskDetailError(null);
      return;
    }
    setCalendarTaskDetailError(null);
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
        setCalendarTaskDetailError(null);
      } catch (e) {
        if (!cancelled) {
          setCalendarTaskDetail(null);
          setCalendarTaskDetailError(typeof e === "string" ? e : (e instanceof Error ? e.message : "Impossible de charger le détail de la tâche."));
        }
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
    if (!calendarCellDetail) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setCalendarCellDetail(null);
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [calendarCellDetail]);

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
    setScheduleEditPrompt(scheduleDetail?.channel_context ?? "");
  }, [scheduleDetail]);

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
/skills            — liste des skills installés
/skills list       — idem
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
      if (sub === "" || sub === "list") {
        try {
          const list = await invoke<Array<{ name?: string; description?: string }>>("get_skills", { port });
          if (!list?.length) return "Aucun skill installé. Utilisez /skills reload après en avoir ajouté dans data_dir/skills ou spec/skills.";
          return list
            .map((s) => `  • ${s.name ?? "?"} — ${(s.description ?? "").trim() || "(sans description)"}`)
            .join("\n");
        } catch {
          return "Impossible de lister les skills (daemon déconnecté ou erreur).";
        }
      }
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
      return "Usage: /skills [list] — lister les skills ; /skills reload — recharger ; /skills uninstall <nom> — désinstaller.";
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

  const handlePathClick = useCallback((path: string, openFolder?: boolean) => {
    invoke(openFolder ? "open_path_in_explorer" : "open_path", { path }).catch(() => {});
  }, []);

  const handleVoiceMessageToggle = useCallback(async () => {
    if (voiceRecording) {
      const mr = voiceMediaRecorderRef.current;
      if (mr && mr.state !== "inactive") {
        mr.stop();
      }
      setVoiceRecording(false);
      voiceMediaRecorderRef.current = null;
      const chunks = voiceChunksRef.current;
      voiceChunksRef.current = [];
      if (chunks.length === 0) return;
      const blob = new Blob(chunks, { type: "audio/webm;codecs=opus" });
      try {
        const dataUrl = await new Promise<string>((resolve, reject) => {
          const r = new FileReader();
          r.onload = () => resolve(r.result as string);
          r.onerror = () => reject(new Error("Read failed"));
          r.readAsDataURL(blob);
        });
        const result = await invoke<{ text?: string }>("voice_stt_transcribe", {
          port: DAEMON_PORT,
          payload: { data_url: dataUrl },
        });
        const text = (result?.text ?? "").trim();
        if (text) {
          setMessage(text);
          handleSendRef.current(text, true);
        }
      } catch (err) {
        setMessages((prev) => [...prev, { role: "assistant", text: `Erreur transcription : ${String(err)}`, error: true }]);
      }
      return;
    }
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      const mimeType = MediaRecorder.isTypeSupported("audio/webm;codecs=opus") ? "audio/webm;codecs=opus" : "audio/webm";
      const recorder = new MediaRecorder(stream, { mimeType });
      voiceChunksRef.current = [];
      recorder.ondataavailable = (e) => {
        if (e.data.size > 0) voiceChunksRef.current.push(e.data);
      };
      recorder.onstop = () => {
        stream.getTracks().forEach((t) => t.stop());
      };
      recorder.start(200);
      voiceMediaRecorderRef.current = recorder;
      setVoiceRecording(true);
    } catch (err) {
      setMessages((prev) => [...prev, { role: "assistant", text: `Micro inaccessible : ${String(err)}`, error: true }]);
    }
  }, [voiceRecording, sessionId]);

  const handleSend = async (overrideMessage?: string, fromVoice?: boolean) => {
    const content = (overrideMessage ?? message).trim();
    const hasContent = content || attachments.length > 0;
    if (!hasContent || loading) return;

    if (fromVoice) replyWithTtsRef.current = true;
    const userMessage = content || "(Pièce(s) jointe(s))";
    setMessages((prev) => {
      const cleaned = prev.filter((m) => !(m.role === "assistant" && m.streaming));
      return [...cleaned, { role: "user", text: userMessage }];
    });
    if (overrideMessage === undefined) setMessage("");
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
      const reconnect = firstMessageSinceLoadRef.current;
      firstMessageSinceLoadRef.current = false;
      const ack = await invoke<{ task_id: string; session_id: string; message: string }>("send_message_ack", {
        message: userMessage,
        session_id: sessionId,
        attachments: attachmentsPayload,
        port: DAEMON_PORT,
        reconnect: reconnect || undefined,
      });
      setLoading(false);
      if (ack?.session_id) {
        setSessionId(ack.session_id);
        try {
          localStorage.setItem(AKASHA_SESSION_ID_KEY, ack.session_id);
        } catch {
          /* ignore */
        }
      }
      const ackText = ack?.message ?? "Request received. You can follow progress in the Tasks tab.";
      if (ack?.task_id) {
        lastChatTaskIdRef.current = ack.task_id;
        ackTextByTaskRef.current[ack.task_id] = ackText;
        setMessages((prev) => [...prev, { role: "assistant", text: ackText, taskId: ack.task_id }]);
      } else {
        lastChatTaskIdRef.current = null;
        setMessages((prev) => [...prev, { role: "assistant", text: ackText }]);
      }
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
              const status = JSON.parse(raw) as {
                status?: string;
                progress?: Array<{ progress_pct?: number; message?: string }>;
                tokens_used?: number;
                cost_usd?: number;
              };
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
              if (status?.status !== "completed" && status?.status !== "failed" && msg) {
                applyChatStreamProgress(taskId, msg);
              }
              if (status?.status === "completed") {
                setRunningTaskChips((prev) => { const next = { ...prev }; delete next[taskId]; return next; });
                setRunningTaskEvents((prev) => { const next = { ...prev }; delete next[taskId]; return next; });
                setPendingHumanInput((prev) => { const next = { ...prev }; delete next[taskId]; return next; });
                humanInputAutoOpenedRef.current.delete(taskId);
                setHumanInputModalTaskId((c) => (c === taskId ? null : c));
                const finalMsg = status?.progress?.slice(-1)[0]?.message ?? "Terminé.";
                if (taskId === lastChatTaskIdRef.current) {
                  setMessages((prev) => {
                    const idx = prev.findIndex((m) => m.role === "assistant" && m.taskId === taskId);
                    if (idx >= 0) {
                      const next = [...prev];
                      next[idx] = { role: "assistant", text: finalMsg };
                      return next;
                    }
                    return [...prev, { role: "assistant", text: finalMsg }];
                  });
                  delete ackTextByTaskRef.current[taskId];
                }
                if (replyWithTtsRef.current && voiceStatus?.tts_configured && finalMsg?.trim()) {
                  replyWithTtsRef.current = false;
                  invoke<{ data_url?: string }>("voice_tts", { text: finalMsg, port: DAEMON_PORT })
                    .then((r) => {
                      const url = r?.data_url;
                      if (url) {
                        const audio = new Audio(url);
                        audio.play().catch(() => {});
                      }
                    })
                    .catch(() => {});
                }
                requestAnimationFrame(() => chatInputRef.current?.focus());
                return;
              }
              if (status?.status === "failed") {
                setRunningTaskChips((prev) => { const next = { ...prev }; delete next[taskId]; return next; });
                setRunningTaskEvents((prev) => { const next = { ...prev }; delete next[taskId]; return next; });
                setPendingHumanInput((prev) => { const next = { ...prev }; delete next[taskId]; return next; });
                humanInputAutoOpenedRef.current.delete(taskId);
                setHumanInputModalTaskId((c) => (c === taskId ? null : c));
                replyWithTtsRef.current = false;
                if (taskId === lastChatTaskIdRef.current) {
                  setMessages((prev) => {
                    const idx = prev.findIndex((m) => m.role === "assistant" && m.taskId === taskId);
                    if (idx >= 0) {
                      const next = [...prev];
                      const MAX_FAILURE_CHAT_CHARS = 2500;
                      const baseMsg = msg?.trim() ? msg.trim() : "Tâche en échec.";
                      const tokensUsed = status?.tokens_used;
                      const costUsd = status?.cost_usd;
                      const suffixParts: string[] = [];
                      if (typeof tokensUsed === "number") suffixParts.push(`Tokens: ${tokensUsed}`);
                      if (typeof costUsd === "number" && Number.isFinite(costUsd) && Math.abs(costUsd) > 0) suffixParts.push(`Coût: ${costUsd.toFixed(4)} USD`);
                      const suffix = suffixParts.length > 0 ? `\n\n${suffixParts.join(" · ")}` : "";
                      const composed = `${baseMsg}${suffix}`;
                      const finalMsg = composed.length > MAX_FAILURE_CHAT_CHARS ? composed.slice(0, MAX_FAILURE_CHAT_CHARS).trimEnd() + "…" : composed;
                      next[idx] = { role: "assistant", text: finalMsg, error: true };
                      return next;
                    }
                    const MAX_FAILURE_CHAT_CHARS = 2500;
                    const baseMsg = msg?.trim() ? msg.trim() : "Tâche en échec.";
                    const tokensUsed = status?.tokens_used;
                    const costUsd = status?.cost_usd;
                    const suffixParts: string[] = [];
                    if (typeof tokensUsed === "number") suffixParts.push(`Tokens: ${tokensUsed}`);
                    if (typeof costUsd === "number" && Number.isFinite(costUsd) && Math.abs(costUsd) > 0) suffixParts.push(`Coût: ${costUsd.toFixed(4)} USD`);
                    const suffix = suffixParts.length > 0 ? `\n\n${suffixParts.join(" · ")}` : "";
                    const composed = `${baseMsg}${suffix}`;
                    const finalMsg = composed.length > MAX_FAILURE_CHAT_CHARS ? composed.slice(0, MAX_FAILURE_CHAT_CHARS).trimEnd() + "…" : composed;
                    return [...prev, { role: "assistant", text: finalMsg, error: true }];
                  });
                  delete ackTextByTaskRef.current[taskId];
                }
                requestAnimationFrame(() => chatInputRef.current?.focus());
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
          if (taskId === lastChatTaskIdRef.current) {
            setMessages((prev) => {
              const idx = prev.findIndex((m) => m.role === "assistant" && m.taskId === taskId);
              if (idx >= 0) {
                const next = [...prev];
                next[idx] = { role: "assistant", text: "Délai dépassé. Consultez Tâches." };
                return next;
              }
              return [...prev, { role: "assistant", text: "Délai dépassé. Consultez Tâches." }];
            });
            delete ackTextByTaskRef.current[taskId];
          }
          requestAnimationFrame(() => chatInputRef.current?.focus());
        };
        pollUntilDone();
      }
    } catch (err) {
      setLoading(false);
      setMessages((prev) => [...prev, { role: "assistant", text: `Erreur : ${String(err)}`, error: true }]);
    }
    chatInputRef.current?.focus();
  };
  handleSendRef.current = handleSend;

  return (
    <div className="app">
      <a href="#main-content" className="skip-link">Aller au contenu principal</a>
      {updateBannerInfo && (
        <div className="update-banner" role="region" aria-label={t("update.banner_label")}>
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
      <div className="app-body">
        <aside className="sidebar-left" aria-label="Navigation principale">
          <div className="sidebar-left-top">
            <h1 className="logo">Akasha</h1>
            <p className="tagline">Local-first AI assistant</p>
          </div>
          <nav className="tabs sidebar-nav" role="tablist" aria-label="Sections">
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
              aria-selected={tab === "scheduled"}
              aria-controls="panel-scheduled"
              id="tab-scheduled"
              className={tab === "scheduled" ? "active" : ""}
              onClick={() => setTab("scheduled")}
            >
              {t("tabs.scheduled")}
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
          <div className="sidebar-left-bottom">
            <div className="daemon-status" role="status" aria-live="polite">
              <span
                className={`status-dot ${health?.ok ? "connected" : "disconnected"}`}
                aria-hidden
              />
              {health?.ok ? (
                <span>{t("sidebar.daemon_connected")} {health.port ?? DAEMON_PORT})</span>
              ) : (
                <span>{t("sidebar.daemon_disconnected")} <code>akasha start</code></span>
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
          </div>
        </aside>

        <div className="container-main">
          <div className="container-main-inner">
            <header className="view-header">
              <h2 className="view-title">{t("tabs." + tab)}</h2>
              <span
                className={`daemon-status ${health?.ok ? "daemon-status-ok" : "daemon-status-off"}`}
                role="status"
                aria-live="polite"
                title={health?.ok ? t("status.daemon_ok") : t("status.daemon_off")}
              >
                {health?.ok ? t("status.daemon_ok") : t("status.daemon_off")}
              </span>
              <button
                type="button"
                className="sidebar-right-toggle"
                onClick={() => setRightSidebarOpen((o) => !o)}
                aria-expanded={rightSidebarOpen}
                aria-label={rightSidebarOpen ? t("sidebar.hide_tasks") : t("sidebar.show_tasks")}
                title={rightSidebarOpen ? t("sidebar.hide_tasks") : t("sidebar.show_tasks")}
              >
                {rightSidebarOpen ? "▐" : "▌"}
              </button>
            </header>
      <main className="main" id="main-content" tabIndex={-1}>
        {/* Onboarding: first steps modal (dismissible, "Ne plus afficher" stored in localStorage) */}
        {showOnboarding && (
          <div className="human-input-overlay onboarding-overlay" role="dialog" aria-labelledby="onboarding-title" aria-modal="true">
            <div className="human-input-modal onboarding-modal">
              <h2 id="onboarding-title">{t("onboarding.title")}</h2>
              <p className="onboarding-intro">{t("onboarding.intro")}</p>
              <ul className="onboarding-steps">
                <li>{t("onboarding.step0")}</li>
                <li>{t("onboarding.step1")}</li>
                <li>{t("onboarding.step2")}</li>
                <li>{t("onboarding.step3")}</li>
              </ul>
              <div className="onboarding-actions">
                <button type="button" className="onboarding-doc-btn" onClick={() => { setTab("docs"); setShowOnboarding(false); }}>
                  {t("onboarding.open_doc")}
                </button>
                <button type="button" className="onboarding-dismiss" onClick={() => { try { localStorage.setItem("akasha_onboarding_dismissed", "1"); } catch { /* ignore */ } setShowOnboarding(false); }}>
                  {t("onboarding.dismiss")}
                </button>
                <button type="button" className="human-input-close" onClick={() => setShowOnboarding(false)} aria-label={t("common.close")}>
                  ×
                </button>
              </div>
            </div>
          </div>
        )}
        {/* Human-in-the-loop: visible on all tabs */}
        {Object.keys(pendingHumanInput).length > 0 && !humanInputModalTaskId && (
          <div className="chat-human-input-banner global-human-input-banner" role="status">
            {t("human_input.banner")}
            <button type="button" className="human-input-banner-action" onClick={() => setHumanInputModalTaskId(Object.keys(pendingHumanInput)[0])}>
              {t("human_input.reply")}
            </button>
          </div>
        )}
        {humanInputModalTaskId && pendingHumanInput[humanInputModalTaskId] && (
          <div className="human-input-overlay" role="dialog" aria-labelledby="human-input-title" aria-modal="true">
            <div className="human-input-modal">
              <h2 id="human-input-title">{t("human_input.action_required")}</h2>
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
                  <input
                    type="text"
                    value={humanInputFreeText}
                    onChange={(e) => setHumanInputFreeText(e.target.value)}
                    placeholder={t("human_input.placeholder")}
                    onKeyDown={(e) => e.key === "Enter" && document.getElementById("human-input-submit-btn")?.click()}
                  />
                  <button
                    id="human-input-submit-btn"
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
                    {t("human_input.submit")}
                  </button>
                </div>
              )}
              <button type="button" className="human-input-close" onClick={() => setHumanInputModalTaskId(null)} aria-label={t("common.close")}>
                ×
              </button>
            </div>
          </div>
        )}
        {/* Device bridge: agent requested device access (camera, mic, etc.) */}
        {devicePendingRequest && (
          <div className="human-input-overlay" role="dialog" aria-labelledby="device-request-title" aria-modal="true">
            <div className="human-input-modal" onClick={(e) => e.stopPropagation()}>
              <h2 id="device-request-title">{t("device_bridge.title")}</h2>
              <p className="human-input-question">
                {t("device_bridge.description")} <strong>{devicePendingRequest.interface}</strong> — <strong>{devicePendingRequest.action}</strong>
              </p>
              <div className="human-input-choices">
                <button
                  type="button"
                  className="human-input-choice-btn"
                  onClick={async () => {
                    const payload = { ...devicePendingRequest };
                    setDevicePendingRequest(null);
                    const { request_id, interface: iface, action: act, params: reqParams } = payload;
                    const ALLOW_TIMEOUT_MS = 90000;
                    const runAllow = async () => {
                    try {
                      if (iface === "synthetic_input") {
                        try {
                          const ok = await invoke<boolean>("execute_synthetic_input", {
                            action: act,
                            params: typeof reqParams === "object" && reqParams !== null ? reqParams : {},
                          });
                          await invoke("post_device_result", {
                            request_id,
                            success: ok,
                            data: null,
                            port: DAEMON_PORT,
                          });
                        } catch (err) {
                          await invoke("post_device_result", {
                            request_id,
                            success: false,
                            data: err instanceof Error ? err.message : String(err),
                            port: DAEMON_PORT,
                          });
                        }
                      } else if (iface === "local_media" && (act === "capture" || act === "camera_capture")) {
                        const GETUSERMEDIA_TIMEOUT_MS = 15000;
                        let stream: MediaStream;
                        try {
                          stream = await Promise.race([
                            navigator.mediaDevices.getUserMedia({ video: true }),
                            new Promise<never>((_, rej) => setTimeout(() => rej(new Error("getUserMedia 15s timeout")), GETUSERMEDIA_TIMEOUT_MS)),
                          ]);
                        } catch (e) {
                          await invoke("post_device_result", { request_id, success: false, data: null, port: DAEMON_PORT }).catch(() => {});
                          return;
                        }
                        const video = document.createElement("video");
                        video.muted = true;
                        video.playsInline = true;
                        video.autoplay = true;
                        video.srcObject = stream;
                        video.play().catch(() => {});
                        await new Promise<void>((r) => setTimeout(r, 800));
                        try {
                          const canvas = document.createElement("canvas");
                          const maxW = 1024;
                          const w = Math.max(1, video.videoWidth || 640);
                          const h = Math.max(1, video.videoHeight || 480);
                          const scale = w > maxW || h > maxW ? maxW / Math.max(w, h) : 1;
                          canvas.width = Math.round(w * scale);
                          canvas.height = Math.round(h * scale);
                          const ctx = canvas.getContext("2d");
                          if (ctx) ctx.drawImage(video, 0, 0, w, h, 0, 0, canvas.width, canvas.height);
                          const data = canvas.toDataURL("image/jpeg", 0.85).split(",")[1] ?? "";
                          const url = `http://127.0.0.1:${DAEMON_PORT}/api/device/result`;
                          try {
                            const res = await fetch(url, {
                              method: "POST",
                              headers: { "Content-Type": "application/json" },
                              body: JSON.stringify({ request_id, success: true, data }),
                            });
                            if (!res.ok) {
                              await invoke("post_device_result", { request_id, success: false, data: null, port: DAEMON_PORT }).catch(() => {});
                            }
                          } catch (e) {
                            await invoke("post_device_result", { request_id, success: false, data: null, port: DAEMON_PORT }).catch(() => {});
                          }
                        } finally {
                          stream.getTracks().forEach((t) => t.stop());
                        }
                      } else if (iface === "local_media" && (act === "record" || act === "microphone_record")) {
                        const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
                        const recorder = new MediaRecorder(stream);
                        const chunks: Blob[] = [];
                        recorder.ondataavailable = (e) => e.data.size && chunks.push(e.data);
                        recorder.start();
                        await new Promise<void>((resolve) => {
                          recorder.onstop = () => {
                            resolve();
                          };
                          setTimeout(() => {
                            recorder.stop();
                          }, 3000);
                        });
                        stream.getTracks().forEach((t) => t.stop());
                        const blob = new Blob(chunks, { type: "audio/webm" });
                        const reader = new FileReader();
                        const data = await new Promise<string>((resolve, reject) => {
                          reader.onload = () => {
                            const result = reader.result as string;
                            resolve(result.includes(",") ? result.split(",")[1] ?? "" : "");
                          };
                          reader.onerror = reject;
                          reader.readAsDataURL(blob);
                        });
                        await invoke("post_device_result", { request_id, success: true, data, port: DAEMON_PORT });
                      } else {
                        await invoke("post_device_result", { request_id, success: false, data: null, port: DAEMON_PORT });
                      }
                    } catch (e) {
                      console.error(e);
                      await invoke("post_device_result", { request_id, success: false, data: null, port: DAEMON_PORT });
                    }
                    };
                    try {
                      await Promise.race([
                        runAllow(),
                        new Promise<never>((_, rej) => setTimeout(() => rej(new Error("allow_timeout_90s")), ALLOW_TIMEOUT_MS)),
                      ]);
                    } catch (e) {
                      await invoke("post_device_result", { request_id, success: false, data: null, port: DAEMON_PORT }).catch(() => {});
                    }
                  }}
                >
                  {t("device_bridge.allow")}
                </button>
                <button
                  type="button"
                  className="human-input-choice-btn"
                  onClick={async () => {
                    try {
                      await invoke("post_device_result", {
                        request_id: devicePendingRequest.request_id,
                        success: false,
                        data: null,
                        port: DAEMON_PORT,
                      });
                    } catch (e) {
                      console.error(e);
                    }
                    setDevicePendingRequest(null);
                  }}
                >
                  {t("device_bridge.deny")}
                </button>
              </div>
              <button
                type="button"
                className="human-input-close"
                onClick={async () => {
                  try {
                    await invoke("post_device_result", {
                      request_id: devicePendingRequest.request_id,
                      success: false,
                      data: null,
                      port: DAEMON_PORT,
                    });
                  } catch (e) {
                    console.error(e);
                  }
                  setDevicePendingRequest(null);
                }}
                aria-label={t("common.close")}
              >
                ×
              </button>
            </div>
          </div>
        )}
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
                  {messages.map((m, i) => {
                    const askUserData = m.role === "assistant" ? parseAskUserMessage(m.text) : null;
                    return (
                      <div
                        key={i}
                        className={`message ${m.role} ${m.error ? "error" : ""} ${askUserData ? "message-ask-user" : ""} ${m.streaming ? "message-streaming" : ""}`}
                      >
                        <div className="message-head">
                          {m.role === "user" ? (userAvatar ? <img src={userAvatar} alt="" className="message-avatar message-avatar-user" /> : null) : m.role === "assistant" ? (agentProfile.avatar ? <img src={agentProfile.avatar} alt="" className="message-avatar message-avatar-assistant" /> : null) : null}
                          <span className="role" aria-hidden>
                            {m.role === "user" ? "Vous" : m.role === "system" ? "Système" : (agentProfile.name || "Akasha")}
                          </span>
                        </div>
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
                                {askUserData.choices.map((choice, j) => {
                                  const pendingTaskIdForReply = Object.keys(pendingHumanInput)[0] ?? null;
                                  return pendingTaskIdForReply ? (
                                    <button
                                      key={j}
                                      type="button"
                                      className="message-ask-user-choice-tag"
                                      onClick={async () => {
                                        try {
                                          await invoke("post_task_human_reply", { taskId: pendingTaskIdForReply, response: choice, port: DAEMON_PORT });
                                          setPendingHumanInput((prev) => { const next = { ...prev }; delete next[pendingTaskIdForReply]; return next; });
                                          setHumanInputModalTaskId((c) => (c === pendingTaskIdForReply ? null : c));
                                        } catch (e) {
                                          console.error(e);
                                        }
                                      }}
                                    >
                                      {choice}
                                    </button>
                                  ) : (
                                    <span key={j} className="message-ask-user-choice-tag">{choice}</span>
                                  );
                                })}
                              </div>
                            ) : null}
                            <p className="message-ask-user-hint">Répondre ci‑dessous (boutons ou champ texte) ou via « Action requise » sur la tâche.</p>
                          </div>
                        ) : (
                          <div className="text markdown-rendered">
                            <Suspense fallback={<span className="markdown-rendered">…</span>}><LazyMarkdownContent onPathClick={handlePathClick}>
                              {preprocessMessagePaths(preprocessDataUrlImages(m.text))}
                            </LazyMarkdownContent></Suspense>
                            {m.streaming ? <span className="message-streaming-caret" aria-hidden /> : null}
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
                                    const planEv = events.find((e) => (e.event_type === "plan_proposed" || e.event_type === "plan_committed") && e.payload && typeof e.payload === "object" && "steps" in e.payload);
                                    let steps: Array<{ step_id?: string; agent_type?: string; intent_preview?: string; intent?: string; acceptance_criteria_preview?: string | null; deliverables?: string[] | null }> = planEv?.payload && typeof planEv.payload === "object" && Array.isArray((planEv.payload as { steps?: unknown }).steps)
                                      ? (planEv.payload as { steps: Array<{ step_id?: string; agent_type?: string; intent_preview?: string; intent?: string; acceptance_criteria_preview?: string | null; deliverables?: string[] | null }> }).steps
                                      : [];
                                    if (steps.length === 0) {
                                      const decomposed = events.find((e) => e.event_type === "task_decomposed" && e.payload && typeof e.payload === "object" && "agents" in e.payload);
                                      const agents = decomposed?.payload && typeof decomposed.payload === "object" && Array.isArray((decomposed.payload as { agents?: unknown }).agents)
                                        ? (decomposed.payload as { agents: string[] }).agents
                                        : [];
                                      steps = agents.map((agent_type, i) => ({ step_id: `s${i}`, agent_type, intent_preview: "" }));
                                    }
                                    const byTask: Record<string, typeof events> = {};
                                    for (const ev of events) {
                                      const tid = ev.task_id ?? rootTaskId;
                                      if (!byTask[tid]) byTask[tid] = [];
                                      byTask[tid].push(ev);
                                    }
                                    return (
                                      <>
                                        {steps.length > 0 && (
                                          <div className="chat-subagents-plan" role="region" aria-label={t("events.plan_proposed")}>
                                            <h4 className="chat-subagents-plan-title">{t("events.plan_proposed")}</h4>
                                            <ol className="chat-subagents-plan-steps">
                                              {steps.map((s, i) => (
                                                <li key={s.step_id ?? i} className="chat-subagents-plan-step">
                                                  {s.agent_type && <span className="chat-subagents-plan-agent">{s.agent_type}</span>}
                                                  {s.step_id != null && s.step_id !== "" && (
                                                    <span className="chat-subagents-plan-step-id">{s.step_id}</span>
                                                  )}
                                                  <span className="chat-subagents-plan-intent">{s.intent_preview || s.intent || ""}</span>
                                                  {s.acceptance_criteria_preview != null && s.acceptance_criteria_preview.trim() !== "" && (
                                                    <div className="chat-subagents-plan-meta">{s.acceptance_criteria_preview}</div>
                                                  )}
                                                  {Array.isArray(s.deliverables) && s.deliverables.length > 0 && (
                                                    <ul className="chat-subagents-plan-deliverables">
                                                      {s.deliverables.map((d, j) => (
                                                        <li key={j}>{d}</li>
                                                      ))}
                                                    </ul>
                                                  )}
                                                </li>
                                              ))}
                                            </ol>
                                          </div>
                                        )}
                                        {Object.entries(byTask).map(([tid, evs]) => (
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
                                              {ev.payload && typeof ev.payload === "object" && (ev.event_type === "tool_call_started" || ev.event_type === "tool_call_finished") && "tool" in ev.payload ? (
                                                <span className="chat-subagents-event-agent"> — {String((ev.payload as { tool?: string }).tool ?? "")}</span>
                                              ) : null}
                                              {ev.payload && typeof ev.payload === "object" && (ev.event_type === "task_completed" || ev.event_type === "task_failed") && "model_used" in ev.payload && (ev.payload as { model_used?: string | null }).model_used ? (
                                                <span className="chat-subagents-event-model"> — {t("tasks.model_used")}: {(ev.payload as { model_used: string }).model_used}</span>
                                              ) : null}
                                              {ev.payload && typeof ev.payload === "object" && ev.event_type === "task_decomposed" ? (
                                                (() => {
                                                  const p = ev.payload as {
                                                    decompose_model_task_type?: string;
                                                    decompose_reason?: string;
                                                    decompose_attempt?: string;
                                                  };
                                                  const parts = [
                                                    p.decompose_model_task_type ? `task_type=${p.decompose_model_task_type}` : null,
                                                    p.decompose_attempt ? `attempt=${p.decompose_attempt}` : null,
                                                    p.decompose_reason ? `reason=${p.decompose_reason}` : null,
                                                  ].filter(Boolean);
                                                  return parts.length > 0 ? (
                                                    <span className="chat-subagents-event-agent">
                                                      {" — "}
                                                      {parts.join(" · ")}
                                                    </span>
                                                  ) : null;
                                                })()
                                              ) : null}
                                              {ev.at && <span className="chat-subagents-event-at"> {ev.at.slice(0, 19)}</span>}
                                            </li>
                                          ))}
                                        </ul>
                                      </div>
                                    ))}
                                      </>
                                    );
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
            {Object.keys(pendingHumanInput).length > 0 && !humanInputModalTaskId && (() => {
              const pendingTaskId = Object.keys(pendingHumanInput)[0];
              const pending = pendingTaskId ? pendingHumanInput[pendingTaskId] : null;
              if (!pending || !pendingTaskId) return null;
              return (
                <div ref={chatInlineReplyRef} className="chat-inline-human-reply" role="form" aria-labelledby="inline-reply-label">
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
              {voiceStatus?.stt_configured && (
                <button
                  type="button"
                  className={`chat-voice-btn ${voiceRecording ? "recording" : ""}`}
                  onClick={handleVoiceMessageToggle}
                  disabled={loading}
                  aria-label={voiceRecording ? "Arrêter l'enregistrement et envoyer" : "Message vocal (enregistrer puis ré-encliquer pour envoyer)"}
                  title={voiceRecording ? "Arrêter et envoyer" : "Message vocal"}
                >
                  {voiceRecording ? (
                    <span className="chat-voice-btn-inner">● Enregistrement…</span>
                  ) : (
                    <span className="chat-voice-btn-inner" aria-hidden>🎤</span>
                  )}
                </button>
              )}
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
                onClick={() => handleSend()}
                disabled={loading || (!message.trim() && attachments.length === 0)}
                aria-label="Envoyer le message"
              >
                Envoyer
              </button>
            </div>
            <p id="send-hint" className="hint sr-only">
              Entrée pour envoyer
            </p>
          </section>
        )}

        {tab === "scheduled" && (
          <section
            id="panel-scheduled"
            role="tabpanel"
            aria-labelledby="tab-scheduled"
            className="panel scheduled-panel"
          >
            <h2 className="panel-title">{t("tabs.scheduled")}</h2>
            <p className="panel-description">{t("scheduled.description")}</p>
            <div className="scheduled-scroll" role="region" aria-label={t("tabs.scheduled")}>
            {scheduleReports.length === 0 ? (
              <p className="scheduled-empty">{t("scheduled.empty")}</p>
            ) : (
              <div className="scheduled-reports">
                {scheduleReports.map((r, i) => (
                  <div key={`report-${i}`} className="message system report scheduled-report">
                    <span className="role" aria-hidden>{t("scheduled.role")}</span>
                    {r.ended_at && (
                      <time className="scheduled-report-time" dateTime={r.ended_at}>
                        {new Date(r.ended_at).toLocaleString()}
                      </time>
                    )}
                    <div className="text markdown-rendered">
                      <Suspense fallback={<span className="markdown-rendered">…</span>}>
                        <LazyMarkdownContent>
                          {`**« ${r.schedule_name} »** — ${r.message}`}
                        </LazyMarkdownContent>
                      </Suspense>
                    </div>
                  </div>
                ))}
              </div>
            )}
            </div>
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
                <div
                  className={
                    "activity-tasks-block task-panel-section " +
                    (taskPanelSections.list ? "task-panel-section--open" : "task-panel-section--closed")
                  }
                >
                  <div className="task-panel-section-head">
                    <button
                      type="button"
                      className="task-panel-section-toggle"
                      aria-expanded={taskPanelSections.list}
                      aria-controls="task-panel-body-list"
                      onClick={() => toggleTaskPanelSection("list")}
                      aria-label={
                        (taskPanelSections.list ? t("tasks.section_collapse") : t("tasks.section_expand")) +
                        ": " +
                        t("tasks.list_heading")
                      }
                    >
                      <span className="task-panel-chevron" aria-hidden>
                        {taskPanelSections.list ? "▼" : "▶"}
                      </span>
                    </button>
                    <h3 className="task-panel-section-title" id="task-panel-heading-list">
                      {t("tasks.list_heading")}
                    </h3>
                  </div>
                  {taskPanelSections.list && (
                    <div
                      id="task-panel-body-list"
                      role="region"
                      aria-labelledby="task-panel-heading-list"
                      className="task-panel-section-body"
                    >
                      {tasksList.length > 0 && (
                        <>
                          <div className="activity-tasks-filters" role="tablist" aria-label={t("tasks.filter_label")}>
                            <button
                              type="button"
                              role="tab"
                              aria-selected={taskListFilter === "active"}
                              className={"activity-filter-tab" + (taskListFilter === "active" ? " active" : "")}
                              onClick={() => setTaskListFilter("active")}
                            >
                              {t("tasks.filter_active")}
                            </button>
                            <button
                              type="button"
                              role="tab"
                              aria-selected={taskListFilter === "completed"}
                              className={"activity-filter-tab" + (taskListFilter === "completed" ? " active" : "")}
                              onClick={() => setTaskListFilter("completed")}
                            >
                              {t("tasks.filter_completed")}
                            </button>
                          </div>
                          <div className="activity-tasks-search-wrap">
                            <input
                              type="search"
                              className="activity-tasks-search"
                              placeholder={t("tasks.search_placeholder")}
                              value={taskSearchQuery}
                              onChange={(e) => setTaskSearchQuery(e.target.value)}
                              aria-label={t("tasks.search_placeholder")}
                            />
                          </div>
                        </>
                      )}
                      {tasksList.length === 0 ? (
                        <p className="empty-state">{t("tasks.empty")}</p>
                      ) : filteredTasksList.length === 0 ? (
                        <p className="empty-state">{t("tasks.no_match_filter")}</p>
                      ) : (
                        <ul className="activity-task-cards" role="list">
                          {filteredTasksList.map((task) => {
                            const isSelected = tasksList[tasksSelected]?.id === task.id;
                            const runningChip = task.status === "running" ? runningTaskChips[task.id] : undefined;
                            const createdLabel = task.created_at ? (() => {
                              try {
                                const d = new Date(task.created_at);
                                return d.toLocaleString(undefined, { dateStyle: "short", timeStyle: "short" });
                              } catch {
                                return task.created_at;
                              }
                            })() : null;
                            return (
                              <li key={task.id} className={"activity-task-card" + (isSelected ? " selected" : "")}>
                                <div
                                  className="activity-task-card-inner"
                                  role="button"
                                  tabIndex={0}
                                  onClick={() => setTasksSelected(tasksList.findIndex((x) => x.id === task.id))}
                                  onKeyDown={(e) => {
                                    if (e.key === "Enter" || e.key === " ") {
                                      e.preventDefault();
                                      setTasksSelected(tasksList.findIndex((x) => x.id === task.id));
                                    }
                                    const idx = filteredTasksList.findIndex((x) => x.id === task.id);
                                    if (e.key === "ArrowDown" && idx < filteredTasksList.length - 1) {
                                      const next = filteredTasksList[idx + 1];
                                      setTasksSelected(tasksList.findIndex((x) => x.id === next.id));
                                    }
                                    if (e.key === "ArrowUp" && idx > 0) {
                                      const prev = filteredTasksList[idx - 1];
                                      setTasksSelected(tasksList.findIndex((x) => x.id === prev.id));
                                    }
                                  }}
                                >
                                  <div className="activity-task-card-head">
                                    <span className="activity-task-card-title" title={taskDisplayLabel(task)}>
                                      {taskDisplayLabel(task)}
                                    </span>
                                    <span className={"activity-task-status-pill status-" + task.status}>
                                      {task.status}
                                    </span>
                                  </div>
                                  {task.status === "running" && runningChip != null && (
                                    <div className="activity-task-progress">
                                      <div className="activity-task-progress-bar" style={{ width: `${runningChip.pct ?? 0}%` }} />
                                      <span className="activity-task-progress-pct">{runningChip.pct ?? 0}%</span>
                                    </div>
                                  )}
                                  <div className="activity-task-meta">
                                    <span className="activity-task-id">{t("tasks.task_id_prefix")}{task.id.slice(-8)}</span>
                                    {createdLabel && <span className="activity-task-created">{createdLabel}</span>}
                                    {task.assigned_agent && <span className="activity-task-agent">{task.assigned_agent}</span>}
                                  </div>
                                  <button
                                    type="button"
                                    className="activity-task-view-btn"
                                    onClick={(e) => { e.stopPropagation(); setTasksSelected(tasksList.findIndex((x) => x.id === task.id)); }}
                                  >
                                    {t("tasks.view_task")}
                                  </button>
                                </div>
                              </li>
                            );
                          })}
                        </ul>
                      )}
                    </div>
                  )}
                </div>
                <div
                  className={
                    "activity-events-block task-steps-section task-panel-section " +
                    (taskPanelSections.steps ? "task-panel-section--open" : "task-panel-section--closed")
                  }
                >
                  <div className="task-panel-section-head">
                    <button
                      type="button"
                      className="task-panel-section-toggle"
                      aria-expanded={taskPanelSections.steps}
                      aria-controls="task-panel-body-steps"
                      onClick={() => toggleTaskPanelSection("steps")}
                      aria-label={
                        (taskPanelSections.steps ? t("tasks.section_collapse") : t("tasks.section_expand")) +
                        ": " +
                        t("tasks.steps_title")
                      }
                    >
                      <span className="task-panel-chevron" aria-hidden>
                        {taskPanelSections.steps ? "▼" : "▶"}
                      </span>
                    </button>
                    <h3 className="task-panel-section-title" id="task-panel-heading-steps">
                      {t("tasks.steps_title")}
                    </h3>
                  </div>
                  {taskPanelSections.steps && (
                    <div
                      id="task-panel-body-steps"
                      role="region"
                      aria-labelledby="task-panel-heading-steps"
                      className="task-panel-section-body"
                    >
                      {(() => {
                        const planEv = tasksEvents.find((e) => (e.event_type === "plan_proposed" || e.event_type === "plan_committed") && e.payload && typeof e.payload === "object" && "steps" in e.payload);
                        let steps: Array<{ step_id?: string; agent_type?: string; intent_preview?: string; intent?: string; acceptance_criteria_preview?: string | null; deliverables?: string[] | null }> = planEv?.payload && typeof planEv.payload === "object" && Array.isArray((planEv.payload as { steps?: unknown }).steps)
                          ? (planEv.payload as { steps: Array<{ step_id?: string; agent_type?: string; intent_preview?: string; intent?: string; acceptance_criteria_preview?: string | null; deliverables?: string[] | null }> }).steps
                          : [];
                        if (steps.length === 0) {
                          const decomposed = tasksEvents.find((e) => e.event_type === "task_decomposed" && e.payload && typeof e.payload === "object" && "agents" in e.payload);
                          const agents = decomposed?.payload && typeof decomposed.payload === "object" && Array.isArray((decomposed.payload as { agents?: unknown }).agents)
                            ? (decomposed.payload as { agents: string[] }).agents
                            : [];
                          steps = agents.map((agent_type, i) => ({ step_id: `s${i}`, agent_type, intent_preview: "" }));
                        }
                        return steps.length > 0 ? (
                          <div className="chat-subagents-plan task-panel-plan" role="region" aria-label={t("events.plan_proposed")}>
                            <h4 className="chat-subagents-plan-title">{t("events.plan_proposed")}</h4>
                            <ol className="chat-subagents-plan-steps">
                              {steps.map((s, i) => (
                                <li key={s.step_id ?? i} className="chat-subagents-plan-step">
                                  {s.agent_type && <span className="chat-subagents-plan-agent">{s.agent_type}</span>}
                                  {s.step_id != null && s.step_id !== "" && (
                                    <span className="chat-subagents-plan-step-id">{s.step_id}</span>
                                  )}
                                  <span className="chat-subagents-plan-intent">{s.intent_preview || s.intent || ""}</span>
                                  {s.acceptance_criteria_preview != null && s.acceptance_criteria_preview.trim() !== "" && (
                                    <div className="chat-subagents-plan-meta">{s.acceptance_criteria_preview}</div>
                                  )}
                                  {Array.isArray(s.deliverables) && s.deliverables.length > 0 && (
                                    <ul className="chat-subagents-plan-deliverables">
                                      {s.deliverables.map((d, j) => (
                                        <li key={j}>{d}</li>
                                      ))}
                                    </ul>
                                  )}
                                </li>
                              ))}
                            </ol>
                          </div>
                        ) : null;
                      })()}
                      {taskStepsTodos.length === 0 ? (
                        <p className="empty-state task-steps-empty">{t("tasks.steps_empty")}</p>
                      ) : (
                        <ul className="task-steps-list" role="list" aria-label={t("tasks.steps_title")}>
                          {taskStepsTodos.map((step, idx) => (
                            <li key={`${step.id ?? idx}-${idx}`} className={`task-step task-step--${step.status}`}>
                              <span className="task-step-check" aria-hidden>
                                {step.status === "done" ? "☑" : step.status === "cancelled" ? "⊘" : "☐"}
                              </span>
                              <span className="task-step-title">{step.title}</span>
                              <span className="task-step-badge">
                                {step.status === "done"
                                  ? t("tasks.step_done")
                                  : step.status === "cancelled"
                                    ? t("tasks.step_cancelled")
                                    : t("tasks.step_pending")}
                              </span>
                            </li>
                          ))}
                        </ul>
                      )}
                    </div>
                  )}
                </div>
                <div
                  className={
                    "activity-events-block task-panel-section " +
                    (taskPanelSections.events ? "task-panel-section--open" : "task-panel-section--closed")
                  }
                >
                  <div className="task-panel-section-head">
                    <button
                      type="button"
                      className="task-panel-section-toggle"
                      aria-expanded={taskPanelSections.events}
                      aria-controls="task-panel-body-events"
                      onClick={() => toggleTaskPanelSection("events")}
                      aria-label={
                        (taskPanelSections.events ? t("tasks.section_collapse") : t("tasks.section_expand")) +
                        ": " +
                        t("tasks.events_title")
                      }
                    >
                      <span className="task-panel-chevron" aria-hidden>
                        {taskPanelSections.events ? "▼" : "▶"}
                      </span>
                    </button>
                    <h3 className="task-panel-section-title" id="task-panel-heading-events">
                      {t("tasks.events_title")}
                    </h3>
                  </div>
                  {taskPanelSections.events && (
                    <div
                      id="task-panel-body-events"
                      role="region"
                      aria-labelledby="task-panel-heading-events"
                      className="task-panel-section-body task-panel-events-body"
                    >
                  {tasksList.length > 0 && tasksList[tasksSelected] && (() => {
                    const sel = tasksList[tasksSelected];
                    const canCancel = sel.status === "pending" || sel.status === "running";
                    const canRetry = sel.status === "failed";
                    return (canCancel || canRetry) ? (
                      <div className="task-actions-row" role="group" aria-label="Actions sur la tâche">
                        {canCancel && (
                          <button
                            type="button"
                            className="task-action-btn task-action-cancel"
                            onClick={async () => {
                              if (!sel?.id) return;
                              try {
                                await invoke<{ cancelled?: boolean }>("cancel_task", { task_id: sel.id, port: DAEMON_PORT });
                                fetchTasksList();
                              } catch (e) {
                                console.error(e);
                              }
                            }}
                          >
                            Annuler
                          </button>
                        )}
                        {canRetry && (
                          <button
                            type="button"
                            className="task-action-btn task-action-retry"
                            onClick={() => {
                              setTab("chat");
                              setMessage(sel?.label ?? "Relance la tâche.");
                            }}
                          >
                            Relancer
                          </button>
                        )}
                      </div>
                    ) : null;
                  })()}
                  {tasksEvents.length === 0 ? (
                    <p className="empty-state">
                      {tasksList.length > 0 ? t("tasks.no_events") : t("tasks.select_task")}
                    </p>
                  ) : (
                    <>
                    <ul className="activity-events-list" role="list">
                      {tasksEvents.map((e, i) => (
                        <li key={i}>
                          <strong>{eventLabel(e.event_type)}</strong> @ {e.at}
                          {e.payload != null && typeof e.payload === "object" && (e.event_type === "task_completed" || e.event_type === "task_failed") && "model_used" in e.payload && (e.payload as { model_used?: string | null }).model_used && (
                            <p className="event-model-used">{t("tasks.model_used")}: {(e.payload as { model_used: string }).model_used}</p>
                          )}
                          {e.payload != null && (
                            <pre className="event-payload">{JSON.stringify(e.payload, null, 2)}</pre>
                          )}
                        </li>
                      ))}
                    </ul>
                    </>
                  )}
                    </div>
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
                    const openCellDetail = (slotLabel: string, slotKey: string, cellEvents: CalendarGridEvent[]) => {
                      setCalendarCellDetail({ slotKey, slotLabel, events: cellEvents });
                    };
                    if (calendarGridView === "day") {
                      const byHour: Record<number, typeof events> = {};
                      for (let h = 0; h < 24; h++) byHour[h] = [];
                      events.forEach((ev) => {
                        const date = new Date(ev.at);
                        const h = date.getHours();
                        byHour[h].push(ev);
                      });
                      const gd = calendarGridDate;
                      const todayKey = `${gd.getFullYear()}-${String(gd.getMonth() + 1).padStart(2, "0")}-${String(gd.getDate()).padStart(2, "0")}`;
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
                                  <div className="calendar-cell-content">
                                    <div className="calendar-cell-inner">
                                      <ul className="calendar-grid-slot-events" role="list">
                                        {(calendarGridDedupedBySlot.byHour.get(`${todayKey}-${h}`) ?? []).slice(0, 2).map(({ representative: e, count }, i) => (
                                          <li key={i} className={`calendar-event-block ${calendarGetEventStatusClass(e.status)}`} title={`${e.type} — ${e.status}`} role="button" tabIndex={0} onClick={() => setCalendarSelectedTaskId(e.task_id)} onKeyDown={(ev) => { if (ev.key === "Enter" || ev.key === " ") { ev.preventDefault(); setCalendarSelectedTaskId(e.task_id); } }}>
                                            <span className="calendar-event-label">{calendarEventLabel(e)}{count > 1 ? ` (${count})` : ""}</span>
                                            <span className="calendar-event-time">{new Date(e.at).toLocaleTimeString("fr-FR", { hour: "2-digit", minute: "2-digit" })}</span>
                                          </li>
                                        ))}
                                      </ul>
                                    </div>
                                    <button type="button" className="calendar-cell-view-all" onClick={() => openCellDetail(`${h}h00`, `day-${h}`, byHour[h])}>Voir tout ({byHour[h].length})</button>
                                  </div>
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
                        const key = `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, "0")}-${String(date.getDate()).padStart(2, "0")}`;
                        byDay[key] = [];
                        dayKeys.push(key);
                      }
                      events.forEach((ev) => {
                        const d = new Date(ev.at);
                        const key = `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
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
                                {dayKeys.map((key) => {
                                    const cellEvents = (byDay[key] ?? []).filter((e) => new Date(e.at).getHours() === hour);
                                    const slotLabel = `${key} ${hour}h`;
                                    return (
                                      <td key={key} className="calendar-grid-cell-day">
                                        <div className="calendar-cell-content">
                                          <div className="calendar-cell-inner">
                                            <ul className="calendar-grid-slot-events" role="list">
                                              {(calendarGridDedupedBySlot.byHour.get(`${key}-${hour}`) ?? []).slice(0, 2).map(({ representative: e, count }, i) => (
                                                <li key={i} className={`calendar-event-block ${calendarGetEventStatusClass(e.status)}`} title={`${e.type} — ${e.status}`} role="button" tabIndex={0} onClick={() => setCalendarSelectedTaskId(e.task_id)} onKeyDown={(ev) => { if (ev.key === "Enter" || ev.key === " ") { ev.preventDefault(); setCalendarSelectedTaskId(e.task_id); } }}>
                                                  <span className="calendar-event-label">{calendarEventLabel(e)}{count > 1 ? ` (${count})` : ""}</span>
                                                  <span className="calendar-event-time">{new Date(e.at).toLocaleTimeString("fr-FR", { hour: "2-digit", minute: "2-digit" })}</span>
                                                </li>
                                              ))}
                                            </ul>
                                          </div>
                                          <button type="button" className="calendar-cell-view-all" onClick={() => openCellDetail(slotLabel, `${key}-${hour}`, cellEvents)}>Voir tout ({cellEvents.length})</button>
                                        </div>
                                      </td>
                                    );
                                  })}
                              </tr>
                            ))}
                          </tbody>
                        </table>
                      );
                    }
                    const byDay: Record<string, CalendarGridEvent[]> = {};
                    events.forEach((ev) => {
                      const d = new Date(ev.at);
                      const key = `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
                      if (!byDay[key]) byDay[key] = [];
                      byDay[key].push(ev);
                    });
                    const d = calendarGridDate;
                    const firstDay = new Date(d.getFullYear(), d.getMonth(), 1);
                    const lastDay = new Date(d.getFullYear(), d.getMonth() + 1, 0);
                    // Lun=0 … Dim=6 (ISO weekday: getDay() 0=Sun → 6, 1=Mon → 0, …)
                    const startWeekday = (firstDay.getDay() + 6) % 7;
                    const daysInMonth = lastDay.getDate();
                    const weeks: string[][] = [];
                    let week: string[] = [];
                    for (let i = 0; i < startWeekday; i++) week.push("");
                    for (let day = 1; day <= daysInMonth; day++) {
                      const date = new Date(d.getFullYear(), d.getMonth(), day);
                      const y = date.getFullYear(), m = date.getMonth(), dom = date.getDate();
                      const key = `${y}-${String(m + 1).padStart(2, "0")}-${String(dom).padStart(2, "0")}`;
                      week.push(key);
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
                              {weekRow.map((key, di) => {
                                const cellEvents = key ? (byDay[key] ?? []) : [];
                                const slotLabel = key ? new Date(key + "T12:00:00").toLocaleDateString("fr-FR", { weekday: "short", day: "numeric", month: "short" }) : "";
                                return (
                                  <td key={`${wi}-${di}`} className="calendar-grid-cell-month">
                                    {key ? (
                                      <div className="calendar-cell-content">
                                        <span className="calendar-grid-day-num">{new Date(key + "T12:00:00").getDate()}</span>
                                        <div className="calendar-cell-inner">
                                          <ul className="calendar-grid-slot-events" role="list">
                                            {(calendarGridDedupedBySlot.byDate.get(key) ?? []).slice(0, 2).map(({ representative: e, count }, i) => (
                                              <li key={i} className={`calendar-event-block ${calendarGetEventStatusClass(e.status)}`} title={`${e.type} — ${e.status}`} role="button" tabIndex={0} onClick={() => setCalendarSelectedTaskId(e.task_id)} onKeyDown={(ev) => { if (ev.key === "Enter" || ev.key === " ") { ev.preventDefault(); setCalendarSelectedTaskId(e.task_id); } }}>
                                                <span className="calendar-event-label">{calendarEventLabel(e)}{count > 1 ? ` (${count})` : ""}</span>
                                                <span className="calendar-event-time">{new Date(e.at).toLocaleTimeString("fr-FR", { hour: "2-digit", minute: "2-digit" })}</span>
                                              </li>
                                            ))}
                                          </ul>
                                        </div>
                                        <button type="button" className="calendar-cell-view-all" onClick={() => openCellDetail(slotLabel, key, cellEvents)}>Voir tout ({cellEvents.length})</button>
                                      </div>
                                    ) : null}
                                  </td>
                                );
                              })}
                            </tr>
                          ))}
                        </tbody>
                      </table>
                    );
                  })()}
                </div>
                {calendarCellDetail && (
                  <div className="calendar-detail-modal-overlay" role="dialog" aria-modal="true" aria-labelledby="calendar-cell-detail-title" onClick={() => setCalendarCellDetail(null)}>
                    <div className="calendar-detail-modal calendar-cell-detail-modal" onClick={(e) => e.stopPropagation()}>
                      <div className="calendar-detail-modal-header">
                        <h2 id="calendar-cell-detail-title">Tâches — {calendarCellDetail.slotLabel}</h2>
                        <button type="button" className="calendar-detail-modal-close" onClick={() => setCalendarCellDetail(null)} aria-label="Fermer">×</button>
                      </div>
                      <div className="calendar-detail-modal-body">
                        <ul className="calendar-cell-detail-list" style={{ listStyle: "none", margin: 0, padding: 0 }}>
                          {calendarDedupeByParent(calendarCellDetail.events).map(({ representative: e, count }, i) => (
                            <li key={i} style={{ marginBottom: "0.5rem" }}>
                              <button type="button" className={`calendar-event-block calendar-cell-detail-item ${calendarGetEventStatusClass(e.status)}`} style={{ width: "100%", textAlign: "left", cursor: "pointer" }} onClick={() => { setCalendarCellDetail(null); setCalendarSelectedTaskId(e.task_id); }}>
                                <span className="calendar-event-label">{calendarEventLabel(e)}{count > 1 ? ` (${count})` : ""}</span>
                                <span className="calendar-event-time">{new Date(e.at).toLocaleString("fr-FR", { hour: "2-digit", minute: "2-digit", day: "numeric", month: "short" })} — {e.status}</span>
                              </button>
                            </li>
                          ))}
                        </ul>
                        {calendarCellDetail.events.length === 0 && <p className="muted">Aucune tâche pour ce créneau.</p>}
                      </div>
                    </div>
                  </div>
                )}
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
                  <>
                    {calendarSchedulesSelectedForDelete.size > 0 && (
                      <div className="calendar-schedules-toolbar">
                        <button
                          type="button"
                          className="calendar-schedule-delete-selection"
                          disabled={scheduleDeleting}
                          onClick={async () => {
                            const ids = Array.from(calendarSchedulesSelectedForDelete);
                            if (ids.length === 0) return;
                            setScheduleDeleting(true);
                            const toDelete = new Set(ids);
                            try {
                              for (const id of ids) {
                                try {
                                  await invoke("delete_schedule", { scheduleId: id, port: DAEMON_PORT });
                                } catch {
                                  /* continue */
                                }
                              }
                              const cached = getCached<{ schedules: typeof schedules; taskRuns: unknown }>("calendar");
                              setSchedules((prev) => {
                                const nextSchedules = prev.filter((s) => !toDelete.has(s.id));
                                if (cached) {
                                  setCached("calendar", { ...cached, schedules: nextSchedules });
                                }
                                return nextSchedules;
                              });
                              setCalendarSchedulesSelectedForDelete(new Set());
                              if (calendarSelectedScheduleId && toDelete.has(calendarSelectedScheduleId)) {
                                setCalendarSelectedScheduleId(null);
                                setScheduleDetail(null);
                              }
                            } finally {
                              setScheduleDeleting(false);
                            }
                          }}
                        >
                          {scheduleDeleting ? "…" : t("calendar.delete_selection")} ({calendarSchedulesSelectedForDelete.size})
                        </button>
                      </div>
                    )}
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
                          className="calendar-schedule-list-item"
                        >
                          <label className="calendar-schedule-checkbox" onClick={(e) => e.stopPropagation()} title={t("calendar.delete_selection")}>
                            <input
                              type="checkbox"
                              checked={calendarSchedulesSelectedForDelete.has(s.id)}
                              onChange={(e) => {
                                setCalendarSchedulesSelectedForDelete((prev) => {
                                  const next = new Set(prev);
                                  if (e.target.checked) next.add(s.id);
                                  else next.delete(s.id);
                                  return next;
                                });
                              }}
                              aria-label={`${t("calendar.select_schedule")} ${s.name || s.id.slice(0, 8)}`}
                            />
                          </label>
                          <span className="calendar-schedule-list-label">
                            <strong>{s.name || s.id.slice(0, 8)}</strong>{" "}
                            {s.enabled ? "(activée)" : "(en pause)"}
                            {s.interval_seconds != null && ` — toutes les ${s.interval_seconds}s`}
                          </span>
                          <button
                            type="button"
                            className="calendar-schedule-delete-one"
                            title={t("calendar.delete_schedule")}
                            aria-label={t("calendar.delete_schedule")}
                            onClick={async (e) => {
                              e.stopPropagation();
                              try {
                                await invoke("delete_schedule", { scheduleId: s.id, port: DAEMON_PORT });
                                setSchedules((prev) => prev.filter((x) => x.id !== s.id));
                                setCalendarSchedulesSelectedForDelete((prev) => { const n = new Set(prev); n.delete(s.id); return n; });
                                if (calendarSelectedScheduleId === s.id) {
                                  setCalendarSelectedScheduleId(null);
                                  setScheduleDetail(null);
                                }
                                const cached = getCached<{ schedules: typeof schedules; taskRuns: unknown }>("calendar");
                                if (cached) {
                                  setCached("calendar", {
                                    ...cached,
                                    schedules: cached.schedules.filter((x) => x.id !== s.id),
                                  });
                                }
                              } catch {
                                /* toast or leave list as-is */
                              }
                            }}
                          >
                            {t("settings.delete")}
                          </button>
                        </li>
                      ))}
                    </ul>
                  </>
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
                        {calendarTaskDetailError ? (
                          <p className="error-inline" role="alert">
                            {calendarTaskDetailError}
                            <br />
                            <small className="muted">Vérifiez que le daemon tourne (port {DAEMON_PORT}).</small>
                          </p>
                        ) : calendarTaskDetail ? (
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
                                  {(runStatus === "failed" || taskStatus === "failed") && (
                                    <p className="task-detail-hint">
                                      Cette tâche a échoué ou a expiré (timeout LLM ou sous-tâche).
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
                            {calendarTaskDetail.progress == null || calendarTaskDetail.progress.length === 0 ? (
                              (() => {
                                const run = taskRuns.find((r) => r.task_id === calendarSelectedTaskId);
                                const runStatus = run?.status ?? "?";
                                const taskStatus = calendarTaskDetail.status;
                                if (taskStatus === "failed" || runStatus === "failed" || (taskStatus === "running" && runStatus !== "running" && runStatus !== "queued")) {
                                  return (
                                    <p className="muted">
                                      Aucune progression enregistrée. La tâche a peut-être expiré ou échoué avant d’envoyer du contenu.
                                    </p>
                                  );
                                }
                                return null;
                              })()
                            ) : null}
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
                            <div className="schedule-prompt-edit">
                              <label htmlFor="schedule-prompt-input"><strong>Prompt (message envoyé à chaque exécution)</strong></label>
                              <textarea
                                id="schedule-prompt-input"
                                className="schedule-prompt-textarea"
                                rows={4}
                                value={scheduleEditPrompt}
                                onChange={(e) => setScheduleEditPrompt(e.target.value)}
                                placeholder="Ex: Fais un rapport de statut du dépôt GitHub…"
                              />
                              <button
                                type="button"
                                className="refresh-btn schedule-prompt-save"
                                disabled={schedulePromptSaving}
                                onClick={async () => {
                                  if (!calendarSelectedScheduleId) return;
                                  setSchedulePromptSaving(true);
                                  try {
                                    await invoke("put_schedule", {
                                      scheduleId: calendarSelectedScheduleId,
                                      port: DAEMON_PORT,
                                      body: { channel_context: scheduleEditPrompt || null },
                                    });
                                    setScheduleDetail((prev) => prev ? { ...prev, channel_context: scheduleEditPrompt || null } : null);
                                  } finally {
                                    setSchedulePromptSaving(false);
                                  }
                                }}
                              >
                                {schedulePromptSaving ? "Enregistrement…" : "Enregistrer le prompt"}
                              </button>
                            </div>
                            {scheduleDetail.rrule && (
                              <p className="schedule-rrule"><strong>Règle:</strong> <code>{scheduleDetail.rrule}</code></p>
                            )}
                            <div className="schedule-detail-actions">
                              <button
                                type="button"
                                className="calendar-schedule-delete-one schedule-detail-delete"
                                disabled={scheduleDeleting}
                                onClick={async () => {
                                  const id = calendarSelectedScheduleId;
                                  if (!id) return;
                                  setScheduleDeleting(true);
                                  try {
                                    await invoke("delete_schedule", { scheduleId: id, port: DAEMON_PORT });
                                    setSchedules((prev) => prev.filter((s) => s.id !== id));
                                    setCalendarSchedulesSelectedForDelete((prev) => { const n = new Set(prev); n.delete(id); return n; });
                                    setCalendarSelectedScheduleId(null);
                                    setScheduleDetail(null);
                                    const cached = getCached<{ schedules: { id: string }[]; taskRuns: unknown }>("calendar");
                                    if (cached) setCached("calendar", { ...cached, schedules: cached.schedules.filter((s) => s.id !== id) });
                                  } finally {
                                    setScheduleDeleting(false);
                                  }
                                }}
                              >
                                {scheduleDeleting ? "…" : t("calendar.delete_schedule")}
                              </button>
                            </div>
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
                    ) : memorySearchActive ? (
                      <div className="memory-search-wrap">
                        <div className="memory-search-bar">
                          <input
                            type="text"
                            className="memory-search-input"
                            value={memorySearchQuery}
                            onChange={(ev) => setMemorySearchQuery(ev.target.value)}
                            onKeyDown={(ev) => ev.key === "Enter" && runMemorySearch()}
                            placeholder={t("memory.search_placeholder")}
                            aria-label={t("memory.search")}
                          />
                          <button type="button" className="btn-primary" onClick={runMemorySearch} disabled={memorySearchLoading || !memorySearchQuery.trim()}>
                            {memorySearchLoading ? t("common.loading") : t("memory.search")}
                          </button>
                          <button type="button" className="btn-secondary" onClick={() => { setMemorySearchActive(false); setMemorySearchQuery(""); setMemorySearchResults([]); }}>
                            {t("memory.search_back")}
                          </button>
                        </div>
                        {memorySearchResults.length === 0 ? (
                          <p className="muted">Saisir une requête puis Rechercher. Aucun résultat pour l’instant.</p>
                        ) : (
                          <ul className="memory-long-term-list">
                            {memorySearchResults.map((r, i) => (
                              <li key={r.id ?? i} className="memory-long-term-item">
                                <div className="memory-long-term-body">
                                  <div className="memory-long-term-content">{r.content}</div>
                                  <div className="memory-long-term-meta">id: {r.id}</div>
                                </div>
                              </li>
                            ))}
                          </ul>
                        )}
                      </div>
                    ) : memoryViewGraph ? (
                      <div className="memory-graph-wrap" role="region" aria-label={t("memory.view_graph")}>
                        <div className="memory-graph-toolbar">
                          <button type="button" className="btn-secondary" onClick={() => setMemoryViewGraph(false)}>{t("memory.view_list")}</button>
                          <button type="button" className="btn-secondary" onClick={rebuildMemoryRelations} disabled={memoryRebuildLoading}>{memoryRebuildLoading ? t("common.loading") : t("memory.rebuild_relations")}</button>
                          <span className="memory-graph-hint">{t("memory.detail_click_hint")}</span>
                          {memoryLongTerm.length < memoryLongTermTotal && (
                            <button type="button" className="btn-secondary memory-load-more-btn" onClick={loadMoreMemoryLongTerm} disabled={memoryLongTermLoadingMore}>
                              {memoryLongTermLoadingMore ? t("common.loading") : t("memory.load_more")} ({memoryLongTerm.length} / {memoryLongTermTotal})
                            </button>
                          )}
                          {memoryRebuildMessage != null && <span className="memory-rebuild-msg">{memoryRebuildMessage}</span>}
                        </div>
                        {memoryLongTerm.length === 0 ? (
                          <p className="empty-state">{t("memory.long_empty")}</p>
                        ) : (() => {
                          const graphData = buildMemoryGraphData(memoryLongTerm, memoryLongTermSelected, GRAPH_THEME_COLORS[theme].nodeType, GRAPH_LINE_COLOR[theme]);
                          if (!graphData) {
                            return <p className="muted">Aucune donnée pour le graphe.</p>;
                          }
                          return (
                            <div className="memory-graph-canvas">
                              <RelationGraph
                                ref={memoryGraphRef}
                                options={memoryGraphOptions}
                                onNodeClick={(node: RGNode) => {
                                  const now = Date.now();
                                  const last = memoryGraphLastClickRef.current;
                                  const isDoubleClick = last && last.nodeId === node.id && now - last.at < 400;
                                  if (isDoubleClick) {
                                    memoryGraphLastClickRef.current = null;
                                    const idx = memoryLongTerm.findIndex((e) => e.id === node.id);
                                    const entry = idx >= 0 ? memoryLongTerm[idx]! : { id: node.id, content: "", created_at: "", source: "" };
                                    setMemoryGraphDetail({ entry, index: idx });
                                    return;
                                  }
                                  memoryGraphLastClickRef.current = { nodeId: node.id, at: now };
                                  memoryGraphRef.current?.getInstance?.()?.focusNodeById(node.id);
                                  const idx = memoryLongTerm.findIndex((e) => e.id === node.id);
                                  if (idx >= 0) setMemoryLongTermSelected(idx);
                                }}
                              />
                            </div>
                          );
                        })()}
                      </div>
                    ) : memoryLongTerm.length === 0 ? (
                      <p className="empty-state">{t("memory.long_empty")}</p>
                    ) : (
                      <div className="memory-list-scroll">
                        <div className="memory-long-toolbar">
                          <button type="button" className="btn-secondary" onClick={() => setMemorySearchActive(true)}>{t("memory.search")}</button>
                          <button type="button" className="btn-secondary" onClick={() => setMemoryViewGraph(true)}>{t("memory.view_graph")}</button>
                          <button type="button" className="btn-secondary" onClick={rebuildMemoryRelations} disabled={memoryRebuildLoading}>{memoryRebuildLoading ? t("common.loading") : t("memory.rebuild_relations")}</button>
                          {memoryLongTerm.length < memoryLongTermTotal && (
                            <button type="button" className="btn-secondary memory-load-more-btn" onClick={loadMoreMemoryLongTerm} disabled={memoryLongTermLoadingMore}>
                              {memoryLongTermLoadingMore ? t("common.loading") : t("memory.load_more")} ({memoryLongTerm.length} / {memoryLongTermTotal})
                            </button>
                          )}
                          {memoryRebuildMessage != null && <span className="memory-rebuild-msg">{memoryRebuildMessage}</span>}
                        </div>
                        <ul className="memory-long-term-list">
                          {memoryLongTerm.map((e, i) => (
                            <li
                              key={e.id ?? `entry-${i}`}
                              className={"memory-long-term-item" + (i === memoryLongTermSelected ? " selected" : "")}
                              onClick={() => setMemoryLongTermSelected(i)}
                            >
                              <div className="memory-long-term-body">
                                <div className="memory-long-term-content">{e.content}</div>
                                <div className="memory-long-term-meta">
                                  {e.created_at} {e.source ? ` · ${e.source}` : ""}
                                </div>
                                {e.related && e.related.length > 0 && (
                                  <div className="memory-long-term-related">
                                    → {t("memory.related")}: {e.related.map((r) => {
                                      const content = memoryLongTerm.find((x) => x.id === r.id);
                                      return content ? `${content.content.slice(0, 30)}… (${r.kind || "related"})` : `${r.id.slice(0, 8)} (${r.kind || "related"})`;
                                    }).join(", ")}
                                  </div>
                                )}
                              </div>
                              {e.id != null && (
                                <button
                                  type="button"
                                  className="memory-long-term-delete"
                                  onClick={async (ev) => {
                                    ev.stopPropagation();
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

        {tab === "memory" && memoryGraphDetail && (
          <div className="memory-detail-overlay" role="dialog" aria-modal="true" aria-labelledby="memory-detail-title">
            <div className="memory-detail-modal">
              <h2 id="memory-detail-title" className="memory-detail-title">{t("memory.detail_title")}</h2>
              {memoryGraphDetail.entry.content ? (
                <>
                  <div className="memory-detail-meta">{memoryGraphDetail.entry.created_at}{memoryGraphDetail.entry.source ? ` · ${memoryGraphDetail.entry.source}` : ""}</div>
                  <div className="memory-detail-content">{memoryGraphDetail.entry.content}</div>
                </>
              ) : (
                <p className="muted">ID: {memoryGraphDetail.entry.id}. {t("memory.detail_referenced_only")}</p>
              )}
              <div className="memory-detail-links-section">
                <h3 className="memory-detail-links-title">{t("memory.detail_links_title")}</h3>
                {memoryGraphDetail.entry.related && memoryGraphDetail.entry.related.length > 0 ? (
                  <ul className="memory-detail-links-list">
                    {memoryGraphDetail.entry.related.map((r) => {
                      const targetInList = memoryLongTerm.find((e) => e.id === r.id);
                      return (
                        <li key={r.id} className="memory-detail-link-item">
                          <span className="memory-detail-link-type">{t("memory.detail_link_type")}: {r.kind ?? "related"}</span>
                          <span className="memory-detail-link-id">ID: {r.id}</span>
                          {targetInList ? (
                            <p className="memory-detail-link-preview">{t("memory.detail_link_in_list")}: {targetInList.content.slice(0, 80)}{targetInList.content.length > 80 ? "…" : ""}</p>
                          ) : (
                            <p className="memory-detail-link-missing">{t("memory.detail_link_not_in_list")}</p>
                          )}
                        </li>
                      );
                    })}
                  </ul>
                ) : (
                  <p className="memory-detail-links-none">{t("memory.detail_links_none")}</p>
                )}
              </div>
              <div className="memory-detail-actions">
                {memoryGraphDetail.index >= 0 && (
                  <button type="button" className="btn-primary" onClick={() => { setMemoryLongTermSelected(memoryGraphDetail!.index); setMemoryViewGraph(false); setMemoryGraphDetail(null); }}>
                    {t("memory.view_in_list")}
                  </button>
                )}
                <button type="button" className="btn-secondary" onClick={() => setMemoryGraphDetail(null)}>{t("memory.close")}</button>
              </div>
            </div>
          </div>
        )}

        {tab === "settings" && (
          <section
            id="panel-settings"
            role="tabpanel"
            aria-labelledby="tab-settings"
            className="panel settings-panel"
          >
            <div className="settings-panel-header">
              <h2 className="panel-title settings-panel-title">{t("settings.title")}</h2>
              <nav className="settings-tabs" role="tablist" aria-label={t("settings.sections_label")}>
                <button role="tab" aria-selected={settingsSection === "display"} className={settingsSection === "display" ? "active" : ""} onClick={() => setSettingsSection("display")}>{t("settings.section_display")}</button>
                <button role="tab" aria-selected={settingsSection === "system"} className={settingsSection === "system" ? "active" : ""} onClick={() => setSettingsSection("system")}>{t("settings.section_system")}</button>
                <button role="tab" aria-selected={settingsSection === "agent"} className={settingsSection === "agent" ? "active" : ""} onClick={() => setSettingsSection("agent")}>{t("settings.section_agent")}</button>
                <button role="tab" aria-selected={settingsSection === "user"} className={settingsSection === "user" ? "active" : ""} onClick={() => setSettingsSection("user")}>{t("settings.section_user")}</button>
                <button role="tab" aria-selected={settingsSection === "data"} className={settingsSection === "data" ? "active" : ""} onClick={() => setSettingsSection("data")}>{t("settings.section_data")}</button>
              </nav>
            </div>
            {settingsSection === "display" && (
              <div className="settings-section-content">
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
              <dt>{t("settings.user_avatar")}</dt>
              <dd>
                <input
                  ref={userAvatarFileInputRef}
                  type="file"
                  accept="image/*"
                  className="sr-only"
                  aria-hidden
                  onChange={async (e) => {
                    const file = e.target.files?.[0];
                    if (!file) return;
                    const MAX_AVATAR_BYTES = 200 * 1024;
                    if (file.size > MAX_AVATAR_BYTES) return;
                    try {
                      const { content_base64, mime_type } = await readFileAsBase64(file);
                      const dataUrl = `data:${mime_type};base64,${content_base64}`;
                      setUserAvatar(dataUrl);
                      localStorage.setItem("akasha_user_avatar", dataUrl);
                    } catch {
                      // ignore
                    }
                    e.target.value = "";
                  }}
                />
                <div className="settings-avatar-row">
                  {userAvatar ? <img src={userAvatar} alt="" className="settings-avatar-preview" /> : null}
                  <div>
                    <button type="button" className="btn-secondary" onClick={() => userAvatarFileInputRef.current?.click()}>{t("settings.user_avatar_choose")}</button>
                    {userAvatar ? <button type="button" className="btn-secondary" onClick={() => { setUserAvatar(""); try { localStorage.removeItem("akasha_user_avatar"); } catch {} }}>{t("settings.user_avatar_remove")}</button> : null}
                  </div>
                </div>
              </dd>
            </dl>
              </div>
            )}
            {settingsSection === "system" && (
              <div className="settings-section-content">
                <dl className="settings-list">
                  <dt>{t("settings.daemon_port")}</dt>
                  <dd><code>{DAEMON_PORT}</code> ({t("settings.daemon_default")})</dd>
                  <dt>{t("settings.data_dir")}</dt>
                  <dd><code>%LOCALAPPDATA%\akasha</code> (Windows) ou <code>~/.local/share/akasha</code> (Linux/macOS)</dd>
                  <dt>{t("settings.documentation")}</dt>
                  <dd>
                    <button type="button" className="settings-link-btn" onClick={() => setTab("docs")}>
                      {t("settings.open_docs_tab")}
                    </button>
                    <span className="settings-doc muted"> — {t("settings.doc_from_daemon")}</span>
                  </dd>
                </dl>
              </div>
            )}
            {settingsSection === "agent" && (
              <div className="settings-section-content">
                <p className="settings-doc muted">{t("settings.agent_profile_desc")}</p>
                {agentProfileError && <p className="error-inline" role="alert">{agentProfileError}</p>}
                <div className="settings-agent-template-row">
                  <label htmlFor="agent-profile-template">{t("settings.agent_profile_apply_template")}</label>
                  <select id="agent-profile-template" className="settings-theme-select" value="" onChange={(e) => { const idx = e.target.value ? parseInt(e.target.value, 10) : -1; e.target.value = ""; if (idx >= 0 && idx < AGENT_PROFILE_TEMPLATES.length) { const tpl = AGENT_PROFILE_TEMPLATES[idx]; setAgentProfile((p) => ({ ...p, name: tpl.name, role: tpl.role ?? "", personality: tpl.personality, rules: [...tpl.rules], can_do: [...tpl.can_do], cannot_do: [...tpl.cannot_do] })); } }}>
                    <option value="">—</option>
                    {AGENT_PROFILE_TEMPLATES.map((tpl, i) => (<option key={i} value={i}>{tpl.label}</option>))}
                  </select>
                </div>
                {agentProfileLoading && <p className="panel-loading" aria-busy="true">{t("common.loading")}</p>}
                {!agentProfileLoading && (
                  <>
                    <div className="settings-agent-subtabs" role="tablist" aria-label={t("settings.agent_profile_title")}>
                      {(["identity", "personality", "traits", "rules", "can_do", "cannot_do"] as const).map((st) => (
                        <button key={st} role="tab" aria-selected={agentProfileSubTab === st} className={agentProfileSubTab === st ? "active" : ""} onClick={() => setAgentProfileSubTab(st)}>{t(`settings.agent_subtab_${st}`)}</button>
                      ))}
                    </div>
                    <div className="settings-agent-tab-content">
                      {agentProfileSubTab === "identity" && (
                        <>
                          <dl className="settings-list">
                            <dt>{t("settings.agent_profile_name")}</dt>
                            <dd>
                              <input type="text" aria-label={t("settings.agent_profile_name")} className="settings-input" maxLength={AGENT_PROFILE_LIMITS.name} value={agentProfile.name} onChange={(e) => setAgentProfile((p) => ({ ...p, name: e.target.value.slice(0, AGENT_PROFILE_LIMITS.name) }))} placeholder="Akasha" />
                              <span className="settings-char-count">{agentProfile.name.length} / {AGENT_PROFILE_LIMITS.name}</span>
                            </dd>
                            <dt>{t("settings.agent_profile_role")}</dt>
                            <dd>
                              <input type="text" aria-label={t("settings.agent_profile_role")} className="settings-input" maxLength={AGENT_PROFILE_LIMITS.role} value={agentProfile.role} onChange={(e) => setAgentProfile((p) => ({ ...p, role: e.target.value.slice(0, AGENT_PROFILE_LIMITS.role) }))} placeholder="e.g. joyful assistant, clever assistant" />
                              <span className="settings-char-count">{agentProfile.role.length} / {AGENT_PROFILE_LIMITS.role}</span>
                            </dd>
                            <dt>{t("settings.agent_profile_gender")}</dt>
                            <dd>
                              <select aria-label={t("settings.agent_profile_gender")} className="settings-theme-select" value={agentProfile.gender} onChange={(e) => setAgentProfile((p) => ({ ...p, gender: e.target.value }))}>
                                <option value="">—</option>
                                <option value="male">{t("settings.gender_male")}</option>
                                <option value="female">{t("settings.gender_female")}</option>
                                <option value="neutral">{t("settings.gender_neutral")}</option>
                              </select>
                            </dd>
                            <dt>{t("settings.agent_profile_avatar")}</dt>
                            <dd>
                              <input
                                ref={agentAvatarFileInputRef}
                                type="file"
                                accept="image/*"
                                className="sr-only"
                                aria-hidden
                                onChange={async (e) => {
                                  const file = e.target.files?.[0];
                                  if (!file) return;
                                  const MAX_AVATAR_BYTES = 200 * 1024;
                                  if (file.size > MAX_AVATAR_BYTES) { setAgentProfileError(`Image trop grande (max ${MAX_AVATAR_BYTES / 1024} Ko).`); e.target.value = ""; return; }
                                  try {
                                    const { content_base64, mime_type } = await readFileAsBase64(file);
                                    const dataUrl = `data:${mime_type};base64,${content_base64}`;
                                    setAgentProfile((p) => ({ ...p, avatar: dataUrl }));
                                    setAgentProfileError(null);
                                  } catch (err) { setAgentProfileError(String(err)); }
                                  e.target.value = "";
                                }}
                              />
                              <div className="settings-avatar-row">
                                {agentProfile.avatar ? <img src={agentProfile.avatar} alt="" className="settings-avatar-preview" /> : null}
                                <div>
                                  <button type="button" className="btn-secondary" onClick={() => agentAvatarFileInputRef.current?.click()}>{t("settings.agent_profile_avatar_choose")}</button>
                                  {agentProfile.avatar ? <button type="button" className="btn-secondary" onClick={() => setAgentProfile((p) => ({ ...p, avatar: "" }))}>{t("settings.agent_profile_avatar_remove")}</button> : null}
                                </div>
                              </div>
                            </dd>
                          </dl>
                        </>
                      )}
                      {agentProfileSubTab === "personality" && (
                        <dl className="settings-list">
                          <dt>{t("settings.agent_profile_personality")}</dt>
                          <dd>
                            <textarea aria-label={t("settings.agent_profile_personality")} className="settings-textarea" rows={8} maxLength={AGENT_PROFILE_LIMITS.personality} value={agentProfile.personality} onChange={(e) => setAgentProfile((p) => ({ ...p, personality: e.target.value.slice(0, AGENT_PROFILE_LIMITS.personality) }))} placeholder={t("settings.agent_profile_personality")} />
                            <span className="settings-char-count">{agentProfile.personality.length} / {AGENT_PROFILE_LIMITS.personality}</span>
                          </dd>
                        </dl>
                      )}
                      {agentProfileSubTab === "traits" && (
                        <div className="settings-list">
                          <p className="settings-doc muted">{t("settings.agent_traits_desc")}</p>
                          <dl className="settings-list">
                            <dt>{t("settings.agent_preferred_mode")}</dt>
                            <dd>
                              <select
                                aria-label={t("settings.agent_preferred_mode")}
                                className="settings-theme-select"
                                value={agentProfile.preferred_mode || "assistant"}
                                onChange={(e) => setAgentProfile((p) => ({ ...p, preferred_mode: e.target.value || "" }))}
                              >
                                <option value="">—</option>
                                {PREFERRED_MODES.map((m) => (
                                  <option key={m} value={m}>{t(`settings.agent_preferred_mode_${m}`)}</option>
                                ))}
                              </select>
                            </dd>
                          </dl>
                          {TRAIT_KEYS.map((key) => (
                            <dl key={key} className="settings-list">
                              <dt>{t(`settings.trait_${key}`)}</dt>
                              <dd className="settings-trait-row">
                                <input
                                  type="range"
                                  min={0}
                                  max={1}
                                  step={0.05}
                                  aria-label={t(`settings.trait_${key}`)}
                                  value={agentProfile.traits_override[key] ?? 0.5}
                                  onChange={(e) => {
                                    const v = parseFloat(e.target.value);
                                    setAgentProfile((p) => ({
                                      ...p,
                                      traits_override: { ...p.traits_override, [key]: v },
                                    }));
                                  }}
                                />
                                <span className="settings-trait-value">{(agentProfile.traits_override[key] ?? 0.5).toFixed(2)}</span>
                              </dd>
                            </dl>
                          ))}
                          <button
                            type="button"
                            className="btn-secondary"
                            onClick={() => setAgentProfile((p) => ({ ...p, traits_override: {}, preferred_mode: "" }))}
                          >
                            {t("settings.traits_reset")}
                          </button>
                        </div>
                      )}
                      {agentProfileSubTab === "rules" && (
                        <div className="settings-list-two-cols">
                          <div className="settings-list-add-col">
                            <label className="settings-label" htmlFor="agent-rules-add">{t("settings.agent_profile_rules")}</label>
                            <div className="settings-add-row">
                              <input
                                id="agent-rules-add"
                                type="text"
                                className="settings-input"
                                maxLength={AGENT_PROFILE_LIMITS.ruleLength}
                                value={rulesDraft}
                                onChange={(e) => setRulesDraft(e.target.value)}
                                onKeyDown={(e) => {
                                  if (e.key === "Enter") {
                                    e.preventDefault();
                                    const trimmed = rulesDraft.trim();
                                    if (trimmed && agentProfile.rules.length < AGENT_PROFILE_LIMITS.ruleCount) {
                                      setAgentProfile((p) => ({ ...p, rules: [...p.rules, trimmed.slice(0, AGENT_PROFILE_LIMITS.ruleLength)] }));
                                      setRulesDraft("");
                                    }
                                  }
                                }}
                                placeholder={t("settings.agent_add_line_placeholder")}
                                aria-label={t("settings.agent_profile_rules")}
                              />
                              <button
                                type="button"
                                className="btn-secondary settings-add-btn"
                                disabled={!rulesDraft.trim() || agentProfile.rules.length >= AGENT_PROFILE_LIMITS.ruleCount}
                                onClick={() => {
                                  const trimmed = rulesDraft.trim();
                                  if (trimmed && agentProfile.rules.length < AGENT_PROFILE_LIMITS.ruleCount) {
                                    setAgentProfile((p) => ({ ...p, rules: [...p.rules, trimmed.slice(0, AGENT_PROFILE_LIMITS.ruleLength)] }));
                                    setRulesDraft("");
                                  }
                                }}
                              >
                                {t("settings.agent_add_line")}
                              </button>
                            </div>
                            <span className="settings-char-count">{agentProfile.rules.length} / {AGENT_PROFILE_LIMITS.ruleCount} {t("settings.lines")}</span>
                          </div>
                          <div className="settings-list-list-col">
                            <p className="settings-list-col-title">{t("settings.agent_list_title")}</p>
                            {agentProfile.rules.length === 0 ? (
                              <p className="settings-list-empty">{t("settings.agent_list_empty")}</p>
                            ) : (
                              <ul className="settings-list-items" role="list">
                                {agentProfile.rules.map((line, i) => (
                                  <li key={i} className="settings-list-item">
                                    <span className="settings-list-item-text">{line}</span>
                                    <button type="button" className="settings-list-item-delete" onClick={() => setAgentProfile((p) => ({ ...p, rules: p.rules.filter((_, j) => j !== i) }))} aria-label={t("settings.agent_delete_line")}>×</button>
                                  </li>
                                ))}
                              </ul>
                            )}
                          </div>
                        </div>
                      )}
                      {agentProfileSubTab === "can_do" && (
                        <div className="settings-list-two-cols">
                          <div className="settings-list-add-col">
                            <label className="settings-label" htmlFor="agent-can-do-add">{t("settings.agent_profile_can_do")}</label>
                            <div className="settings-add-row">
                              <input
                                id="agent-can-do-add"
                                type="text"
                                className="settings-input"
                                maxLength={AGENT_PROFILE_LIMITS.canDoLength}
                                value={canDoDraft}
                                onChange={(e) => setCanDoDraft(e.target.value)}
                                onKeyDown={(e) => {
                                  if (e.key === "Enter") {
                                    e.preventDefault();
                                    const trimmed = canDoDraft.trim();
                                    if (trimmed && agentProfile.can_do.length < AGENT_PROFILE_LIMITS.canDoCount) {
                                      setAgentProfile((p) => ({ ...p, can_do: [...p.can_do, trimmed.slice(0, AGENT_PROFILE_LIMITS.canDoLength)] }));
                                      setCanDoDraft("");
                                    }
                                  }
                                }}
                                placeholder={t("settings.agent_add_line_placeholder")}
                                aria-label={t("settings.agent_profile_can_do")}
                              />
                              <button
                                type="button"
                                className="btn-secondary settings-add-btn"
                                disabled={!canDoDraft.trim() || agentProfile.can_do.length >= AGENT_PROFILE_LIMITS.canDoCount}
                                onClick={() => {
                                  const trimmed = canDoDraft.trim();
                                  if (trimmed && agentProfile.can_do.length < AGENT_PROFILE_LIMITS.canDoCount) {
                                    setAgentProfile((p) => ({ ...p, can_do: [...p.can_do, trimmed.slice(0, AGENT_PROFILE_LIMITS.canDoLength)] }));
                                    setCanDoDraft("");
                                  }
                                }}
                              >
                                {t("settings.agent_add_line")}
                              </button>
                            </div>
                            <span className="settings-char-count">{agentProfile.can_do.length} / {AGENT_PROFILE_LIMITS.canDoCount} {t("settings.lines")}</span>
                          </div>
                          <div className="settings-list-list-col">
                            <p className="settings-list-col-title">{t("settings.agent_list_title")}</p>
                            {agentProfile.can_do.length === 0 ? (
                              <p className="settings-list-empty">{t("settings.agent_list_empty")}</p>
                            ) : (
                              <ul className="settings-list-items" role="list">
                                {agentProfile.can_do.map((line, i) => (
                                  <li key={i} className="settings-list-item">
                                    <span className="settings-list-item-text">{line}</span>
                                    <button type="button" className="settings-list-item-delete" onClick={() => setAgentProfile((p) => ({ ...p, can_do: p.can_do.filter((_, j) => j !== i) }))} aria-label={t("settings.agent_delete_line")}>×</button>
                                  </li>
                                ))}
                              </ul>
                            )}
                          </div>
                        </div>
                      )}
                      {agentProfileSubTab === "cannot_do" && (
                        <div className="settings-list-two-cols">
                          <div className="settings-list-add-col">
                            <label className="settings-label" htmlFor="agent-cannot-do-add">{t("settings.agent_profile_cannot_do")}</label>
                            <div className="settings-add-row">
                              <input
                                id="agent-cannot-do-add"
                                type="text"
                                className="settings-input"
                                maxLength={AGENT_PROFILE_LIMITS.cannotDoLength}
                                value={cannotDoDraft}
                                onChange={(e) => setCannotDoDraft(e.target.value)}
                                onKeyDown={(e) => {
                                  if (e.key === "Enter") {
                                    e.preventDefault();
                                    const trimmed = cannotDoDraft.trim();
                                    if (trimmed && agentProfile.cannot_do.length < AGENT_PROFILE_LIMITS.cannotDoCount) {
                                      setAgentProfile((p) => ({ ...p, cannot_do: [...p.cannot_do, trimmed.slice(0, AGENT_PROFILE_LIMITS.cannotDoLength)] }));
                                      setCannotDoDraft("");
                                    }
                                  }
                                }}
                                placeholder={t("settings.agent_add_line_placeholder")}
                                aria-label={t("settings.agent_profile_cannot_do")}
                              />
                              <button
                                type="button"
                                className="btn-secondary settings-add-btn"
                                disabled={!cannotDoDraft.trim() || agentProfile.cannot_do.length >= AGENT_PROFILE_LIMITS.cannotDoCount}
                                onClick={() => {
                                  const trimmed = cannotDoDraft.trim();
                                  if (trimmed && agentProfile.cannot_do.length < AGENT_PROFILE_LIMITS.cannotDoCount) {
                                    setAgentProfile((p) => ({ ...p, cannot_do: [...p.cannot_do, trimmed.slice(0, AGENT_PROFILE_LIMITS.cannotDoLength)] }));
                                    setCannotDoDraft("");
                                  }
                                }}
                              >
                                {t("settings.agent_add_line")}
                              </button>
                            </div>
                            <span className="settings-char-count">{agentProfile.cannot_do.length} / {AGENT_PROFILE_LIMITS.cannotDoCount} {t("settings.lines")}</span>
                          </div>
                          <div className="settings-list-list-col">
                            <p className="settings-list-col-title">{t("settings.agent_list_title")}</p>
                            {agentProfile.cannot_do.length === 0 ? (
                              <p className="settings-list-empty">{t("settings.agent_list_empty")}</p>
                            ) : (
                              <ul className="settings-list-items" role="list">
                                {agentProfile.cannot_do.map((line, i) => (
                                  <li key={i} className="settings-list-item">
                                    <span className="settings-list-item-text">{line}</span>
                                    <button type="button" className="settings-list-item-delete" onClick={() => setAgentProfile((p) => ({ ...p, cannot_do: p.cannot_do.filter((_, j) => j !== i) }))} aria-label={t("settings.agent_delete_line")}>×</button>
                                  </li>
                                ))}
                              </ul>
                            )}
                          </div>
                        </div>
                      )}
                    </div>
                    <button type="button" className="refresh-btn" disabled={agentProfileSaving} onClick={async () => { setAgentProfileSaving(true); setAgentProfileError(null); try { const traits = Object.keys(agentProfile.traits_override).length ? agentProfile.traits_override : undefined; await invoke("post_agent_profile", { body: { name: agentProfile.name.trim().slice(0, AGENT_PROFILE_LIMITS.name) || undefined, personality: agentProfile.personality.trim().slice(0, AGENT_PROFILE_LIMITS.personality) || undefined, role: agentProfile.role.trim().slice(0, AGENT_PROFILE_LIMITS.role) || undefined, gender: (agentProfile.gender === "male" || agentProfile.gender === "female" || agentProfile.gender === "neutral") ? agentProfile.gender : undefined, avatar: agentProfile.avatar || undefined, rules: agentProfile.rules, can_do: agentProfile.can_do, cannot_do: agentProfile.cannot_do, traits_override: traits, preferred_mode: agentProfile.preferred_mode.trim() || undefined }, port: DAEMON_PORT }); } catch (err) { setAgentProfileError(String(err)); } finally { setAgentProfileSaving(false); } }}>{agentProfileSaving ? t("common.loading") : t("settings.agent_profile_save")}</button>
                  </>
                )}
              </div>
            )}
            {settingsSection === "user" && (
              <div className="settings-section-content">
                <p className="settings-doc muted">{t("settings.user_profile_desc")}</p>
                {userProfileError && <p className="error-inline" role="alert">{userProfileError}</p>}
                {userProfileLoading && <p className="panel-loading" aria-busy="true">{t("common.loading")}</p>}
                {!userProfileLoading && (
                  <>
                    <dl className="settings-list">
                      <dt>{t("settings.user_profile_first_name")}</dt>
                      <dd>
                        <input type="text" aria-label={t("settings.user_profile_first_name")} className="settings-input" maxLength={80} value={userProfile.first_name} onChange={(e) => setUserProfile((p) => ({ ...p, first_name: e.target.value.slice(0, 80) }))} placeholder="Marc" />
                      </dd>
                      <dt>{t("settings.user_profile_last_name")}</dt>
                      <dd>
                        <input type="text" aria-label={t("settings.user_profile_last_name")} className="settings-input" maxLength={80} value={userProfile.last_name} onChange={(e) => setUserProfile((p) => ({ ...p, last_name: e.target.value.slice(0, 80) }))} placeholder="Dupont" />
                      </dd>
                      <dt>{t("settings.user_profile_how_to_call")}</dt>
                      <dd>
                        <input type="text" aria-label={t("settings.user_profile_how_to_call")} className="settings-input" maxLength={80} value={userProfile.how_to_call} onChange={(e) => setUserProfile((p) => ({ ...p, how_to_call: e.target.value.slice(0, 80) }))} placeholder="Marc" />
                        <span className="settings-doc muted">{t("settings.user_profile_how_to_call_hint")}</span>
                      </dd>
                    </dl>
                    <h3 className="settings-subtitle">{t("settings.user_profile_proactive_title")}</h3>
                    <dl className="settings-list">
                      <dt>{t("settings.user_profile_proactive_enabled")}</dt>
                      <dd>
                        <label className="settings-checkbox-label">
                          <input type="checkbox" checked={userProfile.proactive_check_in_enabled} onChange={(e) => setUserProfile((p) => ({ ...p, proactive_check_in_enabled: e.target.checked }))} />
                          {t("settings.user_profile_proactive_enabled_label")}
                        </label>
                      </dd>
                      <dt>{t("settings.user_profile_proactive_interval_days")}</dt>
                      <dd>
                        <input type="number" aria-label={t("settings.user_profile_proactive_interval_days")} className="settings-input" min={0} max={365} value={userProfile.proactive_check_in_interval_days || ""} onChange={(e) => setUserProfile((p) => ({ ...p, proactive_check_in_interval_days: Math.max(0, parseInt(e.target.value, 10) || 0) }))} placeholder="1" />
                        <span className="settings-doc muted">{t("settings.user_profile_proactive_interval_hint")}</span>
                      </dd>
                    </dl>
                    <button type="button" className="refresh-btn" disabled={userProfileSaving} onClick={async () => {
                      setUserProfileSaving(true);
                      setUserProfileError(null);
                      try {
                        await invoke("post_user_profile", {
                          body: {
                            first_name: userProfile.first_name.trim() || undefined,
                            last_name: userProfile.last_name.trim() || undefined,
                            how_to_call: userProfile.how_to_call.trim() || undefined,
                            onboarding_completed: userProfile.onboarding_completed,
                            proactive_check_in_enabled: userProfile.proactive_check_in_enabled,
                            proactive_check_in_interval_days: userProfile.proactive_check_in_interval_days,
                          },
                          port: DAEMON_PORT,
                        });
                      } catch (err) {
                        setUserProfileError(String(err));
                      } finally {
                        setUserProfileSaving(false);
                      }
                    }}>{userProfileSaving ? t("common.loading") : t("settings.user_profile_save")}</button>
                  </>
                )}
              </div>
            )}
            {settingsSection === "data" && (
              <div className="settings-section-content">
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
                    contentBase64: content_base64,
                    mimeType: mime_type,
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
            <p className="settings-doc">{t("settings.config_note")}</p>
              </div>
            )}
          </section>
        )}
      </main>
          </div>
        </div>

        {rightSidebarOpen && (
          <aside className="sidebar-right" aria-label={t("sidebar.tasks_panel")}>
            <div className="sidebar-right-header">
              <h3 className="sidebar-right-title">{t("tabs.tasks")}</h3>
              <button
                type="button"
                className="sidebar-right-close"
                onClick={() => setRightSidebarOpen(false)}
                aria-label={t("sidebar.hide_tasks")}
              >
                ×
              </button>
            </div>
            <div className="sidebar-right-content">
              <button
                type="button"
                className="refresh-btn sidebar-right-refresh"
                onClick={fetchTasksList}
                disabled={tasksLoading}
              >
                {t("sidebar.refresh_tasks")}
              </button>
              {tasksList.length > 0 && (
                <>
                  <div className="sidebar-right-filters" role="tablist" aria-label={t("tasks.filter_label")}>
                    <button
                      type="button"
                      role="tab"
                      aria-selected={taskListFilter === "active"}
                      className={"sidebar-right-filter-tab" + (taskListFilter === "active" ? " active" : "")}
                      onClick={() => setTaskListFilter("active")}
                    >
                      {t("tasks.filter_active")}
                    </button>
                    <button
                      type="button"
                      role="tab"
                      aria-selected={taskListFilter === "completed"}
                      className={"sidebar-right-filter-tab" + (taskListFilter === "completed" ? " active" : "")}
                      onClick={() => setTaskListFilter("completed")}
                    >
                      {t("tasks.filter_completed")}
                    </button>
                  </div>
                  <div className="sidebar-right-search-wrap">
                    <input
                      type="search"
                      className="sidebar-right-search"
                      placeholder={t("tasks.search_placeholder")}
                      value={taskSearchQuery}
                      onChange={(e) => setTaskSearchQuery(e.target.value)}
                      aria-label={t("tasks.search_placeholder")}
                    />
                  </div>
                </>
              )}
              {tasksLoading && (
                <p className="panel-loading" aria-busy="true">{t("common.loading")}</p>
              )}
              {!tasksLoading && tasksList.length === 0 && (
                <p className="empty-state">{t("tasks.empty")}</p>
              )}
              {!tasksLoading && tasksList.length > 0 && filteredTasksList.length === 0 && (
                <p className="empty-state">{t("tasks.no_match_filter")}</p>
              )}
              {!tasksLoading && filteredTasksList.length > 0 && (
                <ul className="sidebar-right-task-list" role="list">
                  {filteredTasksList.map((task) => {
                    const isSelected = tasksList[tasksSelected]?.id === task.id;
                    const runningChip = task.status === "running" ? runningTaskChips[task.id] : undefined;
                    const createdLabel = task.created_at ? (() => {
                      try {
                        const d = new Date(task.created_at);
                        return d.toLocaleString(undefined, { dateStyle: "short", timeStyle: "short" });
                      } catch {
                        return task.created_at;
                      }
                    })() : null;
                    return (
                      <li key={task.id} className={"sidebar-right-task-card" + (isSelected ? " selected" : "")}>
                        <div
                          className="sidebar-right-task-card-inner"
                          role="button"
                          tabIndex={0}
                          onClick={() => { setTasksSelected(tasksList.findIndex((x) => x.id === task.id)); setTab("tasks"); }}
                          onKeyDown={(e) => {
                            if (e.key === "Enter" || e.key === " ") {
                              e.preventDefault();
                              setTasksSelected(tasksList.findIndex((x) => x.id === task.id));
                              setTab("tasks");
                            }
                          }}
                        >
                          <div className="sidebar-right-task-card-head">
                            <span className="sidebar-right-task-card-title" title={taskDisplayLabel(task)}>
                              {taskDisplayLabel(task)}
                            </span>
                            <span className={"sidebar-right-task-status-pill status-" + task.status}>
                              {task.status}
                            </span>
                          </div>
                          {task.status === "running" && runningChip != null && (
                            <div className="sidebar-right-task-progress">
                              <div className="sidebar-right-task-progress-bar" style={{ width: `${runningChip.pct ?? 0}%` }} />
                              <span className="sidebar-right-task-progress-pct">{runningChip.pct ?? 0}%</span>
                            </div>
                          )}
                          <div className="sidebar-right-task-meta">
                            <span className="sidebar-right-task-id">{t("tasks.task_id_prefix")}{task.id.slice(-8)}</span>
                            {createdLabel && <span className="sidebar-right-task-created">{createdLabel}</span>}
                            {task.assigned_agent && <span className="sidebar-right-task-agent">{task.assigned_agent}</span>}
                          </div>
                          <button
                            type="button"
                            className="sidebar-right-task-view-btn"
                            onClick={(e) => { e.stopPropagation(); setTasksSelected(tasksList.findIndex((x) => x.id === task.id)); setTab("tasks"); }}
                          >
                            {t("tasks.view_task")}
                          </button>
                        </div>
                      </li>
                    );
                  })}
                </ul>
              )}
            </div>
          </aside>
        )}
      </div>
    </div>
  );
}

export default App;
