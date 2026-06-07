import { useState, useEffect, useCallback, useRef, lazy, Suspense, useMemo, type CSSProperties, type MutableRefObject, type PointerEvent as ReactPointerEvent, type WheelEvent as ReactWheelEvent } from "react";
import { defaultExportBasename, exportChatPlainText, heuristicToolBatchSummary } from "./chatTranscriptExport";
import { invoke } from "@tauri-apps/api/core";
import RelationGraph from "relation-graph/react";
import type { RGJsonData, RGOptions, RGNode, RelationGraphComponent } from "relation-graph/react";
import {
  collapseStreamedProgressEvents,
  isTaskActiveStatus,
  mergeTaskEvents,
  normalizeTaskStatus,
} from "./taskEvents";
import { getCached, setCached } from "./useTabCache";
import { useI18n } from "./useI18n";
import { GeoMapView } from "./GeoMapView";
import { SystemHealthPanel } from "./SystemHealthPanel";
import { AppNavigation } from "./components/AppNavigation";
import { PermissionsBell } from "./components/PermissionsBell";
import { SteeringQueueBell } from "./components/SteeringQueueBell";
import { OnboardingWizard, readSetupWizardPending } from "./components/OnboardingWizard";
import { UserRagPanel } from "./components/UserRagPanel";
import { PluginCatalogPanel } from "./components/PluginCatalogPanel";
import { OpenClawMigrationPanel } from "./components/OpenClawMigrationPanel";
import { CalDavAccountsPanel } from "./components/CalDavAccountsPanel";
import { ToolsPolicyPanel } from "./components/ToolsPolicyPanel";
import { ConnectorsPanel } from "./components/ConnectorsPanel";
import { NotificationCenter } from "./components/NotificationCenter";
import { AppNotificationsSync } from "./components/AppNotificationsSync";
import { useNotify } from "./notifications/useNotifyOnMessage";
import { InfoTip, Tooltip } from "./components/Tooltip";
import { ChatRenderer } from "./components/ChatRenderer";
import { ChatCompositionBar } from "./components/ChatCompositionBar";
import { MonoIcon } from "./components/MonoIcon";
import { useHashRoute } from "./hooks/useHashRoute";
import { NAV_ITEMS } from "./navigation/types";
import { ComparePanel } from "./panels/ComparePanel";
import { CookbookPanel } from "./panels/CookbookPanel";
import { DeepResearchPanel } from "./panels/DeepResearchPanel";
import {
  buildMessageWithResearchContext,
  type ChatResearchContext,
} from "./chatResearchContext";
import {
  buildMessageWithNoteContext,
  type ChatNoteContext,
} from "./chatNoteContext";
import { getNote, listNotes } from "./notesApi";
import type { ResearchReportDocument } from "./researchReportExport";
import {
  buildCookbookPricingLookup,
  lookupPriceRates,
  parseUsageFromEventPayload,
  type ModelPriceRates,
  type ModelUsageStats,
} from "./modelUsage";
import { ModelUsageBadge } from "./components/ModelUsageBadge";
import { TaskExecutionSteps } from "./components/TaskExecutionSteps";
import { CreateTaskDialog } from "./components/CreateTaskDialog";
import { NotesPanel } from "./components/NotesPanel";
import { ThemeEditorPanel, applyThemeOverrides, loadThemeOverrides } from "./components/ThemeEditorPanel";
import { buildExecutionSteps } from "./tasks/buildExecutionSteps";
import { pollTaskUntilDone, type PollTaskUntilDoneDeps } from "./tasks/pollTaskUntilDone";
import { THEME_IDS, THEME_STORAGE_KEY, type ThemeId } from "./themeTypes";

export type { ThemeId } from "./themeTypes";

const LazyMarkdownContent = lazy(() => import("./MarkdownContent").then((m) => ({ default: m.default })));

const DAEMON_PORT = 3876;
/** Browser E2E (Playwright): talk to daemon over HTTP instead of Tauri. Match VITE_E2E or vite --mode e2e (npm run test:e2e). */
const E2E_WEB = import.meta.env.VITE_E2E === "true" || import.meta.env.MODE === "e2e";
/** Same-origin path proxied in vite `server`/`preview` when mode is e2e — avoids cross-port browser blocks (e.g. Chromium PNA on Windows). */
function e2eDaemonHttpUrl(path: string): string {
  const p = path.startsWith("/") ? path : `/${path}`;
  if (E2E_WEB) return `/__e2e_daemon${p}`;
  return `http://127.0.0.1:${DAEMON_PORT}${p}`;
}
const UI_MODE_STORAGE_KEY = "akasha_ui_mode";
const AKASHA_SESSION_ID_KEY = "akasha_session_id";
const TASK_TREE_COLLAPSE_STORAGE_KEY = "akasha_task_tree_collapsed";
const TASK_ORCHESTRATION_DEBUG_STORAGE_KEY = "akasha_task_orchestration_debug";
const TASK_SIDEBAR_STORAGE_KEY = "akasha_task_sidebar_open";
const CHAT_TIPS_STORAGE_KEY = "akasha_ui_chat_tips";
const CHAT_PROMPT_CHIPS_STORAGE_KEY = "akasha_ui_prompt_chips";
const CHAT_BUDDY_STORAGE_KEY = "akasha_ui_buddy";
const AKASHA_CHAT_THREADS_KEY = "akasha_chat_threads_v1";
const DENSITY_STORAGE_KEY = "akasha_ui_density";
const CHAT_AGENT_MODE_KEY = "akasha_chat_agent_mode";
const CHAT_WEB_SEARCH_KEY = "akasha_chat_web_search";
const CHAT_INCOGNITO_KEY = "akasha_chat_incognito";
const CHAT_COMPANION_OPEN_KEY = "akasha_companion_open";
const AKASHA_TASK_SESSIONS_KEY = "akasha_task_sessions_v1";

function loadTaskSessionMap(): Record<string, string> {
  try {
    const raw = localStorage.getItem(AKASHA_TASK_SESSIONS_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw) as unknown;
    if (!parsed || typeof parsed !== "object") return {};
    return parsed as Record<string, string>;
  } catch {
    return {};
  }
}

function persistTaskSession(taskId: string, sessionId: string) {
  if (!taskId.trim() || !sessionId.trim()) return;
  try {
    const map = loadTaskSessionMap();
    map[taskId] = sessionId;
    const keys = Object.keys(map);
    if (keys.length > 64) {
      for (const k of keys.slice(0, keys.length - 64)) {
        delete map[k];
      }
    }
    localStorage.setItem(AKASHA_TASK_SESSIONS_KEY, JSON.stringify(map));
  } catch {
    /* ignore */
  }
}

function isGenericTaskLabel(label: string | undefined, taskId: string): boolean {
  if (!label?.trim()) return true;
  const suffix = taskId.length >= 8 ? taskId.slice(-8) : taskId;
  return label.includes(`…${suffix}`) || label.includes(`...${suffix}`);
}

export type ChatThreadEntry = {
  id: string;
  title: string;
  createdAt: string;
  updatedAt: string;
  pendingTitle?: boolean;
  lastSnippet?: string;
  /** Optional session folder label for sidebar grouping (localStorage only). */
  folder?: string;
};

function loadChatThreadsInitial(): ChatThreadEntry[] {
  try {
    const raw = localStorage.getItem(AKASHA_CHAT_THREADS_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as ChatThreadEntry[];
      if (Array.isArray(parsed) && parsed.length > 0) return parsed;
    }
    const legacy = localStorage.getItem(AKASHA_SESSION_ID_KEY)?.trim();
    if (legacy) {
      const now = new Date().toISOString();
      return [{ id: legacy, title: "", createdAt: now, updatedAt: now }];
    }
  } catch {
    /* ignore */
  }
  return [];
}


type TaskOrchestrationDebugLevel = "minimal" | "normal" | "full";

function shouldChatStreamProgress(message: string): boolean {
  const m = message?.trim() ?? "";
  if (!m) return false;
  if (/^\s*TOOL\s*:/im.test(m)) return false;
  if (/\n\s*TOOL\s*:/i.test(m)) return false;
  return true;
}

function isChatStreamToolPhase(message: string): boolean {
  const s = message ?? "";
  return /^\s*TOOL\s*:/im.test(s.trim()) || /\n\s*TOOL\s*:/i.test(s);
}

/** Avoid replacing React state when task list data is unchanged (prevents full App re-renders / scroll reset on every get_tasks poll). */
function tasksListsEqual(
  a: Array<{ id: string; status: string; label?: string; created_at?: string; parent_task_id?: string; assigned_agent?: string }>,
  b: typeof a,
): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    const x = a[i]!;
    const y = b[i]!;
    if (
      x.id !== y.id ||
      x.status !== y.status ||
      x.label !== y.label ||
      x.created_at !== y.created_at ||
      x.parent_task_id !== y.parent_task_id ||
      x.assigned_agent !== y.assigned_agent
    ) {
      return false;
    }
  }
  return true;
}

function taskTodoRowsEqual(
  a: Array<{ id?: string | null; title: string; status: string }>,
  b: typeof a,
): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    const x = a[i]!;
    const y = b[i]!;
    if (x.id !== y.id || x.title !== y.title || x.status !== y.status) return false;
  }
  return true;
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
    backgroundColor: "#0d0d14",
    defaultNodeColor: "#1a1a2e",
    defaultNodeFontColor: "#f1f5f9",
    defaultNodeBorderColor: "#3d3666",
    defaultLineColor: "#64748b",
    defaultLineWidth: 2,
    defaultLineFontColor: "#94a3b8",
    defaultShowLineLabel: true,
    checkedLineColor: "#8b5cf6",
    nodeType: {
      entry: { color: "#2d1f4e", fontColor: "#f1f5f9" },
      related: { color: "#252538", fontColor: "#94a3b8" },
      selected: { color: "#312e4a", fontColor: "#f1f5f9", borderColor: "#a78bfa" },
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

function loadSavedUiMode(): UiMode {
  try {
    const s = localStorage.getItem(UI_MODE_STORAGE_KEY);
    if (s === "simple" || s === "expert") return s;
  } catch {
    /* ignore */
  }
  return "simple";
}

function trimPreview(text: string, max = 140): string {
  const compact = text.replace(/\s+/g, " ").trim();
  if (compact.length <= max) return compact;
  return `${compact.slice(0, Math.max(0, max - 1))}…`;
}

type EventChartSeries = {
  name: string;
  points: Array<{ x: number; y: number }>;
};

/** Parsed maps plugin route option (GeoJSON → schematic points). */
type MapRouteLeg = {
  id: string;
  label: string;
  mode?: string;
  points: Array<{ x: number; y: number }>;
  distanceM?: number;
  durationS?: number;
  steps?: Array<{ instruction: string; distance_m?: number }>;
};

type EventAdvancedView =
  | {
      kind: "map";
      title?: string;
      summary?: string;
      /** `great_circle_estimate` | `road_network` (from routing engine in plugin output). */
      geometryKind?: "great_circle_estimate" | "road_network";
      points: Array<{ x: number; y: number }>;
      distanceM?: number;
      durationS?: number;
      routes?: MapRouteLeg[];
      osmEmbedUrl?: string;
      osmBrowseUrl?: string;
      mapAttribution?: string;
    }
  | {
      kind: "graph" | "timeseries";
      title?: string;
      series: EventChartSeries[];
    };

type ChatMapVisual = Extract<EventAdvancedView, { kind: "map" }>;

function asRecord(v: unknown): Record<string, unknown> | null {
  return v && typeof v === "object" ? (v as Record<string, unknown>) : null;
}

/** GET /api/tasks/:id/events → { task_id, events } — tolerate alternate key casings after IPC. */
function normalizeTaskEventsInvokeResponse(
  data: unknown,
): Array<{ schema_version?: number; kind?: string; event_type?: string; payload?: unknown; at?: string; task_id?: string }> {
  if (data == null || typeof data !== "object") return [];
  const o = data as Record<string, unknown>;
  const raw = o.events ?? o.Events;
  if (!Array.isArray(raw)) return [];
  return raw.map((e) => {
    if (e && typeof e === "object") {
      const ev = e as Record<string, unknown>;
      const kind = (ev.kind ?? ev.Kind) as string | undefined;
      const eventType = (ev.event_type ?? ev.EventType ?? ev.eventType ?? kind) as string | undefined;
      return {
        schema_version: (ev.schema_version ?? ev.schemaVersion ?? ev.SchemaVersion) as number | undefined,
        kind,
        event_type: eventType,
        payload: ev.payload ?? ev.Payload,
        at: (ev.at ?? ev.created_at ?? ev.At) as string | undefined,
        task_id: (ev.task_id ?? ev.taskId) as string | undefined,
      };
    }
    return { event_type: "?" };
  });
}

/** Milestone payload is usually an object; DB round-trips may yield a JSON string. */
function timelineMilestonePayloadRecord(payload: unknown): Record<string, unknown> | null {
  if (payload == null) return null;
  if (typeof payload === "string") {
    try {
      return asRecord(JSON.parse(payload) as unknown);
    } catch {
      return null;
    }
  }
  return asRecord(payload);
}

function toFiniteNumber(v: unknown): number | undefined {
  const n = typeof v === "number" ? v : typeof v === "string" ? Number(v) : NaN;
  return Number.isFinite(n) ? n : undefined;
}

function toPoint(v: unknown): { x: number; y: number } | null {
  if (Array.isArray(v) && v.length >= 2) {
    const x = toFiniteNumber(v[0]);
    const y = toFiniteNumber(v[1]);
    if (x != null && y != null) return { x, y };
  }
  const r = asRecord(v);
  if (!r) return null;
  const x = toFiniteNumber(r.x ?? r.lon ?? r.lng ?? r.time ?? r.t);
  const y = toFiniteNumber(r.y ?? r.lat ?? r.value ?? r.v);
  if (x != null && y != null) return { x, y };
  return null;
}

/** GeoJSON LineString / MultiLineString → schematic points (x=lon, y=lat). */
function geojsonLineStringToPoints(geom: unknown): Array<{ x: number; y: number }> {
  const o = asRecord(geom);
  if (!o) return [];
  const typ = typeof o.type === "string" ? o.type : "";
  if (typ === "LineString") {
    const coords = o.coordinates;
    if (!Array.isArray(coords)) return [];
    return coords.map((p) => toPoint(p)).filter((p): p is { x: number; y: number } => p != null);
  }
  if (typ === "MultiLineString") {
    const coords = o.coordinates;
    if (!Array.isArray(coords)) return [];
    const out: Array<{ x: number; y: number }> = [];
    for (const line of coords) {
      if (!Array.isArray(line)) continue;
      for (const p of line) {
        const pt = toPoint(p);
        if (pt) out.push(pt);
      }
    }
    return out;
  }
  const coordsLoose = o.coordinates;
  if (Array.isArray(coordsLoose) && coordsLoose.length > 0 && Array.isArray(coordsLoose[0])) {
    return coordsLoose.map((p) => toPoint(p)).filter((p): p is { x: number; y: number } => p != null);
  }
  return [];
}

function pointsFromMapGeometryField(geometry: unknown): Array<{ x: number; y: number }> {
  if (Array.isArray(geometry)) {
    return geometry.map((p) => toPoint(p)).filter((p): p is { x: number; y: number } => p != null);
  }
  return geojsonLineStringToPoints(geometry);
}

function normalizeSeries(input: unknown): EventChartSeries[] {
  const arr = Array.isArray(input) ? input : [];
  const out: EventChartSeries[] = [];
  arr.forEach((item, idx) => {
    const r = asRecord(item);
    if (!r) return;
    const name = typeof r.name === "string" && r.name.trim() ? r.name.trim() : `series_${idx + 1}`;

    const pointsFromPoints = Array.isArray(r.points)
      ? r.points.map((p) => toPoint(p)).filter((p): p is { x: number; y: number } => p != null)
      : [];
    if (pointsFromPoints.length > 0) {
      out.push({ name, points: pointsFromPoints });
      return;
    }

    const yValues = Array.isArray(r.y) ? r.y.map((n) => toFiniteNumber(n)).filter((n): n is number => n != null) : [];
    const xValues = Array.isArray(r.x) ? r.x.map((n) => toFiniteNumber(n)).filter((n): n is number => n != null) : [];
    if (yValues.length > 0) {
      const points = yValues.map((y, i) => ({ x: xValues[i] ?? i, y }));
      out.push({ name, points });
      return;
    }

    const dataPoints = Array.isArray(r.data)
      ? r.data.map((p) => toPoint(p)).filter((p): p is { x: number; y: number } => p != null)
      : [];
    if (dataPoints.length > 0) {
      out.push({ name, points: dataPoints });
    }
  });
  return out;
}

function scalePoints(points: Array<{ x: number; y: number }>, width: number, height: number, pad = 12) {
  if (points.length === 0) return [] as Array<{ x: number; y: number }>;
  const xs = points.map((p) => p.x);
  const ys = points.map((p) => p.y);
  const minX = Math.min(...xs);
  const maxX = Math.max(...xs);
  const minY = Math.min(...ys);
  const maxY = Math.max(...ys);
  const spanX = Math.max(maxX - minX, 1e-9);
  const spanY = Math.max(maxY - minY, 1e-9);
  return points.map((p) => ({
    x: pad + ((p.x - minX) / spanX) * (width - pad * 2),
    y: height - pad - ((p.y - minY) / spanY) * (height - pad * 2),
  }));
}

function extractAdvancedViewData(payload: unknown): EventAdvancedView | null {
  const p0 = asRecord(payload);
  const candidates = [
    p0,
    asRecord(p0?.result),
    asRecord(p0?.output),
    asRecord(p0?.data),
  ].filter((c): c is Record<string, unknown> => c != null);

  for (const c of candidates) {
    const view = typeof c.view === "string" ? c.view.toLowerCase() : "";
    if (!view) continue;
    const title = typeof c.title === "string" ? c.title : undefined;

    if (view === "map") {
      const geoPoints = pointsFromMapGeometryField(c.geometry);
      const routeObj = asRecord(c.route);
      const fallbackGeo =
        routeObj && routeObj.geometry != null ? pointsFromMapGeometryField(routeObj.geometry) : [];
      const routesIn: MapRouteLeg[] = [];
      if (Array.isArray(c.routes)) {
        c.routes.forEach((raw, i) => {
          const rr = asRecord(raw);
          if (!rr) return;
          const id = typeof rr.id === "string" && rr.id.trim() ? rr.id.trim() : `route_${i}`;
          const label = typeof rr.label === "string" ? rr.label : `Option ${i + 1}`;
          const mode = typeof rr.mode === "string" ? rr.mode : undefined;
          const pts = pointsFromMapGeometryField(rr.geometry);
          const distanceM = toFiniteNumber(rr.distance_m ?? rr.distanceM);
          const durationS = toFiniteNumber(rr.duration_s ?? rr.durationS);
          let steps: MapRouteLeg["steps"];
          if (Array.isArray(rr.steps)) {
            steps = rr.steps
              .map((s) => {
                const sr = asRecord(s);
                if (!sr) return null;
                const instruction = typeof sr.instruction === "string" ? sr.instruction : "";
                if (!instruction.trim()) return null;
                const distance_m = toFiniteNumber(sr.distance_m ?? sr.distanceM);
                return distance_m != null ? { instruction, distance_m } : { instruction };
              })
              .filter((x): x is NonNullable<typeof x> => x != null);
          }
          if (pts.length >= 2 || distanceM != null) {
            routesIn.push({ id, label, mode, points: pts, distanceM, durationS, steps: steps?.length ? steps : undefined });
          }
        });
      }
      const points =
        routesIn.length > 0 && routesIn[0]!.points.length >= 2
          ? routesIn[0]!.points
          : geoPoints.length >= 2
            ? geoPoints
            : fallbackGeo;
      const distanceM = toFiniteNumber(c.distance_m ?? c.distanceM) ?? routesIn[0]?.distanceM;
      const durationS = toFiniteNumber(c.duration_s ?? c.durationS) ?? routesIn[0]?.durationS;
      const summary = typeof c.summary === "string" && c.summary.trim() ? c.summary.trim() : undefined;
      const gk = typeof c.geometry_kind === "string" ? c.geometry_kind.toLowerCase() : "";
      const geometryKind: "great_circle_estimate" | "road_network" | undefined =
        gk === "great_circle_estimate"
          ? "great_circle_estimate"
          : gk === "road_network"
            ? "road_network"
            : undefined;
      const osmEmbedUrl =
        typeof c.osm_embed_url === "string" && c.osm_embed_url.startsWith("http") ? c.osm_embed_url : undefined;
      const osmBrowseUrl =
        typeof c.osm_browse_url === "string" && c.osm_browse_url.startsWith("http") ? c.osm_browse_url : undefined;
      const mapAttribution =
        typeof c.map_attribution === "string" && c.map_attribution.trim() ? c.map_attribution.trim() : undefined;
      if (points.length >= 2 || distanceM != null || durationS != null) {
        return {
          kind: "map",
          title,
          summary,
          geometryKind,
          points,
          distanceM,
          durationS,
          routes: routesIn.length > 0 ? routesIn : undefined,
          osmEmbedUrl,
          osmBrowseUrl,
          mapAttribution,
        };
      }
    }

    if (view === "graph" || view === "timeseries") {
      const series = normalizeSeries(c.series)
        .concat(normalizeSeries(asRecord(c.figure)?.data))
        .filter((s) => s.points.length > 0);
      if (series.length > 0) {
        return { kind: view, title, series };
      }
    }
  }

  return null;
}

type ChatMessageRow = {
  role: "user" | "assistant" | "system";
  text: string;
  error?: boolean;
  streaming?: boolean;
  taskId?: string;
  mapVisual?: ChatMapVisual;
  usage?: ModelUsageStats;
};

function findLastChatAssistantIndex(messages: ChatMessageRow[], taskId: string): number {
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    if (m?.role === "assistant" && m.taskId === taskId) return i;
  }
  return -1;
}

function parsePluginToolResultBody(raw: string): unknown | null {
  const s = raw.trim();
  const m = s.match(/^\[plugin:[^\]]+\]\s*([\s\S]*)$/);
  const jsonStr = ((m ? m[1] : s) ?? "").trim();
  if (!jsonStr) return null;
  try {
    return JSON.parse(jsonStr) as unknown;
  } catch {
    return null;
  }
}

/** Build OSM embed + browse URLs from route vertices (lon, lat in `x`/`y`). */
function osmUrlsFromLonLatPoints(points: Array<{ x: number; y: number }>): { embed: string; browse: string } | undefined {
  if (points.length < 2) return undefined;
  const lons = points.map((p) => p.x);
  const lats = points.map((p) => p.y);
  let minLon = Math.min(...lons);
  let maxLon = Math.max(...lons);
  let minLat = Math.min(...lats);
  let maxLat = Math.max(...lats);
  let lonSpan = maxLon - minLon;
  let latSpan = maxLat - minLat;
  const minSpan = 1e-4;
  if (lonSpan < minSpan) {
    const half = minSpan / 2;
    const mid = (minLon + maxLon) / 2;
    minLon = mid - half;
    maxLon = mid + half;
    lonSpan = minSpan;
  }
  if (latSpan < minSpan) {
    const half = minSpan / 2;
    const mid = (minLat + maxLat) / 2;
    minLat = mid - half;
    maxLat = mid + half;
    latSpan = minSpan;
  }
  const padX = lonSpan * 0.1;
  const padY = latSpan * 0.1;
  const west = minLon - padX;
  const south = minLat - padY;
  const east = maxLon + padX;
  const north = maxLat + padY;
  const embed = `https://www.openstreetmap.org/export/embed.html?bbox=${west},${south},${east},${north}&layer=mapnik`;
  const browse = `https://www.openstreetmap.org/?minlat=${south}&minlon=${west}&maxlat=${north}&maxlon=${east}`;
  return { embed, browse };
}

/** When `result_full` is truncated (invalid JSON) or IPC clips the string, recover map metrics + polyline from the readable prefix. */
function salvageMapVisualFromPluginPrefix(raw: string): ChatMapVisual | null {
  const s = raw.trim();
  const plug = s.match(/^\[plugin:[^\]]+\]\s*/);
  const body = plug ? s.slice(plug[0].length) : s;
  if (!body.startsWith("{")) return null;
  if (!/"view"\s*:\s*"map"/i.test(body)) return null;

  const numField = (key: string): number | undefined => {
    const re = new RegExp(`"${key}"\\s*:\\s*([0-9.+-eE]+)`);
    const mm = body.match(re);
    if (!mm) return undefined;
    const n = Number(mm[1]);
    return Number.isFinite(n) ? n : undefined;
  };
  const distanceM = numField("distance_m") ?? numField("distanceM");
  const durationS = numField("duration_s") ?? numField("durationS");

  // Only scan the primary `geometry` + `steps` region. Including `"routes":[...]` would merge every
  // alternate leg (corridor, train) into one bogus polyline (good start, wrong end).
  const routesKey = body.search(/"routes"\s*:\s*\[/);
  const coordSource = routesKey >= 0 ? body.slice(0, routesKey) : body;

  const coordRe = /\[\s*(-?\d+(?:\.\d+)?)\s*,\s*(-?\d+(?:\.\d+)?)\s*\]/g;
  const points: Array<{ x: number; y: number }> = [];
  let cm: RegExpExecArray | null;
  while ((cm = coordRe.exec(coordSource)) !== null) {
    const x = Number(cm[1]);
    const y = Number(cm[2]);
    if (Number.isFinite(x) && Number.isFinite(y)) points.push({ x, y });
  }

  let summary: string | undefined;
  const sm = body.match(/"summary"\s*:\s*"((?:[^"\\]|\\.)*)"/);
  if (sm?.[1]) {
    summary = sm[1].replace(/\\n/g, "\n").replace(/\\"/g, '"').replace(/\\\\/g, "\\").trim();
  }

  let geometryKind: "great_circle_estimate" | "road_network" | undefined;
  if (/"geometry_kind"\s*:\s*"road_network"/.test(body)) {
    geometryKind = "road_network";
  } else if (/"geometry_kind"\s*:\s*"great_circle_estimate"/.test(body)) {
    geometryKind = "great_circle_estimate";
  }

  const osmEmbedM = body.match(/"osm_embed_url"\s*:\s*"([^"]+)"/);
  const osmBrowseM = body.match(/"osm_browse_url"\s*:\s*"([^"]+)"/);
  let osmEmbedUrl =
    osmEmbedM?.[1]?.startsWith("http") ? osmEmbedM[1] : undefined;
  let osmBrowseUrl =
    osmBrowseM?.[1]?.startsWith("http") ? osmBrowseM[1] : undefined;
  if (!osmEmbedUrl && points.length >= 2) {
    const syn = osmUrlsFromLonLatPoints(points);
    if (syn) {
      osmEmbedUrl = syn.embed;
      osmBrowseUrl = osmBrowseUrl ?? syn.browse;
    }
  }

  if (points.length >= 2 || distanceM != null || durationS != null) {
    return {
      kind: "map",
      summary: summary || undefined,
      geometryKind,
      points,
      distanceM,
      durationS,
      osmEmbedUrl,
      osmBrowseUrl,
    };
  }
  return null;
}

function extractChatMapVisualFromTaskEvents(
  events: Array<{ event_type?: string; payload?: unknown }>,
): ChatMapVisual | null {
  for (let i = events.length - 1; i >= 0; i--) {
    const e = events[i];
    const et = e.event_type ?? "";
    if (et !== "timeline_milestone" && et !== "deterministic_preferred_tool_result") continue;
    const p = timelineMilestonePayloadRecord(e.payload);
    if (!p || p.name !== "deterministic_preferred_tool_result") continue;
    if (p.success === false || p.success === 0 || p.success === "false") continue;
    const tool = typeof p.tool === "string" ? p.tool : "";
    if (!tool.startsWith("maps_")) continue;
    const full =
      typeof p.result_full === "string"
        ? p.result_full
        : typeof p.result_preview === "string"
          ? p.result_preview
          : null;
    if (!full) continue;
    const parsed = parsePluginToolResultBody(full);
    const vis = parsed ? extractAdvancedViewData(parsed) : null;
    if (vis?.kind === "map") return vis;
    const salvaged = salvageMapVisualFromPluginPrefix(full);
    if (salvaged) return salvaged;
  }
  return null;
}

/** When the model pastes raw plugin JSON in the reply, still show the map (no milestone / truncated preview). */
function extractChatMapVisualFromAssistantText(text: string): ChatMapVisual | null {
  const idx = text.indexOf("[plugin:");
  if (idx < 0) return null;
  const slice = text.slice(idx);
  const parsed = parsePluginToolResultBody(slice);
  const vis = parsed ? extractAdvancedViewData(parsed) : null;
  if (vis?.kind === "map") return vis;
  return salvageMapVisualFromPluginPrefix(slice);
}

function simpleTextHash(s: string): string {
  const slice = s.length > 4000 ? s.slice(0, 4000) : s;
  let h = 0;
  for (let i = 0; i < slice.length; i++) h = (Math.imul(31, h) + slice.charCodeAt(i)) | 0;
  return String(h >>> 0);
}

function chatMapMessageCacheKey(sessionId: string, assistantText: string): string {
  const CACHE_VERSION = "v2";
  return `akasha_map_msg_${CACHE_VERSION}_${sessionId}_${simpleTextHash(assistantText.trim())}`;
}

function isChatMapVisualLike(o: unknown): o is ChatMapVisual {
  const r = asRecord(o);
  if (!r || r.kind !== "map" || !Array.isArray(r.points) || r.points.length < 2) return false;
  return r.points.every((p) => {
    const pr = asRecord(p);
    const x = pr ? toFiniteNumber(pr.x) : null;
    const y = pr ? toFiniteNumber(pr.y) : null;
    return x != null && y != null;
  });
}

/** Restore map UI after app reload when the assistant bubble text matches a cached snapshot (same session). */
function tryLoadCachedChatMapVisual(sessionId: string, assistantText: string): ChatMapVisual | null {
  if (!sessionId?.trim() || !assistantText?.trim()) return null;
  try {
    const raw = localStorage.getItem(chatMapMessageCacheKey(sessionId, assistantText));
    if (!raw) return null;
    const o = JSON.parse(raw) as unknown;
    return isChatMapVisualLike(o) ? o : null;
  } catch {
    return null;
  }
}

function formatDistanceLabel(meters?: number): string | null {
  if (meters == null || !Number.isFinite(meters)) return null;
  if (meters < 1000) return `${Math.round(meters)} m`;
  return `${(meters / 1000).toFixed(2)} km`;
}

function formatDurationLabel(seconds?: number): string | null {
  if (seconds == null || !Number.isFinite(seconds)) return null;
  if (seconds < 60) return `${Math.round(seconds)} s`;
  const min = Math.floor(seconds / 60);
  const sec = Math.round(seconds % 60);
  return sec > 0 ? `${min} min ${sec} s` : `${min} min`;
}

function advancedViewToCsv(visual: EventAdvancedView): string {
  if (visual.kind === "map") {
    if (visual.routes && visual.routes.length > 0) {
      const header = "route_id,route_label,index,lon,lat";
      const rows: string[] = [];
      visual.routes.forEach((rt) => {
        const safeLabel = rt.label.replace(/"/g, '""');
        rt.points.forEach((p, idx) => {
          rows.push(`${rt.id},"${safeLabel}",${idx},${p.x},${p.y}`);
        });
      });
      return [header, ...rows].join("\n");
    }
    const header = "index,lon,lat";
    const rows = visual.points.map((p, idx) => `${idx},${p.x},${p.y}`);
    return [header, ...rows].join("\n");
  }
  const header = "series,index,x,y";
  const rows: string[] = [];
  visual.series.forEach((series) => {
    series.points.forEach((point, idx) => {
      rows.push(`"${series.name.replace(/"/g, '""')}",${idx},${point.x},${point.y}`);
    });
  });
  return [header, ...rows].join("\n");
}

function renderAdvancedViewToCanvas(visual: EventAdvancedView, width: number, height: number): HTMLCanvasElement {
  const canvas = document.createElement("canvas");
  canvas.width = width;
  canvas.height = height;
  const ctx = canvas.getContext("2d");
  if (!ctx) return canvas;

  ctx.fillStyle = "#0d0d14";
  ctx.fillRect(0, 0, width, height);

  if (visual.kind === "map") {
    const scaled = scalePoints(visual.points, width, height);
    if (scaled.length >= 2) {
      ctx.strokeStyle = "#0ea5e9";
      ctx.lineWidth = 2.4;
      ctx.lineJoin = "round";
      ctx.lineCap = "round";
      ctx.beginPath();
      ctx.moveTo(scaled[0]!.x, scaled[0]!.y);
      for (let i = 1; i < scaled.length; i++) {
        ctx.lineTo(scaled[i]!.x, scaled[i]!.y);
      }
      ctx.stroke();
    }
    for (let i = 0; i < scaled.length; i++) {
      const p = scaled[i]!;
      const r = i === 0 || i === scaled.length - 1 ? 4 : 3;
      ctx.beginPath();
      ctx.fillStyle = "#38bdf8";
      ctx.strokeStyle = "#0ea5e9";
      ctx.lineWidth = 1;
      ctx.arc(p.x, p.y, r, 0, Math.PI * 2);
      ctx.fill();
      ctx.stroke();
    }
    return canvas;
  }

  const palette = ["#8b5cf6", "#22c55e", "#f97316", "#06b6d4", "#f59e0b", "#ec4899"];
  visual.series.forEach((s, idx) => {
    const scaled = scalePoints(s.points, width, height);
    if (scaled.length < 2) return;
    ctx.strokeStyle = palette[idx % palette.length]!;
    ctx.lineWidth = 2.2;
    ctx.lineJoin = "round";
    ctx.lineCap = "round";
    ctx.beginPath();
    ctx.moveTo(scaled[0]!.x, scaled[0]!.y);
    for (let i = 1; i < scaled.length; i++) {
      ctx.lineTo(scaled[i]!.x, scaled[i]!.y);
    }
    ctx.stroke();
  });
  return canvas;
}

type MapVisual = Extract<EventAdvancedView, { kind: "map" }>;

type MapPluginEventViewProps = {
  visual: MapVisual;
  t: (key: string) => string;
  width: number;
  height: number;
  toolbar: "inline" | "hidden";
  /** Adds full-screen panel spacing class (modal body). */
  layout?: "default" | "fullscreen";
  /** Chat: OSM iframe first and taller; SVG schematic in a collapsible block. */
  variant?: "default" | "chat";
  /** When false, hide title (modal already has a heading). */
  showPanelHeading?: boolean;
  onFullscreen?: () => void;
  onExportCsv?: () => void;
  endPointR?: number;
  midPointR?: number;
  interactive?: {
    dragging: boolean;
    transform: string;
    onWheel: (e: ReactWheelEvent<HTMLDivElement>) => void;
    onDoubleClick: () => void;
    onPointerDown: (e: ReactPointerEvent<HTMLDivElement>) => void;
    onPointerMove: (e: ReactPointerEvent<HTMLDivElement>) => void;
    onPointerUp: (e: ReactPointerEvent<HTMLDivElement>) => void;
    onPointerCancel: (e: ReactPointerEvent<HTMLDivElement>) => void;
    onPointerLeave: (e: ReactPointerEvent<HTMLDivElement>) => void;
  };
};

function MapPluginEventView({
  visual,
  t,
  width,
  height,
  toolbar,
  layout = "default",
  variant = "default",
  showPanelHeading = true,
  onFullscreen,
  onExportCsv,
  endPointR = 3.5,
  midPointR = 2.5,
  interactive,
}: MapPluginEventViewProps) {
  const routes = useMemo(() => {
    if (visual.routes && visual.routes.length > 0) {
      return visual.routes;
    }
    return [
      {
        id: "default",
        label: t("tasks.map_route_primary"),
        points: visual.points,
        distanceM: visual.distanceM,
        durationS: visual.durationS,
        steps: [],
      },
    ];
  }, [visual, t]);

  const [routeIx, setRouteIx] = useState(0);
  useEffect(() => {
    setRouteIx(0);
  }, [visual]);

  const safeIx = Math.min(routeIx, Math.max(0, routes.length - 1));
  const active = routes[safeIx] ?? routes[0]!;
  const points = active.points.length >= 2 ? active.points : visual.points;
  const hasLeafletMap = points.length >= 2;
  const scaled = scalePoints(points, width, height);
  const polyline = scaled.map((p) => `${p.x},${p.y}`).join(" ");
  const distance = formatDistanceLabel(active.distanceM ?? visual.distanceM);
  const duration = formatDurationLabel(active.durationS ?? visual.durationS);

  const synthesizedOsm = !visual.osmEmbedUrl && hasLeafletMap ? osmUrlsFromLonLatPoints(points) : null;
  const effectiveOsmEmbed = visual.osmEmbedUrl ?? synthesizedOsm?.embed;
  const effectiveOsmBrowse = visual.osmBrowseUrl ?? synthesizedOsm?.browse;
  const linkBrowseUrl =
    typeof effectiveOsmBrowse === "string" && effectiveOsmBrowse.startsWith("http")
      ? effectiveOsmBrowse
      : hasLeafletMap
        ? osmUrlsFromLonLatPoints(points)?.browse
        : undefined;

  const lineAndPoints = (
    <>
      {scaled.length >= 2 && <polyline points={polyline} className="event-advanced-map-line" />}
      {scaled.map((p, idx) => (
        <circle
          key={`mapv-pt-${idx}`}
          cx={p.x}
          cy={p.y}
          r={idx === 0 || idx === scaled.length - 1 ? endPointR : midPointR}
          className="event-advanced-map-point"
        />
      ))}
    </>
  );

  return (
    <div
      className={`event-advanced-view event-advanced-view-map${interactive ? " event-advanced-view-map--fullscreen" : ""}${
        layout === "fullscreen" ? " event-advanced-view-fullscreen" : ""
      }`}
    >
      {showPanelHeading && <strong className="metadata-label">{visual.title ?? t("tasks.plugin_map_title")}</strong>}
      {visual.summary && <p className="event-map-summary">{visual.summary}</p>}
      {visual.geometryKind === "great_circle_estimate" ? (
        <p className="event-map-geometry-note">{t("tasks.map_geometry_great_circle_note")}</p>
      ) : null}
      {visual.geometryKind === "road_network" ? (
        <p className="event-map-geometry-note">{t("tasks.map_geometry_road_network_note")}</p>
      ) : null}
      {toolbar === "inline" && (onFullscreen || onExportCsv) && (
        <div className="event-advanced-toolbar">
          {onFullscreen && (
            <button type="button" className="event-advanced-action-btn" onClick={onFullscreen}>
              {t("tasks.open_fullscreen")}
            </button>
          )}
          {onExportCsv && (
            <button type="button" className="event-advanced-action-btn" onClick={onExportCsv}>
              {t("tasks.export_csv")}
            </button>
          )}
        </div>
      )}
      {routes.length > 1 && (
        <div className="event-map-route-tabs" role="tablist" aria-label={t("tasks.map_route_options")}>
          {routes.map((r, i) => (
            <button
              key={r.id}
              type="button"
              role="tab"
              aria-selected={i === safeIx}
              className={i === safeIx ? "event-map-route-tab is-active" : "event-map-route-tab"}
              onClick={() => setRouteIx(i)}
            >
              {r.label || `${t("tasks.map_route_primary")} ${i + 1}`}
            </button>
          ))}
        </div>
      )}
      {(distance || duration) && (
        <div className="event-advanced-metrics-inline" role="list">
          {distance && (
            <span className="event-advanced-chip" role="listitem">
              {t("tasks.distance_label")}: {distance}
            </span>
          )}
          {duration && (
            <span className="event-advanced-chip" role="listitem">
              {t("tasks.duration_label")}: {duration}
            </span>
          )}
          {active.mode && (
            <span className="event-advanced-chip" role="listitem">
              {t("tasks.map_mode_label")}: {active.mode}
            </span>
          )}
        </div>
      )}
      {hasLeafletMap ? (
        <div className="event-map-geo-leaflet">
          <div className="event-map-geo-leaflet-head">
            <span className="metadata-label">{t("tasks.map_interactive_layer")}</span>
          </div>
          <GeoMapView
            points={points}
            height={variant === "chat" ? 260 : layout === "fullscreen" ? 360 : 300}
            ariaLabel={t("tasks.map_interactive_layer")}
          />
        </div>
      ) : null}
      {(() => {
        const schematicBlock = !hasLeafletMap
          ? interactive
            ? (
                <div
                  className={`event-visual-interactive-surface ${interactive.dragging ? "is-dragging" : ""}`}
                  onWheel={interactive.onWheel}
                  onDoubleClick={interactive.onDoubleClick}
                  onPointerDown={interactive.onPointerDown}
                  onPointerMove={interactive.onPointerMove}
                  onPointerUp={interactive.onPointerUp}
                  onPointerCancel={interactive.onPointerCancel}
                  onPointerLeave={interactive.onPointerLeave}
                >
                  <svg className="event-advanced-chart" viewBox={`0 0 ${width} ${height}`} preserveAspectRatio="none" aria-label={t("tasks.plugin_map_title")}>
                    <rect x="0" y="0" width={width} height={height} rx="10" ry="10" className="event-advanced-chart-bg" />
                    <g transform={interactive.transform}>{lineAndPoints}</g>
                  </svg>
                </div>
              )
            : (
                <svg className="event-advanced-chart" viewBox={`0 0 ${width} ${height}`} preserveAspectRatio="none" aria-label={t("tasks.plugin_map_title")}>
                  <rect x="0" y="0" width={width} height={height} rx="10" ry="10" className="event-advanced-chart-bg" />
                  {lineAndPoints}
                </svg>
              )
          : null;

        const osmIframeBlock =
          effectiveOsmEmbed && !hasLeafletMap ? (
            <div className="event-map-osm-block">
              <div className="event-map-osm-head">
                <span className="metadata-label">{t("tasks.map_openstreetmap")}</span>
                {effectiveOsmBrowse && String(effectiveOsmBrowse).startsWith("http") ? (
                  <a className="event-map-osm-link" href={effectiveOsmBrowse} target="_blank" rel="noreferrer">
                    {t("tasks.map_open_in_browser")}
                  </a>
                ) : null}
              </div>
              <p className="event-map-osm-hint">{variant === "chat" ? t("tasks.map_osm_hint_chat") : t("tasks.map_osm_hint")}</p>
              <iframe title={t("tasks.map_openstreetmap")} className="event-map-osm-iframe" src={effectiveOsmEmbed} loading="lazy" referrerPolicy="no-referrer-when-downgrade" />
            </div>
          ) : null;

        const osmLinkOnlyBlock =
          hasLeafletMap && linkBrowseUrl ? (
            <div className="event-map-osm-block event-map-osm-link-only">
              <div className="event-map-osm-head">
                <span className="metadata-label">{t("tasks.map_openstreetmap")}</span>
                <a className="event-map-osm-link" href={linkBrowseUrl} target="_blank" rel="noreferrer">
                  {t("tasks.map_open_in_browser")}
                </a>
              </div>
              <p className="event-map-osm-hint">{t("tasks.map_osm_with_leaflet_hint")}</p>
            </div>
          ) : null;

        if (variant === "chat" && (Boolean(effectiveOsmEmbed) || hasLeafletMap)) {
          return (
            <>
              {hasLeafletMap ? osmLinkOnlyBlock : osmIframeBlock}
              {!hasLeafletMap && scaled.length >= 2 ? (
                <details className="event-map-schematic-details">
                  <summary className="event-map-schematic-details-summary">{t("tasks.map_schematic_details_summary")}</summary>
                  {schematicBlock}
                </details>
              ) : null}
            </>
          );
        }

        return (
          <>
            {schematicBlock}
            {osmIframeBlock}
            {osmLinkOnlyBlock}
          </>
        );
      })()}
      {active.steps && active.steps.length > 0 && (
        <div className="event-map-steps-wrap">
          <strong className="metadata-label">{t("tasks.map_itinerary_steps")}</strong>
          <ol className="event-map-steps">
            {active.steps.map((s, si) => (
              <li key={`st-${si}`}>
                {s.instruction}
                {s.distance_m != null && Number.isFinite(s.distance_m) ? (
                  <span className="event-map-step-dist"> · {formatDistanceLabel(s.distance_m)}</span>
                ) : null}
              </li>
            ))}
          </ol>
        </div>
      )}
      {visual.mapAttribution && <p className="event-map-attribution">{visual.mapAttribution}</p>}
    </div>
  );
}

function formatRelativeTimeLabel(value?: string, locale: "fr" | "en" = "fr"): string | null {
  if (!value) return null;
  const d = new Date(value);
  if (Number.isNaN(d.getTime())) return value;
  const deltaMs = Date.now() - d.getTime();
  const future = deltaMs < 0;
  const deltaSec = Math.round(Math.abs(deltaMs) / 1000);
  if (deltaSec < 60) return locale === "fr" ? (future ? "dans quelques sec." : "à l'instant") : (future ? "in a few sec" : "just now");
  const deltaMin = Math.round(deltaSec / 60);
  if (deltaMin < 60) return locale === "fr" ? (future ? `dans ${deltaMin} min` : `il y a ${deltaMin} min`) : (future ? `in ${deltaMin} min` : `${deltaMin} min ago`);
  const deltaHours = Math.round(deltaMin / 60);
  if (deltaHours < 24) return locale === "fr" ? (future ? `dans ${deltaHours} h` : `il y a ${deltaHours} h`) : (future ? `in ${deltaHours} h` : `${deltaHours} h ago`);
  const deltaDays = Math.round(deltaHours / 24);
  return locale === "fr" ? (future ? `dans ${deltaDays} j` : `il y a ${deltaDays} j`) : (future ? `in ${deltaDays} d` : `${deltaDays} d ago`);
}

function classifyAgentKind(agent?: string | null): string {
  const value = (agent ?? "").trim().toLowerCase();
  if (!value) return "generic";
  if (/(orchestr|planner|plan|router|main_agent|coordinator|supervisor)/.test(value)) return "orchestrator";
  if (/(code|coding|dev|developer|refactor|review|test|debug|fix)/.test(value)) return "code";
  if (/(conversation|chat|dialog|assistant|support)/.test(value)) return "conversation";
  if (/(memory|rag|search|retriev|knowledge|research|docs?)/.test(value)) return "knowledge";
  if (/(tool|exec|shell|terminal|operator|command)/.test(value)) return "tooling";
  if (/(vision|image|ocr|screen)/.test(value)) return "vision";
  if (/(voice|audio|speech|stt|tts)/.test(value)) return "voice";
  if (/(schedule|calendar|time|cron)/.test(value)) return "scheduler";
  return "generic";
}

function classifyEventKind(eventType?: string | null): string {
  const value = (eventType ?? "").trim().toLowerCase();
  if (!value) return "neutral";
  if (/(received|created|queued|accepted|started)$/.test(value) || value === "task_received") return "received";
  if (value === "progress_update" || value === "todo_list_updated") return "progress";
  if (value === "deterministic_preferred_tool_attempt" || value === "deterministic_preferred_tool_result") return "tool";
  if (value === "deterministic_preferred_tool_no_success") return "failure";
  if (value === "tool_call_started" || value === "tool_call_finished" || value === "tool_invoked") return "tool";
  if (value === "sub_agent_spawned" || value === "task_decomposed" || value === "plan_proposed" || value === "plan_committed" || value === "timeline_milestone" || value === "subagent_startup_started") return "orchestration";
  if (value === "task_completed") return "success";
  if (value === "task_failed") return "failure";
  if (/(ask_user|human_input|approval|confirmation)/.test(value)) return "question";
  if (value === "subagent_startup_pending") return "progress";
  return "neutral";
}

function isDeterministicAutoToolEvent(eventType?: string | null): boolean {
  const value = (eventType ?? "").trim().toLowerCase();
  return (
    value === "deterministic_preferred_tool_attempt" ||
    value === "deterministic_preferred_tool_result" ||
    value === "deterministic_preferred_tool_no_success"
  );
}

type UiMode = "simple" | "expert";
type UiDensity = "compact" | "comfortable" | "spacious";
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

type PluginStatusEntry = {
  id?: string;
  name?: string;
  description?: string;
  version?: string;
  kind?: string;
  enabled?: boolean;
  score?: number;
  disabled_reason?: string | null;
};

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
  const { tab, setTab } = useHashRoute("chat");
  const [sidebarNavCollapsed, setSidebarNavCollapsed] = useState(false);
  const [mobileSidebarOpen, setMobileSidebarOpen] = useState(false);
  const [uiDensity, setUiDensity] = useState<UiDensity>(() => {
    try {
      const s = localStorage.getItem(DENSITY_STORAGE_KEY);
      if (s === "compact" || s === "comfortable" || s === "spacious") return s;
    } catch {
      /* ignore */
    }
    return "comfortable";
  });
  const [chatAgentMode, setChatAgentMode] = useState(() => localStorage.getItem(CHAT_AGENT_MODE_KEY) !== "0");
  const [chatWebSearch, setChatWebSearch] = useState(() => localStorage.getItem(CHAT_WEB_SEARCH_KEY) === "1");
  const [chatIncognito, setChatIncognito] = useState(() => localStorage.getItem(CHAT_INCOGNITO_KEY) === "1");
  const [rightSidebarOpen, setRightSidebarOpen] = useState(() => {
    try {
      const raw = localStorage.getItem(TASK_SIDEBAR_STORAGE_KEY);
      if (raw === "0") return false;
      if (raw === "1") return true;
    } catch {
      /* ignore */
    }
    return true;
  });
  const [createTaskDialogOpen, setCreateTaskDialogOpen] = useState(false);
  const [eventTriggers, setEventTriggers] = useState<
    Array<{ id: string; name: string; enabled: boolean; trigger_type: string; last_fired_at?: string | null }>
  >([]);
  const [theme, setTheme] = useState<ThemeId>(loadSavedTheme);
  const [uiMode, setUiMode] = useState<UiMode>(loadSavedUiMode);
  const [showOnboarding, setShowOnboarding] = useState(() => readSetupWizardPending());
  const eventLabel = useCallback(
    (typ: string) => {
      const key = "events." + typ;
      const s = t(key);
      return s === key ? typ : s;
    },
    [t]
  );
  /** Libellé court pour les pastilles (évite « timeline_milestone » brut dans l'UI). */
  const eventTypeBadgeLabel = useCallback(
    (typ: string) => {
      if (typ === "timeline_milestone") {
        const short = t("events.timeline_milestone_badge");
        return short === "events.timeline_milestone_badge" ? typ : short;
      }
      return eventLabel(typ);
    },
    [t, eventLabel]
  );
  const summarizeTaskEvent = useCallback(
    (event: { event_type: string; payload?: unknown }) => {
      const payload = event.payload;
      if (event.event_type === "progress_update" && payload && typeof payload === "object" && "message" in payload && typeof (payload as { message?: unknown }).message === "string") {
        return trimPreview(String((payload as { message: string }).message), 160);
      }
      if ((event.event_type === "tool_call_started" || event.event_type === "tool_call_finished") && payload && typeof payload === "object" && "tool" in payload && (payload as { tool?: string }).tool) {
        return `${t("tasks.tool_summary_prefix")}: ${String((payload as { tool: string }).tool)}`;
      }
      if (event.event_type === "tool_invoked" && payload && typeof payload === "object") {
        const p = payload as Record<string, unknown>;
        const tool = typeof p.tool === "string" ? p.tool : "?";
        const success = typeof p.success === "boolean" ? p.success : null;
        const suffix = success !== null ? ` · ${success ? t("common.yes") : t("common.no")}` : "";
        return `${t("tasks.tool_summary_prefix")}: ${tool}${suffix}`;
      }
      if (event.event_type === "sub_agent_spawned" && payload && typeof payload === "object" && "agent" in payload && (payload as { agent?: string }).agent) {
        return `${t("tasks.agent_summary_prefix")}: ${String((payload as { agent: string }).agent)}`;
      }
      if ((event.event_type === "plan_proposed" || event.event_type === "plan_committed") && payload && typeof payload === "object" && Array.isArray((payload as { steps?: unknown[] }).steps)) {
        return t("tasks.plan_summary").replace("{{count}}", String((payload as { steps: unknown[] }).steps.length));
      }
      if ((event.event_type === "task_completed" || event.event_type === "task_failed") && payload && typeof payload === "object" && "model_used" in payload && (payload as { model_used?: string | null }).model_used) {
        return `${t("tasks.model_used")}: ${String((payload as { model_used: string }).model_used)}`;
      }
      if (event.event_type === "llm_route_planned" && payload && typeof payload === "object") {
        const p = payload as Record<string, unknown>;
        const provider = typeof p.provider === "string" ? p.provider : "?";
        const model = typeof p.model === "string" ? p.model : "?";
        return `${t("tasks.route_planned_summary")}: ${provider}/${model}`;
      }
      if (event.event_type === "llm_call_started" && payload && typeof payload === "object") {
        const p = payload as Record<string, unknown>;
        const provider = typeof p.provider === "string" ? p.provider : "?";
        const model = typeof p.model === "string" ? p.model : "?";
        const round = typeof p.round === "number" ? p.round : undefined;
        const base = `${t("tasks.llm_call_summary")}: ${provider}/${model}`;
        return round != null ? `${base} · #${round}` : base;
      }
      if (event.event_type === "llm_call_finished" && payload && typeof payload === "object") {
        const p = payload as Record<string, unknown>;
        const success = p.success === true;
        const model = typeof p.model_used === "string" ? p.model_used : undefined;
        const err = typeof p.error === "string" ? p.error : undefined;
        const latency = typeof p.latency_ms === "number" ? p.latency_ms : undefined;
        if (success && model) {
          return latency != null
            ? `${t("tasks.model_used")}: ${model} · ${latency} ms`
            : `${t("tasks.model_used")}: ${model}`;
        }
        return err ? `${t("tasks.llm_call_summary")}: ${trimPreview(err, 120)}` : t("tasks.llm_call_summary");
      }
      if (event.event_type === "memory_recall_started" && payload && typeof payload === "object") {
        const p = payload as Record<string, unknown>;
        const topK = typeof p.semantic_top_k === "number" ? p.semantic_top_k : undefined;
        return topK != null ? `${t("events.memory_recall_started")} (top_k=${topK})` : t("events.memory_recall_started");
      }
      if (event.event_type === "memory_recall_finished" && payload && typeof payload === "object") {
        const p = payload as Record<string, unknown>;
        const had = p.had_results === true;
        const timedOut = p.timed_out === true;
        if (timedOut) return `${t("events.memory_recall_finished")} · timeout`;
        return had
          ? `${t("events.memory_recall_finished")} · ${t("common.yes")}`
          : `${t("events.memory_recall_finished")} · ${t("common.no")}`;
      }
      if (event.event_type === "pipeline_checkpoint" && payload && typeof payload === "object") {
        const p = payload as Record<string, unknown>;
        const msg = typeof p.message === "string" ? p.message : "";
        const pct = typeof p.progress_pct === "number" ? p.progress_pct : undefined;
        if (msg) return trimPreview(msg, 160);
        if (pct != null) return t("tasks.progress_pct_summary").replace("{{pct}}", String(pct));
      }
      if (event.event_type === "deterministic_preferred_tool_attempt" && payload && typeof payload === "object") {
        const p = payload as Record<string, unknown>;
        const tool = typeof p.tool === "string" ? p.tool : "?";
        const round = typeof p.round === "number" ? p.round : undefined;
        return round != null
          ? `${t("tasks.deterministic_tool_attempt_summary")} ${tool} · ${t("tasks.attempt_label")} #${round}`
          : `${t("tasks.deterministic_tool_attempt_summary")} ${tool}`;
      }
      if (event.event_type === "deterministic_preferred_tool_result" && payload && typeof payload === "object") {
        const p = payload as Record<string, unknown>;
        const tool = typeof p.tool === "string" ? p.tool : "?";
        const success = typeof p.success === "boolean" ? p.success : false;
        return `${t("tasks.deterministic_tool_result_summary")} ${tool} · ${success ? t("common.yes") : t("common.no")}`;
      }
      if (event.event_type === "deterministic_preferred_tool_no_success" && payload && typeof payload === "object") {
        const p = payload as Record<string, unknown>;
        const tools = Array.isArray(p.attempted_tools)
          ? (p.attempted_tools as unknown[]).map((x) => String(x)).filter(Boolean)
          : [];
        return tools.length > 0
          ? `${t("tasks.deterministic_tool_no_success_summary")} ${tools.join(", ")}`
          : t("tasks.deterministic_tool_no_success_summary");
      }
      if (event.event_type === "timeline_milestone" && payload && typeof payload === "object") {
        const p = payload as Record<string, unknown>;
        const name = typeof p.name === "string" ? p.name.trim() : "";
        if (name) {
          const key = `events.milestone.${name}`;
          let line = t(key);
          if (line === key) {
            line = name.replace(/_/g, " ");
          }
          if (typeof p.tool === "string" && p.tool.trim()) {
            line = `${line} — ${p.tool.trim()}`;
          }
          if (typeof p.step_id === "string" && p.step_id.trim()) {
            line = `${line} · ${p.step_id.trim()}`;
          }
          if (typeof p.child_task_id === "string" && p.child_task_id.length >= 8) {
            line = `${line} · #${p.child_task_id.slice(-8)}`;
          }
          if (typeof p.round === "number") {
            line = `${line} · ${t("tasks.attempt_label")} #${p.round}`;
          }
          return trimPreview(line, 220);
        }
      }
      return "";
    },
    [t]
  );
  const extractTaskDiscussionHighlights = useCallback(
    (event: { event_type: string; payload?: unknown }) => {
      const payload = event.payload;
      if (!payload || typeof payload !== "object") return [] as string[];
      const p = payload as Record<string, unknown>;
      const out: string[] = [];
      const pushLabeled = (label: string, value: unknown) => {
        if (typeof value === "string" && value.trim()) {
          out.push(`${label}: ${trimPreview(value.trim(), 180)}`);
        }
      };

      if (event.event_type === "task_decomposed") {
        pushLabeled(t("tasks.task_type_label"), p.decompose_model_task_type);
        pushLabeled(t("tasks.reason_label"), p.decompose_reason);
        pushLabeled(t("tasks.attempt_label"), p.decompose_attempt);
      }
      if (event.event_type === "sub_agent_spawned") {
        pushLabeled(t("tasks.reason_label"), p.delegation_reason);
      }

      pushLabeled(t("tasks.reason_label"), p.reason);
      pushLabeled(t("tasks.reason_label"), p.selector_reason);

      if (event.event_type === "progress_update" && typeof p.message === "string" && p.message.trim()) {
        const msg = p.message.trim();
        if (/(analy|analyse|orchestr|routing|routage|decompos|décompos|plan|thinking|réflex|reflex|recovery|best-effort|startup|démarrage)/i.test(msg)) {
          out.push(`${t("tasks.phase_label")}: ${trimPreview(msg, 180)}`);
        }
      }
      if (event.event_type === "timeline_milestone") {
        const mn = typeof p.name === "string" ? p.name.trim() : "";
        if (mn) {
          const key = `events.milestone.${mn}`;
          let line = t(key);
          if (line === key) {
            line = mn.replace(/_/g, " ");
          }
          out.push(line);
        }
        pushLabeled(t("tasks.phase_label"), p.milestone);
      }

      if (event.event_type === "deterministic_preferred_tool_attempt") {
        pushLabeled(t("tasks.tool_summary_prefix"), p.tool);
        pushLabeled(t("tasks.attempt_label"), p.round);
      }

      if (event.event_type === "deterministic_preferred_tool_result") {
        pushLabeled(t("tasks.tool_summary_prefix"), p.tool);
        if (typeof p.success === "boolean") {
          out.push(`${t("tasks.result_label")}: ${p.success ? t("common.yes") : t("common.no")}`);
        }
        pushLabeled(t("tasks.reason_label"), p.reason);
      }

      if (event.event_type === "deterministic_preferred_tool_no_success") {
        if (Array.isArray(p.attempted_tools) && p.attempted_tools.length > 0) {
          out.push(`${t("tasks.tool_summary_prefix")}: ${(p.attempted_tools as unknown[]).map((x) => String(x)).join(", ")}`);
        }
      }

      if (event.event_type === "tool_invoked") {
        pushLabeled(t("tasks.tool_summary_prefix"), p.tool);
        if (typeof p.success === "boolean") {
          out.push(`${t("tasks.result_label")}: ${p.success ? t("common.yes") : t("common.no")}`);
        }
        if (typeof p.result_preview === "string" && p.result_preview.trim()) {
          out.push(`${t("tasks.result_preview_label")}: ${trimPreview(p.result_preview.trim(), 200)}`);
        }
      }

      return Array.from(new Set(out));
    },
    [t]
  );
  const extractModelMetadata = useCallback(
    (event: { event_type: string; payload?: unknown }) => {
      const payload = event.payload;
      if (!payload || typeof payload !== "object") return null;
      const p = payload as Record<string, unknown>;

      const thinking = typeof p.thinking === "string" ? p.thinking.trim() : null;
      const response = typeof p.response === "string" ? p.response.trim() : null;
      const model = typeof p.model === "string" ? p.model.trim() : null;
      const evalCount = typeof p.eval_count === "number" ? p.eval_count : null;
      const promptEvalCount = typeof p.prompt_eval_count === "number" ? p.prompt_eval_count : null;
      const evalDuration = typeof p.eval_duration === "number" ? p.eval_duration : null;
      const promptEvalDuration = typeof p.prompt_eval_duration === "number" ? p.prompt_eval_duration : null;
      const loadDuration = typeof p.load_duration === "number" ? p.load_duration : null;
      const totalDuration = typeof p.total_duration === "number" ? p.total_duration : null;
      const doneReason = typeof p.done_reason === "string" ? p.done_reason.trim() : null;

      if (!thinking && !response && !model && !evalCount) return null;

      return {
        thinking,
        response,
        model,
        evalCount,
        promptEvalCount,
        evalDuration,
        promptEvalDuration,
        loadDuration,
        totalDuration,
        doneReason,
      };
    },
    []
  );
  const [health, setHealth] = useState<HealthState | null>(null);
  const [message, setMessage] = useState("");
  const [chatResearchContext, setChatResearchContext] = useState<ChatResearchContext | null>(null);
  const [chatNoteContext, setChatNoteContext] = useState<ChatNoteContext | null>(null);
  const [notePickerOpen, setNotePickerOpen] = useState(false);
  const [notePickerItems, setNotePickerItems] = useState<Array<{ id: string; title: string }>>([]);
  const [notePickerLoading, setNotePickerLoading] = useState(false);
  const [chatDeliveryMode, setChatDeliveryMode] = useState<"immediate" | "steering" | "follow_up">("immediate");
  const [memoryHygieneHint, setMemoryHygieneHint] = useState<string | null>(null);
  type MemoryAdvancedSettings = {
    multi_query?: boolean;
    hyde?: boolean;
    rrf?: boolean;
    rollup_days?: number;
    semantic_top_k?: number;
    graph_expand_hops?: number;
    user_rag_top_k?: number;
    workspace_graph_top_k?: number;
  };
  const [memoryAdvancedSettings, setMemoryAdvancedSettings] = useState<MemoryAdvancedSettings | null>(null);
  const [memoryAdvancedSaving, setMemoryAdvancedSaving] = useState(false);
  const [memoryAdvancedMessage, setMemoryAdvancedMessage] = useState<string | null>(null);
  const [messages, setMessages] = useState<ChatMessageRow[]>([]);
  const [modelPricingLookup, setModelPricingLookup] = useState<Map<string, ModelPriceRates>>(new Map());

  const enrichUsageWithPricing = useCallback(
    (usage: ModelUsageStats | null | undefined): ModelUsageStats | undefined => {
      if (!usage) return undefined;
      const rates = lookupPriceRates(modelPricingLookup, usage.model);
      return rates ? { ...usage, priceRates: rates } : usage;
    },
    [modelPricingLookup],
  );

  const discussResearchReport = useCallback(
    (doc: ResearchReportDocument) => {
      if (!doc.reportMarkdown.trim()) return;
      setChatNoteContext(null);
      setChatResearchContext({
        topic: doc.topic,
        reportMarkdown: doc.reportMarkdown,
        category: doc.reportMeta?.category,
      });
      setTab("chat");
    },
    [setTab],
  );

  const discussNote = useCallback(
    (ctx: ChatNoteContext) => {
      setChatResearchContext(null);
      setChatNoteContext(ctx);
      setTab("chat");
    },
    [setTab],
  );

  const openNotePicker = useCallback(async () => {
    setNotePickerLoading(true);
    setNotePickerOpen(true);
    try {
      const notes = await listNotes(DAEMON_PORT);
      setNotePickerItems(notes.map((n) => ({ id: n.id, title: n.title })));
    } catch {
      setNotePickerItems([]);
    } finally {
      setNotePickerLoading(false);
    }
  }, []);

  const attachNoteToChat = useCallback(async (noteId: string) => {
    try {
      const doc = await getNote(noteId, DAEMON_PORT);
      setChatResearchContext(null);
      setChatNoteContext({
        noteId: doc.id,
        title: doc.title,
        markdown: doc.content,
        intent: "discuss",
      });
      setNotePickerOpen(false);
    } catch {
      /* ignore */
    }
  }, []);
  const exportChatTranscript = useCallback(() => {
    const body = exportChatPlainText(messages);
    const base = defaultExportBasename(messages);
    const blob = new Blob([body], { type: "text/plain;charset=utf-8" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `${base}.txt`;
    document.body.appendChild(a);
    a.click();
    a.remove();
    URL.revokeObjectURL(url);
  }, [messages]);
  /** Map visuals keyed by chat task_id — fallback when message.mapVisual was overwritten during streaming. */
  const [chatMapByTaskId, setChatMapByTaskId] = useState<Record<string, ChatMapVisual>>({});
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
  const [tasksEvents, setTasksEvents] = useState<Array<{ event_type: string; payload?: unknown; at: string; task_id?: string }>>([]);
  const [tasksLoading, setTasksLoading] = useState(false);
  const isSimpleMode = uiMode === "simple";
  const [collapsedTaskBranches, setCollapsedTaskBranches] = useState<Record<string, boolean>>(() => {
    try {
      const raw = localStorage.getItem(TASK_TREE_COLLAPSE_STORAGE_KEY);
      if (!raw) return {};
      const parsed = JSON.parse(raw) as Record<string, boolean>;
      return parsed && typeof parsed === "object" ? parsed : {};
    } catch {
      return {};
    }
  });
  /** Tâches : sections pliables (étapes / événements), mémorisées localement. */
  const [taskPanelSections, setTaskPanelSections] = useState(() => {
    try {
      const raw = localStorage.getItem("akasha_task_panel_sections");
      if (raw) {
        const j = JSON.parse(raw) as { list?: boolean; steps?: boolean; events?: boolean };
        return {
          steps: j.steps !== false,
          events: j.events !== false,
        };
      }
    } catch {
      /* ignore */
    }
    return { steps: true, events: true };
  });
  const [taskOrchestrationDebugLevel, setTaskOrchestrationDebugLevel] = useState<TaskOrchestrationDebugLevel>(() => {
    try {
      const raw = localStorage.getItem(TASK_ORCHESTRATION_DEBUG_STORAGE_KEY);
      if (raw === "minimal" || raw === "normal" || raw === "full") return raw;
      // Backward compatibility with previous boolean persistence.
      if (raw === "1") return "normal";
      if (raw === "0") return "minimal";
    } catch {
      /* ignore */
    }
    return loadSavedUiMode() === "expert" ? "normal" : "minimal";
  });
  const setTaskSidebarOpen = useCallback((open: boolean) => {
    setRightSidebarOpen(open);
    try {
      localStorage.setItem(TASK_SIDEBAR_STORAGE_KEY, open ? "1" : "0");
    } catch {
      /* ignore */
    }
  }, []);

  const toggleTaskPanelSection = useCallback((key: "steps" | "events") => {
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
  const setTaskOrchestrationDebugLevelAndSave = useCallback((next: TaskOrchestrationDebugLevel) => {
    setTaskOrchestrationDebugLevel(next);
    try {
      localStorage.setItem(TASK_ORCHESTRATION_DEBUG_STORAGE_KEY, next);
    } catch {
      /* ignore */
    }
  }, []);
  const [eventVisualFullscreen, setEventVisualFullscreen] = useState<{
    visual: EventAdvancedView;
    sourceEventType: string;
  } | null>(null);
  const [eventVisualHelpOpen, setEventVisualHelpOpen] = useState(false);
  const [eventVisualTransform, setEventVisualTransform] = useState<{ scale: number; tx: number; ty: number }>({
    scale: 1,
    tx: 0,
    ty: 0,
  });
  const [eventVisualDragging, setEventVisualDragging] = useState<{
    startX: number;
    startY: number;
    baseTx: number;
    baseTy: number;
  } | null>(null);
  const eventVisualPointersRef = useRef<Map<number, { x: number; y: number }>>(new Map());
  const eventVisualPinchBaseDistanceRef = useRef<number | null>(null);
  const eventVisualPinchBaseTransformRef = useRef<{ scale: number; tx: number; ty: number } | null>(null);
  const persistCollapsedTaskBranches = useCallback((next: Record<string, boolean>) => {
    try {
      localStorage.setItem(TASK_TREE_COLLAPSE_STORAGE_KEY, JSON.stringify(next));
    } catch {
      /* ignore */
    }
  }, []);
  const toggleTaskBranch = useCallback((taskId: string) => {
    setCollapsedTaskBranches((prev) => {
      const next = { ...prev, [taskId]: !prev[taskId] };
      persistCollapsedTaskBranches(next);
      return next;
    });
  }, [persistCollapsedTaskBranches]);
  const [taskStepsTodos, setTaskStepsTodos] = useState<Array<{ id?: string | null; title: string; status: string }>>([]);
  const selectedTaskIdForTodosRef = useRef<string | null>(null);
  const tasksEventsTaskIdRef = useRef<string | null>(null);
  const fetchTaskStepsRef = useRef<(taskId: string) => Promise<void>>(async () => {});
  const fetchTasksEventsRef = useRef<(taskId: string) => Promise<void>>(async () => {});
  const trackTaskUntilDoneRef = useRef<(taskId: string) => void>(() => {});
  const [runningTaskChips, setRunningTaskChips] = useState<Record<string, { pct?: number; message?: string }>>({});
  const activeSteeringTaskId = useMemo(() => {
    const ids = Object.keys(runningTaskChips);
    return ids.length > 0 ? ids[ids.length - 1]! : null;
  }, [runningTaskChips]);
  /** Events (sub_agent_spawned, progress_update, etc.) per running task for collapsible sub-agent panel. Each event may have task_id (root or child). */
  const [runningTaskEvents, setRunningTaskEvents] = useState<Record<string, Array<{ event_type: string; payload?: unknown; at: string; task_id?: string }>>>({});
  const chatToolBatchSummary = useMemo(() => {
    const names: string[] = [];
    for (const taskId of Object.keys(runningTaskChips)) {
      const evs = runningTaskEvents[taskId] ?? [];
      for (const ev of evs) {
        const p = ev.payload;
        if (!p || typeof p !== "object") continue;
        if (ev.event_type === "tool_call_started" || ev.event_type === "tool_call_finished") {
          const tool = (p as { tool?: string }).tool;
          if (tool) names.push(tool);
        } else if (ev.event_type === "tool_invoked") {
          const pl = p as { tool_name?: string; tool?: string };
          const tool = pl.tool_name ?? pl.tool;
          if (tool) names.push(tool);
        }
      }
    }
    return heuristicToolBatchSummary(names);
  }, [runningTaskEvents, runningTaskChips]);
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
  /** Active session id for filtering stream/poll updates when multiple chat threads exist. */
  const sessionIdRef = useRef<string | null>(null);
  /** task_id → session_id at send time (multi-thread). */
  const taskIdToSessionIdRef = useRef<Record<string, string>>({});
  /** After /newsession, next POST uses new_session: true. */
  const pendingNewSessionAfterSlashRef = useRef(false);
  const chatMapByTaskIdRef = useRef<Record<string, ChatMapVisual>>({});
  const ackTextByTaskRef = useRef<Record<string, string>>({});
  const [humanInputFreeText, setHumanInputFreeText] = useState("");
  /** Reply text for the inline ask_user form in the chat (when modal is not used). */
  const [inlineHumanReplyText, setInlineHumanReplyText] = useState("");
  /** Fil d'activité des agents : ouvert par défaut pour suivre les étapes (assistant conversationnel). */
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
  type CalendarGridEvent = { at: string; task_id?: string; type: string; status: string; label?: string; schedule_id?: string | null; event_id?: string };
  const [calendarGridEvents, setCalendarGridEvents] = useState<CalendarGridEvent[]>([]);
  const [calendarGridDate, setCalendarGridDate] = useState(() => new Date());
  const [calendarCellDetail, setCalendarCellDetail] = useState<{ slotKey: string; slotLabel: string; events: CalendarGridEvent[] } | null>(null);
  type CalendarSubTab = "grid" | "recent" | "schedules" | "wakeups" | "external";
  const [calendarSubTab, setCalendarSubTab] = useState<CalendarSubTab>("grid");
  type WakeupRow = { id: string; session_id: string; fire_at: string; message: string; status: string; created_at?: string };
  const [wakeups, setWakeups] = useState<WakeupRow[]>([]);
  const [wakeupFormMinutes, setWakeupFormMinutes] = useState("60");
  const [wakeupFormMessage, setWakeupFormMessage] = useState("");
  const [wakeupSaving, setWakeupSaving] = useState(false);
  const calendarEventLabel = (ev: { label?: string; task_id?: string; type?: string }) => {
    if (ev.label && ev.label.trim()) return ev.label;
    if (ev.type === "external") return ev.label?.trim() || (locale === "en" ? "External event" : "Événement externe");
    const tid = ev.task_id ?? "";
    return tid ? `Tâche …${tid.slice(-8)}` : (locale === "en" ? "Event" : "Événement");
  };
  const calendarGetParentKey = (ev: CalendarGridEvent) =>
    ev.type === "external" ? `external_${ev.event_id ?? ev.at}` : ev.schedule_id ?? `task_${ev.task_id}`;
  const calendarOpenGridEvent = (e: CalendarGridEvent) => {
    if (e.type === "external" || !e.task_id) return;
    setCalendarSelectedTaskId(e.task_id);
  };
  const calendarGetEventStatusClass = (status: string, type?: string) => {
    if (type === "external") return "calendar-event--external";
    const s = (status ?? "").toLowerCase();
    if (s === "completed") return "calendar-event--completed";
    if (s === "running") return "calendar-event--running";
    if (s === "queued" || s === "skipped") return "calendar-event--upcoming";
    if (s === "failed" || s === "cancelled") return "calendar-event--failed";
    return "calendar-event--upcoming";
  };
  const calendarSlotAverageStatusClass = (events: CalendarGridEvent[]) => {
    if (events.length === 0) return "";
    let completed = 0;
    let running = 0;
    let failed = 0;
    let upcoming = 0;
    for (const ev of events) {
      const s = (ev.status ?? "").toLowerCase();
      if (s === "completed") completed++;
      else if (s === "running") running++;
      else if (s === "failed" || s === "cancelled") failed++;
      else upcoming++;
    }
    const total = events.length;
    if (running > 0) return "calendar-cell-view-all--running";
    if (failed / total >= 0.5) return "calendar-cell-view-all--failed";
    if (completed / total >= 0.5) return "calendar-cell-view-all--completed";
    if (failed > 0) return "calendar-cell-view-all--failed";
    return "calendar-cell-view-all--upcoming";
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
    const getParentKey = (ev: CalendarGridEvent) =>
      ev.type === "external" ? `external_${ev.event_id ?? ev.at}` : ev.schedule_id ?? `task_${ev.task_id}`;
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

  type MissionApi = {
    enabled: boolean;
    global_context: string;
    horizon: string;
    objective: string;
    heartbeat_interval_minutes: number;
    report_dir: string;
    session_id: string;
    status: string;
    operating_rules?: string;
    role_definitions?: Array<{
      name: string;
      responsibility: string;
      preferred_agent_type?: string | null;
    }>;
    heartbeat_preferred_task_type?: string;
    report_path_absolute?: string;
    last_heartbeat_at?: string | null;
    next_heartbeat_approx_at?: string | null;
    last_task_id?: string | null;
  };
  type MissionSubTab = "goals" | "settings" | "status";
  const MISSION_HEARTBEAT_AGENT_TYPES = [
    "conversation",
    "search",
    "code",
    "schedule",
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
  ] as const;
  function normalizeMissionApi(j: MissionApi): MissionApi {
    return {
      ...j,
      operating_rules: j.operating_rules ?? "",
      role_definitions: Array.isArray(j.role_definitions) ? j.role_definitions : [],
      heartbeat_preferred_task_type: j.heartbeat_preferred_task_type ?? "project_manager",
    };
  }
  function cloneMission(m: MissionApi): MissionApi {
    return JSON.parse(JSON.stringify(m)) as MissionApi;
  }
  function missionEditableEqual(a: MissionApi, b: MissionApi): boolean {
    const ra = JSON.stringify(a.role_definitions ?? []);
    const rb = JSON.stringify(b.role_definitions ?? []);
    return (
      a.enabled === b.enabled &&
      a.global_context === b.global_context &&
      a.horizon === b.horizon &&
      a.objective === b.objective &&
      a.heartbeat_interval_minutes === b.heartbeat_interval_minutes &&
      a.report_dir === b.report_dir &&
      a.session_id === b.session_id &&
      (a.operating_rules ?? "") === (b.operating_rules ?? "") &&
      (a.heartbeat_preferred_task_type ?? "project_manager") === (b.heartbeat_preferred_task_type ?? "project_manager") &&
      ra === rb
    );
  }
  const [mission, setMission] = useState<MissionApi | null>(null);
  const [missionDraft, setMissionDraft] = useState<MissionApi | null>(null);
  const [missionSubTab, setMissionSubTab] = useState<MissionSubTab>("goals");
  const [missionEvents, setMissionEvents] = useState<
    Array<{ id: number; at: string; event_type: string; payload?: unknown }>
  >([]);
  const [missionLoading, setMissionLoading] = useState(false);
  const [missionError, setMissionError] = useState<string | null>(null);
  const [missionSaving, setMissionSaving] = useState(false);
  const missionDirty =
    mission != null && missionDraft != null && !missionEditableEqual(missionDraft, mission);

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

  const [scheduleReports, setScheduleReports] = useState<Array<{
    schedule_name: string;
    message: string;
    ended_at?: string;
    schedule_id?: string;
    task_id?: string;
    task_run_id?: string;
    task_label?: string;
    status?: string;
    planned_for?: string;
    started_at?: string;
    run_ended_at?: string;
  }>>([]);
  const [pluginReputationTarget, setPluginReputationTarget] = useState("maps");
  const [pluginReputationResetLoading, setPluginReputationResetLoading] = useState(false);
  const [pluginReputationResetMessage, setPluginReputationResetMessage] = useState<string | null>(null);
  const [pluginReputationResetError, setPluginReputationResetError] = useState<string | null>(null);
  const [pluginStatusList, setPluginStatusList] = useState<PluginStatusEntry[]>([]);
  const [pluginStatusLoading, setPluginStatusLoading] = useState(false);
  const [pluginStatusError, setPluginStatusError] = useState<string | null>(null);
  const [sessionId, setSessionId] = useState<string | null>(() => {
    try {
      return localStorage.getItem(AKASHA_SESSION_ID_KEY);
    } catch {
      return null;
    }
  });
  const [chatThreads, setChatThreads] = useState<ChatThreadEntry[]>(() => loadChatThreadsInitial());
  const [chatThreadSearch, setChatThreadSearch] = useState("");
  const [chatThreadFolderFilter, setChatThreadFolderFilter] = useState("");
  const [handoffDialogOpen, setHandoffDialogOpen] = useState(false);
  const [handoffTargetModel, setHandoffTargetModel] = useState("");
  const [handoffTargetProvider, setHandoffTargetProvider] = useState("");
  const [handoffTaskId, setHandoffTaskId] = useState("");
  const [handoffModels, setHandoffModels] = useState<Array<{ provider: string; model: string }>>([]);
  const [handoffBusy, setHandoffBusy] = useState(false);
  const [handoffStatus, setHandoffStatus] = useState<string | null>(null);
  const [userRagDocuments, setUserRagDocuments] = useState<Array<{ id: string; name: string; mime_type: string; added_at: string; index_status?: string; indexed_at?: string | null; index_error?: string | null }>>([]);
  const [userRagLoading, setUserRagLoading] = useState(false);
  const [userRagError, setUserRagError] = useState<string | null>(null);
  const [dataSourcesSubTab, setDataSourcesSubTab] = useState<"rag" | "project_graph">("rag");
  type ProjectWorkspaceRow = {
    id: string;
    name: string;
    root_path: string;
    created_at?: string;
    node_count?: number;
    edge_count?: number;
    built_at?: string | null;
    file_count?: number;
    indexed_root?: string | null;
  };
  const [projectWorkspaces, setProjectWorkspaces] = useState<ProjectWorkspaceRow[]>([]);
  const [projectWorkspacesLoading, setProjectWorkspacesLoading] = useState(false);
  const [projectGraphError, setProjectGraphError] = useState<string | null>(null);
  const [projectGraphSuccess, setProjectGraphSuccess] = useState<string | null>(null);
  const [newProjectWsName, setNewProjectWsName] = useState("");
  const [newProjectWsPath, setNewProjectWsPath] = useState("");
  const [projectWsBusyId, setProjectWsBusyId] = useState<string | null>(null);
  const [newProjectWsSubmitting, setNewProjectWsSubmitting] = useState(false);
  const agentAvatarFileInputRef = useRef<HTMLInputElement>(null);
  const userAvatarFileInputRef = useRef<HTMLInputElement>(null);
  /** Agent profile (name, role, gender, avatar, personality, rules, can_do, cannot_do, traits_override, preferred_mode) for Settings panel. */
  const [agentProfile, setAgentProfile] = useState<{
    name: string;
    role: string;
    gender: string;
    formality: string;
    avatar: string;
    personality: string;
    rules: string[];
    can_do: string[];
    cannot_do: string[];
    traits_override: Record<string, number>;
    preferred_mode: string;
    temperature: string;
    system_prompt: string;
  }>({
    name: "",
    role: "",
    gender: "",
    formality: "",
    avatar: "",
    personality: "",
    rules: [],
    can_do: [],
    cannot_do: [],
    traits_override: {},
    preferred_mode: "",
    temperature: "",
    system_prompt: "",
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
  const [systemSubTab, setSystemSubTab] = useState<"general" | "plugins" | "policy" | "connectors" | "health">("general");
  const [pluginTableBusyId, setPluginTableBusyId] = useState<string | null>(null);
  const [skillsCatalogText, setSkillsCatalogText] = useState<string | null>(null);
  const [skillsCatalogLoading, setSkillsCatalogLoading] = useState(false);
  const [agentProfileSubTab, setAgentProfileSubTab] = useState<AgentProfileSubTab>("identity");
  /** Constitution layer (agent_identity.yaml): tone, values, constraints. Name/role shared with agentProfile. */
  const [agentConstitution, setAgentConstitution] = useState<{
    tone: string;
    values: string[];
    constraints: string[];
  }>({ tone: "", values: [], constraints: [] });
  const [rulesDraft, setRulesDraft] = useState("");
  const [canDoDraft, setCanDoDraft] = useState("");
  const [cannotDoDraft, setCannotDoDraft] = useState("");
  /** Attachments for the next message: images (vision) and documents (text appended to message). */
  const [attachments, setAttachments] = useState<Array<{ id: string; name: string; typ: "image" | "document"; content_base64: string; mime_type: string }>>([]);
  const chatEndRef = useRef<HTMLDivElement>(null);
  const subagentsDetailRef = useRef<HTMLDivElement>(null);
  const chatInlineReplyRef = useRef<HTMLDivElement>(null);
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

  const [chatTipsEnabled, setChatTipsEnabled] = useState(() => {
    try {
      return localStorage.getItem(CHAT_TIPS_STORAGE_KEY) !== "0";
    } catch {
      return true;
    }
  });
  const [chatPromptChipsEnabled, setChatPromptChipsEnabled] = useState(() => {
    try {
      return localStorage.getItem(CHAT_PROMPT_CHIPS_STORAGE_KEY) === "1";
    } catch {
      return false;
    }
  });
  const [buddyLineEnabled, setBuddyLineEnabled] = useState(() => {
    try {
      return localStorage.getItem(CHAT_BUDDY_STORAGE_KEY) === "1";
    } catch {
      return false;
    }
  });
  const [tipBannerText, setTipBannerText] = useState<string | null>(null);
  const [tipBannerDismissed, setTipBannerDismissed] = useState(false);
  const [companionOpen, setCompanionOpen] = useState(() => localStorage.getItem(CHAT_COMPANION_OPEN_KEY) === "1");

  const setChatTipsEnabledAndSave = useCallback((next: boolean) => {
    setChatTipsEnabled(next);
    try {
      localStorage.setItem(CHAT_TIPS_STORAGE_KEY, next ? "1" : "0");
    } catch {
      /* ignore */
    }
  }, []);
  const setChatPromptChipsEnabledAndSave = useCallback((next: boolean) => {
    setChatPromptChipsEnabled(next);
    try {
      localStorage.setItem(CHAT_PROMPT_CHIPS_STORAGE_KEY, next ? "1" : "0");
    } catch {
      /* ignore */
    }
  }, []);
  const setBuddyLineEnabledAndSave = useCallback((next: boolean) => {
    setBuddyLineEnabled(next);
    try {
      localStorage.setItem(CHAT_BUDDY_STORAGE_KEY, next ? "1" : "0");
    } catch {
      /* ignore */
    }
  }, []);

  const promptChipLabels = useMemo(
    () => [t("chat.prompt_chip_1"), t("chat.prompt_chip_2"), t("chat.prompt_chip_3")],
    [t, locale]
  );
  const buddyCaption = useMemo(() => {
    if (!buddyLineEnabled) return null;
    const lines = [t("chat.buddy_line_1"), t("chat.buddy_line_2"), t("chat.buddy_line_3")];
    const day = Math.floor(Date.now() / 86400000);
    return lines[day % lines.length];
  }, [buddyLineEnabled, t, locale]);
  const companionBubbleText = useMemo(() => {
    if (!buddyLineEnabled) return null;
    if (Object.keys(pendingHumanInput).length > 0) return t("chat.companion_action_required");
    if (loading) return t("chat.companion_thinking");
    if (message.trim().length > 0) return t("chat.companion_draft");
    const runningCount = Object.keys(runningTaskChips).length;
    if (runningCount > 0) return t("chat.active_tasks").replace("{{count}}", String(runningCount));
    if (chatTipsEnabled && tipBannerText && !tipBannerDismissed) return tipBannerText;
    return buddyCaption ?? t("chat.companion_idle");
  }, [
    buddyLineEnabled,
    pendingHumanInput,
    loading,
    message,
    runningTaskChips,
    chatTipsEnabled,
    tipBannerText,
    tipBannerDismissed,
    buddyCaption,
    t,
  ]);

  const checkHealth = useCallback(async () => {
    if (E2E_WEB) {
      try {
        const r = await fetch(e2eDaemonHttpUrl("/"));
        const body = (await r.json()) as { status?: string };
        const ok = r.ok && body?.status === "ok";
        setHealth((prev) => {
          const next = { ok, port: DAEMON_PORT };
          const unchanged = prev != null && prev.ok === next.ok && prev.port === next.port;
          if (unchanged) return prev;
          return next;
        });
      } catch {
        setHealth((prev) => {
          const next = { ok: false, port: DAEMON_PORT };
          const unchanged = prev != null && prev.ok === next.ok && prev.port === next.port;
          if (unchanged) return prev;
          return next;
        });
      }
      return;
    }
    try {
      const result = await invoke<{ ok: boolean; port?: number }>("check_health", {
        port: DAEMON_PORT,
      });
      setHealth((prev) => {
        const next = { ok: result.ok, port: result.port ?? DAEMON_PORT };
        const unchanged = prev != null && prev.ok === next.ok && prev.port === next.port;
        if (unchanged) return prev;
        return next;
      });
    } catch {
      setHealth((prev) => {
        const next = { ok: false, port: DAEMON_PORT };
        const unchanged = prev != null && prev.ok === next.ok && prev.port === next.port;
        if (unchanged) return prev;
        return next;
      });
    }
  }, []);

  useEffect(() => {
    checkHealth();
    const id = setInterval(checkHealth, 10000);
    return () => clearInterval(id);
  }, [checkHealth]);

  useEffect(() => {
    if (!chatTipsEnabled || tab !== "chat" || tipBannerDismissed) {
      if (!chatTipsEnabled || tab !== "chat") setTipBannerText(null);
      return;
    }
    let cancelled = false;
    fetch("/tips.json")
      .then((r) => {
        if (!r.ok) throw new Error("tips");
        return r.json();
      })
      .then((arr: unknown) => {
        if (cancelled || !Array.isArray(arr) || arr.length === 0) return;
        const day = Math.floor(Date.now() / 86400000);
        const ix = day % arr.length;
        const tip = arr[ix] as { en?: string; fr?: string };
        const text = (locale === "fr" ? tip.fr : tip.en) ?? tip.en ?? "";
        if (text) setTipBannerText(text);
      })
      .catch(() => {
        if (!cancelled) setTipBannerText(null);
      });
    return () => {
      cancelled = true;
    };
  }, [chatTipsEnabled, tab, locale, tipBannerDismissed]);

  useEffect(() => {
    setTipBannerDismissed(false);
  }, [locale]);

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
          const sid = data.session_id;
          const rows = data.turns!.map((t) => ({
            role: (t.role === "user" ? "user" : t.role === "assistant" ? "assistant" : "system") as "user" | "assistant" | "system",
            text: t.content,
          }));
          setMessages(
            rows.map((m) => {
              if (m.role !== "assistant") return m;
              const mapVisual =
                extractChatMapVisualFromAssistantText(m.text) ?? tryLoadCachedChatMapVisual(sid, m.text);
              return mapVisual ? { ...m, mapVisual } : m;
            }),
          );
          setSessionId(data.session_id);
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

  useEffect(() => {
    sessionIdRef.current = sessionId;
  }, [sessionId]);

  useEffect(() => {
    try {
      localStorage.setItem(AKASHA_CHAT_THREADS_KEY, JSON.stringify(chatThreads));
    } catch {
      /* ignore */
    }
  }, [chatThreads]);

  useEffect(() => {
    const sid = sessionId?.trim();
    if (!sid) return;
    setChatThreads((prev) => {
      if (prev.some((x) => x.id === sid)) return prev;
      const now = new Date().toISOString();
      return [{ id: sid, title: "", createdAt: now, updatedAt: now }, ...prev];
    });
  }, [sessionId]);

  const hydrateChatMessagesForSession = useCallback(async (sid: string) => {
    try {
      const data = await invoke<{ session_id?: string; turns?: Array<{ role: string; content: string }> }>(
        "get_memory_short_term",
        { sessionId: sid, port: DAEMON_PORT }
      );
      const resolved = data?.session_id ?? sid;
      const turns = data?.turns ?? [];
      if (turns.length === 0) {
        setMessages([]);
        return;
      }
      const rows = turns.map((turn) => ({
        role: (turn.role === "user" ? "user" : turn.role === "assistant" ? "assistant" : "system") as "user" | "assistant" | "system",
        text: turn.content,
      }));
      setMessages(
        rows.map((m) => {
          if (m.role !== "assistant") return m;
          const mapVisual =
            extractChatMapVisualFromAssistantText(m.text) ?? tryLoadCachedChatMapVisual(resolved, m.text);
          return mapVisual ? { ...m, mapVisual } : m;
        }),
      );
    } catch {
      setMessages([]);
    }
  }, []);

  const chatThreadLabel = useCallback(
    (th: ChatThreadEntry) => {
      if (th.pendingTitle) return t("chat.thread_new");
      if (th.title.trim()) return th.title;
      return t("chat.thread_default");
    },
    [t],
  );

  const chatThreadFolders = useMemo(() => {
    const folders = new Set<string>();
    for (const th of chatThreads) {
      const f = th.folder?.trim();
      if (f) folders.add(f);
    }
    return [...folders].sort((a, b) => a.localeCompare(b));
  }, [chatThreads]);

  const setActiveThreadFolder = useCallback((folder: string) => {
    const sid = sessionIdRef.current?.trim();
    if (!sid) return;
    const trimmed = folder.trim();
    setChatThreads((prev) =>
      prev.map((th) =>
        th.id === sid ? { ...th, folder: trimmed || undefined, updatedAt: new Date().toISOString() } : th,
      ),
    );
  }, []);

  const filteredChatThreads = useMemo(() => {
    const q = chatThreadSearch.trim().toLowerCase();
    const folderQ = chatThreadFolderFilter.trim().toLowerCase();
    const base = [...chatThreads].sort((a, b) => b.updatedAt.localeCompare(a.updatedAt));
    return base.filter((th) => {
      if (folderQ && (th.folder ?? "").trim().toLowerCase() !== folderQ) return false;
      if (!q) return true;
      const label = chatThreadLabel(th).toLowerCase();
      const snippet = (th.lastSnippet ?? "").toLowerCase();
      const folder = (th.folder ?? "").toLowerCase();
      return label.includes(q) || snippet.includes(q) || th.id.toLowerCase().includes(q) || folder.includes(q);
    });
  }, [chatThreads, chatThreadLabel, chatThreadSearch, chatThreadFolderFilter]);

  const createChatThread = useCallback(() => {
    const id = crypto.randomUUID();
    const now = new Date().toISOString();
    setChatThreads((prev) => [{ id, title: "", createdAt: now, updatedAt: now, pendingTitle: true }, ...prev]);
    setSessionId(id);
    sessionIdRef.current = id;
    try {
      localStorage.setItem(AKASHA_SESSION_ID_KEY, id);
    } catch {
      /* ignore */
    }
    setMessages([]);
    lastChatTaskIdRef.current = null;
  }, []);

  const selectChatThread = useCallback(
    async (id: string) => {
      if (id === sessionIdRef.current) return;
      setSessionId(id);
      sessionIdRef.current = id;
      try {
        localStorage.setItem(AKASHA_SESSION_ID_KEY, id);
      } catch {
        /* ignore */
      }
      await hydrateChatMessagesForSession(id);
    },
    [hydrateChatMessagesForSession],
  );

  const deleteChatThread = useCallback(
    async (id: string) => {
      if (!window.confirm(t("chat.thread_delete_confirm"))) return;
      try {
        await invoke("delete_memory_session", { sessionId: id, port: DAEMON_PORT });
      } catch (e) {
        console.error(e);
      }
      let nextList: ChatThreadEntry[] = [];
      setChatThreads((prev) => {
        nextList = prev.filter((x) => x.id !== id);
        return nextList;
      });
      const active = sessionIdRef.current;
      if (id !== active) return;
      const fallback = nextList[0]?.id ?? null;
      if (fallback) {
        setSessionId(fallback);
        sessionIdRef.current = fallback;
        try {
          localStorage.setItem(AKASHA_SESSION_ID_KEY, fallback);
        } catch {
          /* ignore */
        }
        await hydrateChatMessagesForSession(fallback);
      } else {
        const nid = crypto.randomUUID();
        const now = new Date().toISOString();
        setChatThreads([{ id: nid, title: "", createdAt: now, updatedAt: now, pendingTitle: true }]);
        setSessionId(nid);
        sessionIdRef.current = nid;
        try {
          localStorage.setItem(AKASHA_SESSION_ID_KEY, nid);
        } catch {
          /* ignore */
        }
        setMessages([]);
      }
    },
    [t, hydrateChatMessagesForSession],
  );

  // Apply theme to document (for CSS variables)
  useEffect(() => {
    document.documentElement.setAttribute("data-theme", theme);
    document.documentElement.setAttribute("data-ui-mode", uiMode);
    document.documentElement.setAttribute("data-density", uiDensity);
    applyThemeOverrides(theme, loadThemeOverrides());
  }, [theme, uiMode, uiDensity]);

  const setThemeAndSave = useCallback((next: ThemeId) => {
    setTheme(next);
    try {
      localStorage.setItem(THEME_STORAGE_KEY, next);
    } catch {
      /* ignore */
    }
  }, []);

  const setUiModeAndSave = useCallback((next: UiMode) => {
    setUiMode(next);
    try {
      localStorage.setItem(UI_MODE_STORAGE_KEY, next);
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
  // Keep sub-agent activity scrolled to latest events inside the detail panel
  useEffect(() => {
    if (tab !== "chat" || subAgentPanelCollapsed) return;
    const el = subagentsDetailRef.current;
    if (!el) return;
    requestAnimationFrame(() => {
      el.scrollTop = el.scrollHeight;
    });
  }, [runningTaskEvents, subAgentPanelCollapsed, tab]);
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
    if (humanInputModalTaskId != null) {
      setHumanInputFreeText("");
    }
  }, [humanInputModalTaskId]);
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

  // Global keyboard shortcuts: 1–9 = switch tab (when not in a modal or input)
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (humanInputModalTaskId != null) return;
      const target = e.target as HTMLElement;
      if (target?.closest("input") || target?.closest("textarea") || target?.closest("[role='dialog']")) return;
      const item = NAV_ITEMS.find((n) => n.shortcut === e.key);
      if (item) {
        e.preventDefault();
        setTab(item.id);
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [humanInputModalTaskId, setTab]);

  const fetchMission = useCallback(async () => {
    setMissionLoading(true);
    setMissionError(null);
    try {
      if (E2E_WEB) {
        const r = await fetch(e2eDaemonHttpUrl("/api/autonomous-mission"));
        if (r.status === 503) {
          setMissionError("unavailable");
          setMission(null);
          setMissionDraft(null);
          setMissionEvents([]);
          return;
        }
        if (!r.ok) throw new Error(`HTTP ${r.status}`);
        const j = normalizeMissionApi((await r.json()) as MissionApi);
        setMission(j);
        setMissionDraft(cloneMission(j));
        const ev = await fetch(e2eDaemonHttpUrl("/api/autonomous-mission/events?limit=200"));
        if (ev.ok) {
          const ej = (await ev.json()) as {
            events?: Array<{ id: number; at: string; event_type: string; payload?: unknown }>;
          };
          setMissionEvents(ej.events ?? []);
        } else {
          setMissionEvents([]);
        }
      } else {
        try {
          const raw = await invoke<MissionApi>("get_autonomous_mission", { port: DAEMON_PORT });
          const j = normalizeMissionApi(raw);
          setMission(j);
          setMissionDraft(cloneMission(j));
        } catch (err) {
          const msg = String(err);
          if (msg === "unavailable" || msg.includes("unavailable")) {
            setMissionError("unavailable");
            setMission(null);
            setMissionDraft(null);
            setMissionEvents([]);
            return;
          }
          throw err;
        }
        try {
          const ej = await invoke<{
            events?: Array<{ id: number; at: string; event_type: string; payload?: unknown }>;
          }>("get_autonomous_mission_events", { limit: 200, port: DAEMON_PORT });
          setMissionEvents(ej.events ?? []);
        } catch {
          setMissionEvents([]);
        }
      }
    } catch (e) {
      setMissionError(String(e));
      setMission(null);
      setMissionDraft(null);
    } finally {
      setMissionLoading(false);
    }
  }, []);

  useEffect(() => {
    if (tab !== "mission") return;
    void fetchMission();
  }, [tab, fetchMission]);

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
      if (E2E_WEB) {
        const r = await fetch(e2eDaemonHttpUrl("/api/docs"));
        if (!r.ok) throw new Error(`HTTP ${r.status}`);
        const j = (await r.json()) as { content?: string };
        const content = j.content ?? "";
        setDocContent(content);
        setCached("docs", content);
      } else {
        const content = await invoke<string>("get_docs", { port: DAEMON_PORT });
        setDocContent(content);
        setCached("docs", content);
      }
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

  const fetchTasksList = useCallback(async (options?: { silent?: boolean; selectTaskId?: string }) => {
    const silent = options?.silent === true;
    const selectTaskId = options?.selectTaskId;
    if (!silent) setTasksLoading(true);
    try {
      const data = await invoke<{ tasks?: Array<{ id?: string; status?: string; label?: string; created_at?: string; parent_task_id?: string; assigned_agent?: string }> }>("get_tasks", {
        port: DAEMON_PORT,
      });
      const list = data?.tasks ?? [];
      const tasks: Array<TaskListItem> = list
        .map((t) => ({
          id: t.id ?? "",
          status: normalizeTaskStatus(t.status),
          label: t.label,
          created_at: t.created_at,
          parent_task_id: t.parent_task_id,
          assigned_agent: t.assigned_agent,
        }))
        .filter((t) => t.id);
      setTasksList((prev) => (tasksListsEqual(prev, tasks) ? prev : tasks));
      setRunningTaskChips((prev) => {
        const activeIds = new Set(tasks.filter((t) => isTaskActiveStatus(t.status)).map((t) => t.id));
        let changed = false;
        const next = { ...prev };
        for (const id of Object.keys(next)) {
          if (!activeIds.has(id)) {
            delete next[id];
            changed = true;
          }
        }
        return changed ? next : prev;
      });
      if (selectTaskId) {
        const idx = tasks.findIndex((t) => t.id === selectTaskId);
        if (idx >= 0) setTasksSelected(idx);
      } else {
        setTasksSelected((prev) => (prev >= tasks.length && tasks.length > 0 ? tasks.length - 1 : prev));
      }
      setCached("tasks", tasks);
    } catch {
      setTasksList([]);
    } finally {
      if (!silent) setTasksLoading(false);
    }
  }, []);

  const openChatTaskDetail = useCallback(
    (taskId: string) => {
      void fetchTasksList({ selectTaskId: taskId });
      setTab("tasks");
      setTaskPanelSections((prev) => ({ ...prev, events: true }));
    },
    [fetchTasksList, setTab],
  );

  const filteredTasksList = useMemo(() => {
    let list = tasksList;
    if (taskListFilter === "active") {
      list = list.filter((t) => isTaskActiveStatus(t.status));
    } else {
      list = list.filter((t) => {
        const s = normalizeTaskStatus(t.status);
        return s === "completed" || s === "failed" || s === "cancelled" || s === "interrupted";
      });
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

  const taskTreeData = useMemo(() => {
    const byId = new Map(tasksList.map((task) => [task.id, task]));
    const visibleIds = new Set<string>();
    const childrenByParent = new Map<string | null, TaskListItem[]>();

    for (const task of tasksList) {
      const parentId = task.parent_task_id && byId.has(task.parent_task_id) ? task.parent_task_id : null;
      const bucket = childrenByParent.get(parentId) ?? [];
      bucket.push(task);
      childrenByParent.set(parentId, bucket);
    }

    for (const task of filteredTasksList) {
      let current: TaskListItem | undefined = task;
      while (current) {
        if (visibleIds.has(current.id)) break;
        visibleIds.add(current.id);
        current = current.parent_task_id ? byId.get(current.parent_task_id) : undefined;
      }
    }

    const branchIds = new Set<string>();
    for (const [parentId, children] of childrenByParent.entries()) {
      if (!parentId || !visibleIds.has(parentId)) continue;
      if (children.some((child) => visibleIds.has(child.id))) {
        branchIds.add(parentId);
      }
    }

    const roots = tasksList.filter((task) => {
      if (!visibleIds.has(task.id)) return false;
      if (!task.parent_task_id) return true;
      return !byId.has(task.parent_task_id);
    });

    return { byId, visibleIds, childrenByParent, branchIds, roots };
  }, [tasksList, filteredTasksList]);

  const selectedTask = tasksList[tasksSelected] ?? null;

  useEffect(() => {
    setCollapsedTaskBranches((prev) => {
      let changed = false;
      const next: Record<string, boolean> = {};

      for (const [taskId, isCollapsed] of Object.entries(prev)) {
        if (taskTreeData.branchIds.has(taskId)) {
          next[taskId] = isCollapsed;
        } else {
          changed = true;
        }
      }

      let current: TaskListItem | undefined | null = selectedTask;
      while (current?.parent_task_id) {
        const parentId = current.parent_task_id;
        if (taskTreeData.branchIds.has(parentId) && next[parentId]) {
          next[parentId] = false;
          changed = true;
        }
        current = taskTreeData.byId.get(parentId);
      }

      if (!changed) return prev;
      persistCollapsedTaskBranches(next);
      return next;
    });
  }, [selectedTask, taskTreeData, persistCollapsedTaskBranches]);

  const taskTreeRows = useMemo(() => {
    const rows: Array<{
      task: TaskListItem;
      depth: number;
      hasChildren: boolean;
      visibleChildCount: number;
      rootId: string;
      isCollapsed: boolean;
    }> = [];

    const visit = (task: TaskListItem, depth: number, rootId: string) => {
      if (!taskTreeData.visibleIds.has(task.id)) return;
      const allChildren = taskTreeData.childrenByParent.get(task.id) ?? [];
      const visibleChildren = allChildren.filter((child) => taskTreeData.visibleIds.has(child.id));
      const isCollapsed = visibleChildren.length > 0 ? !!collapsedTaskBranches[task.id] : false;
      rows.push({
        task,
        depth,
        hasChildren: visibleChildren.length > 0,
        visibleChildCount: visibleChildren.length,
        rootId,
        isCollapsed,
      });
      if (isCollapsed) return;
      for (const child of visibleChildren) {
        visit(child, depth + 1, rootId);
      }
    };

    for (const root of taskTreeData.roots) {
      visit(root, 0, root.id);
    }

    return rows;
  }, [taskTreeData, collapsedTaskBranches]);

  const visibleTaskEvents = useMemo(() => {
    const compact = collapseStreamedProgressEvents(tasksEvents, selectedTask?.id ?? "root");
    if (!isSimpleMode) return compact;
    return [...compact].slice(-8).reverse();
  }, [isSimpleMode, tasksEvents, selectedTask]);

  const selectedTaskHierarchy = useMemo(() => {
    if (!selectedTask) return [] as TaskListItem[];
    const lineage: TaskListItem[] = [];
    let current: TaskListItem | undefined = selectedTask;
    while (current) {
      lineage.unshift(current);
      current = current.parent_task_id ? taskTreeData.byId.get(current.parent_task_id) : undefined;
    }
    return lineage;
  }, [selectedTask, taskTreeData]);

  const selectedTaskRootId = selectedTaskHierarchy[0]?.id ?? null;

  const selectedTaskAncestorIds = useMemo(() => {
    return new Set(selectedTaskHierarchy.slice(0, -1).map((task) => task.id));
  }, [selectedTaskHierarchy]);

  const setAllTaskBranchesCollapsed = useCallback((collapsed: boolean) => {
    const next: Record<string, boolean> = {};
    for (const branchId of taskTreeData.branchIds) {
      next[branchId] = collapsed;
    }
    if (collapsed && selectedTaskHierarchy.length > 1) {
      for (const task of selectedTaskHierarchy.slice(0, -1)) {
        if (taskTreeData.branchIds.has(task.id)) next[task.id] = false;
      }
    }
    persistCollapsedTaskBranches(next);
    setCollapsedTaskBranches(next);
  }, [taskTreeData, selectedTaskHierarchy, persistCollapsedTaskBranches]);

  const taskStatusCounts = useMemo(() => {
    return tasksList.reduce(
      (acc, task) => {
        const status = (task.status ?? "").toLowerCase();
        if (status === "running") acc.running += 1;
        else if (status === "pending") acc.pending += 1;
        else if (status === "completed") acc.completed += 1;
        else if (status === "failed") acc.failed += 1;
        return acc;
      },
      { running: 0, pending: 0, completed: 0, failed: 0 }
    );
  }, [tasksList]);

  const taskExecutionView = useMemo(() => {
    return buildExecutionSteps(tasksEvents, tasksList, selectedTask?.id, taskStepsTodos);
  }, [tasksEvents, tasksList, selectedTask?.id, taskStepsTodos]);

  const fetchEventTriggers = useCallback(async () => {
    try {
      const data = await invoke<{ triggers?: Array<{ id?: string; name?: string; enabled?: boolean; trigger_type?: string; last_fired_at?: string | null }> }>(
        "get_event_triggers",
        { port: DAEMON_PORT }
      );
      setEventTriggers(
        (data?.triggers ?? [])
          .filter((t): t is { id: string; name: string; enabled: boolean; trigger_type: string; last_fired_at?: string | null } => Boolean(t.id))
          .map((t) => ({
            id: t.id!,
            name: t.name ?? t.id!,
            enabled: t.enabled !== false,
            trigger_type: t.trigger_type ?? "webhook",
            last_fired_at: t.last_fired_at,
          }))
      );
    } catch {
      setEventTriggers([]);
    }
  }, []);

  const selectedTaskSummary = useMemo(() => {
    if (!selectedTask) return null;
    const runningChip = selectedTask.status === "running" ? runningTaskChips[selectedTask.id] : undefined;
    const latestEvent = tasksEvents.length > 0 ? tasksEvents[tasksEvents.length - 1] : null;
    const routeEvent = [...tasksEvents].reverse().find((e) => e.event_type === "llm_route_planned");
    const llmEvent = [...tasksEvents].reverse().find((e) => e.event_type === "llm_call_started" || e.event_type === "llm_call_finished");
    const routeHint =
      (llmEvent ? summarizeTaskEvent(llmEvent) : "") ||
      (routeEvent ? summarizeTaskEvent(routeEvent) : "");
    return {
      runningChip,
      latestEvent,
      latestSummary: latestEvent ? summarizeTaskEvent(latestEvent) : "",
      routeHint: routeHint || undefined,
      progressPct: runningChip?.pct,
    };
  }, [selectedTask, runningTaskChips, tasksEvents, summarizeTaskEvent]);

  const taskDiscussionHighlights = useMemo(() => {
    const minimalSignalTypes = new Set([
      "task_decomposed",
      "sub_agent_spawned",
      "plan_proposed",
      "plan_committed",
      "timeline_milestone",
      "contract_violation",
      "deterministic_preferred_tool_no_success",
    ]);

    const normalSignalTypes = new Set([
      "task_decomposed",
      "sub_agent_spawned",
      "plan_proposed",
      "plan_committed",
      "timeline_milestone",
      "subagent_startup_started",
      "subagent_startup_pending",
      "progress_update",
      "contract_violation",
      "subtask_started",
      "subtask_completed",
      "deterministic_preferred_tool_attempt",
      "deterministic_preferred_tool_result",
      "deterministic_preferred_tool_no_success",
      "tool_invoked",
    ]);

    const signalTypes = taskOrchestrationDebugLevel === "minimal" ? minimalSignalTypes : normalSignalTypes;

    const rows = tasksEvents.map((event) => ({
        event,
        highlights: extractTaskDiscussionHighlights(event),
      }));

    const filteredRows = taskOrchestrationDebugLevel === "full"
      ? rows
      : rows.filter(({ event, highlights }) => signalTypes.has(event.event_type) || highlights.length > 0);

    const maxRows = isSimpleMode
      ? (taskOrchestrationDebugLevel === "full" ? 16 : 8)
      : (taskOrchestrationDebugLevel === "full" ? 48 : 24);

    if (isSimpleMode) return [...filteredRows].slice(-maxRows).reverse();
    return filteredRows.slice(-maxRows);
  }, [tasksEvents, extractTaskDiscussionHighlights, isSimpleMode, taskOrchestrationDebugLevel]);

  const taskDisplayLabel = (task: TaskListItem) => (task.label && task.label.trim() ? task.label.trim() : t("tasks.task_unnamed") + task.id.slice(-8));

  const exportAdvancedViewCsv = useCallback((visual: EventAdvancedView) => {
    try {
      const csv = advancedViewToCsv(visual);
      const blob = new Blob([csv], { type: "text/csv;charset=utf-8" });
      const url = URL.createObjectURL(blob);
      const ts = new Date().toISOString().replace(/[:.]/g, "-");
      const filename = `akasha_${visual.kind}_${ts}.csv`;
      const anchor = document.createElement("a");
      anchor.href = url;
      anchor.download = filename;
      document.body.appendChild(anchor);
      anchor.click();
      anchor.remove();
      setTimeout(() => URL.revokeObjectURL(url), 0);
    } catch (err) {
      console.error("Failed to export visualization CSV", err);
    }
  }, []);

  const exportAdvancedViewPng = useCallback((visual: EventAdvancedView) => {
    try {
      const width = visual.kind === "map" ? 1200 : 1400;
      const height = visual.kind === "map" ? 520 : 560;
      const canvas = renderAdvancedViewToCanvas(visual, width, height);
      const ts = new Date().toISOString().replace(/[:.]/g, "-");
      const filename = `akasha_${visual.kind}_${ts}.png`;
      const saveBlob = (blob: Blob) => {
        const url = URL.createObjectURL(blob);
        const anchor = document.createElement("a");
        anchor.href = url;
        anchor.download = filename;
        document.body.appendChild(anchor);
        anchor.click();
        anchor.remove();
        setTimeout(() => URL.revokeObjectURL(url), 0);
      };
      canvas.toBlob((blob) => {
        if (blob) {
          saveBlob(blob);
          return;
        }
        const dataUrl = canvas.toDataURL("image/png");
        const anchor = document.createElement("a");
        anchor.href = dataUrl;
        anchor.download = filename;
        document.body.appendChild(anchor);
        anchor.click();
        anchor.remove();
      }, "image/png");
    } catch (err) {
      console.error("Failed to export visualization PNG", err);
    }
  }, []);

  const clampEventVisualScale = useCallback((v: number) => Math.max(0.5, Math.min(8, v)), []);

  const clearEventVisualGestures = useCallback(() => {
    eventVisualPointersRef.current.clear();
    eventVisualPinchBaseDistanceRef.current = null;
    eventVisualPinchBaseTransformRef.current = null;
    setEventVisualDragging(null);
  }, []);

  const resetEventVisualViewport = useCallback(() => {
    setEventVisualTransform({ scale: 1, tx: 0, ty: 0 });
    clearEventVisualGestures();
  }, [clearEventVisualGestures]);

  const eventVisualZoomPercent = Math.round(eventVisualTransform.scale * 100);

  const zoomEventVisualAt = useCallback((cx: number, cy: number, factor: number) => {
    setEventVisualTransform((prev) => {
      const nextScale = clampEventVisualScale(prev.scale * factor);
      const nextTx = cx - ((cx - prev.tx) / prev.scale) * nextScale;
      const nextTy = cy - ((cy - prev.ty) / prev.scale) * nextScale;
      return { scale: nextScale, tx: nextTx, ty: nextTy };
    });
  }, [clampEventVisualScale]);

  const handleEventVisualWheel = useCallback((ev: ReactWheelEvent<HTMLDivElement>) => {
    ev.preventDefault();
    const rect = ev.currentTarget.getBoundingClientRect();
    const cx = ev.clientX - rect.left;
    const cy = ev.clientY - rect.top;
    const factor = ev.deltaY < 0 ? 1.12 : 1 / 1.12;
    zoomEventVisualAt(cx, cy, factor);
  }, [zoomEventVisualAt]);

  const handleEventVisualPointerDown = useCallback((ev: ReactPointerEvent<HTMLDivElement>) => {
    if (ev.pointerType === "mouse" && ev.button !== 0) return;
    ev.currentTarget.setPointerCapture(ev.pointerId);
    eventVisualPointersRef.current.set(ev.pointerId, { x: ev.clientX, y: ev.clientY });

    const points = Array.from(eventVisualPointersRef.current.values());
    if (points.length >= 2) {
      const [a, b] = points;
      if (!a || !b) return;
      const dist = Math.hypot(b.x - a.x, b.y - a.y);
      eventVisualPinchBaseDistanceRef.current = Math.max(dist, 1e-6);
      eventVisualPinchBaseTransformRef.current = { ...eventVisualTransform };
      setEventVisualDragging(null);
      return;
    }

    setEventVisualDragging({
      startX: ev.clientX,
      startY: ev.clientY,
      baseTx: eventVisualTransform.tx,
      baseTy: eventVisualTransform.ty,
    });
  }, [eventVisualTransform]);

  const handleEventVisualPointerMove = useCallback((ev: ReactPointerEvent<HTMLDivElement>) => {
    if (!eventVisualPointersRef.current.has(ev.pointerId)) return;
    eventVisualPointersRef.current.set(ev.pointerId, { x: ev.clientX, y: ev.clientY });
    const points = Array.from(eventVisualPointersRef.current.values());

    if (points.length >= 2 && eventVisualPinchBaseDistanceRef.current != null && eventVisualPinchBaseTransformRef.current != null) {
      const [a, b] = points;
      if (!a || !b) return;
      const dist = Math.hypot(b.x - a.x, b.y - a.y);
      const factor = dist / eventVisualPinchBaseDistanceRef.current;
      const centerX = (a.x + b.x) * 0.5;
      const centerY = (a.y + b.y) * 0.5;
      const base = eventVisualPinchBaseTransformRef.current;
      const nextScale = clampEventVisualScale(base.scale * factor);
      const nextTx = centerX - ((centerX - base.tx) / base.scale) * nextScale;
      const nextTy = centerY - ((centerY - base.ty) / base.scale) * nextScale;
      setEventVisualTransform({ scale: nextScale, tx: nextTx, ty: nextTy });
      return;
    }

    if (!eventVisualDragging) return;
    const dx = ev.clientX - eventVisualDragging.startX;
    const dy = ev.clientY - eventVisualDragging.startY;
    setEventVisualTransform((prev) => ({
      ...prev,
      tx: eventVisualDragging.baseTx + dx,
      ty: eventVisualDragging.baseTy + dy,
    }));
  }, [eventVisualDragging, clampEventVisualScale]);

  const handleEventVisualPointerEnd = useCallback((ev: ReactPointerEvent<HTMLDivElement>) => {
    try {
      ev.currentTarget.releasePointerCapture(ev.pointerId);
    } catch {
      /* ignore */
    }
    eventVisualPointersRef.current.delete(ev.pointerId);
    const points = Array.from(eventVisualPointersRef.current.values());
    if (points.length >= 2) {
      const [a, b] = points;
      if (a && b) {
        const dist = Math.hypot(b.x - a.x, b.y - a.y);
        eventVisualPinchBaseDistanceRef.current = Math.max(dist, 1e-6);
      }
      return;
    }
    eventVisualPinchBaseDistanceRef.current = null;
    eventVisualPinchBaseTransformRef.current = null;
    if (points.length === 1) {
      const p = points[0];
      if (p) {
        setEventVisualDragging(() => ({
          startX: p.x,
          startY: p.y,
          baseTx: eventVisualTransform.tx,
          baseTy: eventVisualTransform.ty,
        }));
        return;
      }
    }
    setEventVisualDragging(null);
  }, [eventVisualTransform.tx, eventVisualTransform.ty]);

  const fetchTasksEvents = useCallback(async (taskId: string) => {
    try {
      const data = await invoke<unknown>("get_task_events", { taskId, port: DAEMON_PORT });
      const list = normalizeTaskEventsInvokeResponse(data);
      const mapped = list.map((e) => ({
        event_type: e.event_type ?? "?",
        payload: e.payload,
        at: e.at ?? "",
        task_id: e.task_id,
      }));
      setTasksEvents((prev) => {
        if (tasksEventsTaskIdRef.current !== taskId) {
          tasksEventsTaskIdRef.current = taskId;
          return mapped;
        }
        return mergeTaskEvents(prev, mapped);
      });
    } catch {
      /* keep previous events on transient fetch errors */
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
      if (selectedTaskIdForTodosRef.current === taskId) {
        setTaskStepsTodos((prev) => (taskTodoRowsEqual(prev, rows) ? prev : rows));
      }
    } catch {
      if (selectedTaskIdForTodosRef.current === taskId) setTaskStepsTodos([]);
    }
  }, []);

  useEffect(() => {
    fetchTaskStepsRef.current = fetchTaskSteps;
  }, [fetchTaskSteps]);

  useEffect(() => {
    fetchTasksEventsRef.current = fetchTasksEvents;
  }, [fetchTasksEvents]);

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

  const fetchWakeups = useCallback(async () => {
    try {
      const res = await fetch(e2eDaemonHttpUrl("/api/wakeups"));
      if (!res.ok) throw new Error(String(res.status));
      const data = (await res.json()) as { wakeups?: WakeupRow[] };
      setWakeups(data.wakeups ?? []);
    } catch {
      setWakeups([]);
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
    if (tab === "calendar" && calendarSubTab === "wakeups") void fetchWakeups();
  }, [tab, calendarSubTab, fetchWakeups]);

  useEffect(() => {
    if (tab !== "tasks") return;
    const cached = getCached<Array<TaskListItem>>("tasks");
    if (cached != null) {
      setTasksList(cached);
      setTasksSelected((prev) => (prev >= cached.length && cached.length > 0 ? cached.length - 1 : prev));
      setTasksLoading(false);
    } else {
      fetchTasksList();
    }
    void fetchEventTriggers();
  }, [tab, fetchTasksList, fetchEventTriggers]);

  const applyChatStreamProgress = useCallback((taskId: string, msg: string) => {
    if (!taskId) return;
    const sidForTask = taskIdToSessionIdRef.current[taskId];
    if (!sidForTask || sidForTask !== sessionIdRef.current) return;
    if (!msg.trim()) return;
    if (isChatStreamToolPhase(msg)) {
      setMessages((prev) => {
        const idx = findLastChatAssistantIndex(prev, taskId);
        if (idx < 0) return prev;
        const next = [...prev];
        const cachedMap = next[idx].mapVisual ?? chatMapByTaskIdRef.current[taskId];
        next[idx] = {
          role: "assistant",
          text: ackTextByTaskRef.current[taskId] ?? next[idx].text,
          taskId,
          streaming: false,
          mapVisual: cachedMap,
          usage: next[idx].usage,
        };
        return next;
      });
      return;
    }
    if (shouldChatStreamProgress(msg)) {
      setMessages((prev) => {
        const idx = findLastChatAssistantIndex(prev, taskId);
        if (idx < 0) return prev;
        const next = [...prev];
        const cachedMap = next[idx].mapVisual ?? chatMapByTaskIdRef.current[taskId];
        next[idx] = { role: "assistant", text: msg, taskId, streaming: true, mapVisual: cachedMap, usage: next[idx].usage };
        return next;
      });
    }
  }, []);

  // SSE: subscribe to daemon events for real-time updates (< 1s) when daemon is healthy.
  // On disconnect, fall back to polling task list every 5s (see agent_client_event_contract.md).
  useEffect(() => {
    if (!health?.ok) return;
    const sseTypes =
      "progress_update,task_completed,task_failed,task_cancelled,todo_list_updated,user_steering_queued,user_follow_up_queued,user_steering_applied,user_follow_up_applied,user_queue_flushed";
    const base = E2E_WEB
      ? e2eDaemonHttpUrl("/api/events")
      : `http://127.0.0.1:${health.port ?? DAEMON_PORT}/api/events`;
    const url = `${base}?types=${encodeURIComponent(sseTypes)}`;
    let es: EventSource | null = null;
    let pollFallbackId: number | null = null;
    const startPollFallback = () => {
      if (pollFallbackId != null) return;
      pollFallbackId = window.setInterval(() => {
        void fetchTasksList({ silent: true });
        void fetchPendingHumanInput();
      }, 5000);
    };
    const stopPollFallback = () => {
      if (pollFallbackId != null) {
        window.clearInterval(pollFallbackId);
        pollFallbackId = null;
      }
    };
    try {
      es = new EventSource(url);
      es.onopen = () => stopPollFallback();
      es.onmessage = (msgEv) => {
        try {
          const d = JSON.parse(msgEv.data) as {
            event_type?: string;
            payload?: { task_id?: string; message?: string; text?: string } | Record<string, unknown>;
            correlation_id?: string | null;
          };
          const shouldRefreshTaskList =
            d.event_type === "task_completed" ||
            d.event_type === "task_failed" ||
            d.event_type === "task_cancelled" ||
            d.event_type === "user_steering_queued" ||
            d.event_type === "user_follow_up_queued" ||
            d.event_type === "user_steering_applied" ||
            d.event_type === "user_follow_up_applied" ||
            d.event_type === "todo_list_updated";
          if (shouldRefreshTaskList) void fetchTasksList({ silent: true });
          if (d.event_type === "task_completed" || d.event_type === "task_failed" || d.event_type === "task_cancelled") {
            void fetchPendingHumanInput();
          }
          if (d.event_type === "progress_update" && d.payload && typeof d.payload === "object") {
            const p = d.payload as Record<string, unknown>;
            const tid = typeof p.task_id === "string" ? p.task_id : "";
            const streamMsg = typeof p.message === "string" ? p.message : "";
            if (tid && streamMsg) applyChatStreamProgress(tid, streamMsg);
          }
          if (
            d.event_type === "user_steering_queued" ||
            d.event_type === "user_follow_up_queued" ||
            d.event_type === "user_steering_applied" ||
            d.event_type === "user_follow_up_applied"
          ) {
            const p = d.payload && typeof d.payload === "object" ? (d.payload as Record<string, unknown>) : null;
            const tid =
              (p && typeof p.task_id === "string" ? p.task_id : "") ||
              (typeof d.correlation_id === "string" ? d.correlation_id : "");
            const hint =
              (p && typeof p.text === "string" ? p.text : "") ||
              (p && typeof p.message === "string" ? p.message : "") ||
              d.event_type;
            if (tid && hint) applyChatStreamProgress(tid, `[queue] ${hint}`);
          }
          if (d.event_type === "task_completed" || d.event_type === "task_failed" || d.event_type === "task_cancelled") {
            const p = d.payload && typeof d.payload === "object" ? (d.payload as Record<string, unknown>) : null;
            const tid = p && typeof p.task_id === "string" ? p.task_id : "";
            if (tid) {
              setRunningTaskChips((prev) => {
                if (prev[tid] === undefined) return prev;
                const next = { ...prev };
                delete next[tid];
                return next;
              });
              if (selectedTaskIdForTodosRef.current === tid) {
                void fetchTasksEventsRef.current(tid);
              }
            }
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
        startPollFallback();
      };
    } catch {
      startPollFallback();
    }
    return () => {
      es?.close();
      stopPollFallback();
    };
  }, [health?.ok, health?.port, fetchTasksList, fetchPendingHumanInput, applyChatStreamProgress]);

  useEffect(() => {
    const task = tasksList[tasksSelected];
    if (task?.id) {
      void fetchTasksEvents(task.id);
    } else {
      tasksEventsTaskIdRef.current = null;
      setTasksEvents([]);
    }
  }, [tasksList, tasksSelected, fetchTasksEvents]);

  useEffect(() => {
    if (tab !== "tasks") return;
    const task = tasksList[tasksSelected];
    if (!task?.id || !isTaskActiveStatus(task.status)) return;
    const taskId = task.id;
    const timer = window.setInterval(() => {
      void fetchTasksEventsRef.current(taskId);
    }, 2000);
    return () => window.clearInterval(timer);
  }, [tab, tasksList, tasksSelected]);

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
      const [reportsData, runsData] = await Promise.all([
        invoke<{ reports?: Array<{ schedule_name?: string; message?: string; ended_at?: string; schedule_id?: string; task_id?: string; task_run_id?: string }> }>(
          "get_schedule_run_reports",
          { port: DAEMON_PORT }
        ),
        invoke<{ task_runs?: Array<{ id?: string; schedule_id?: string; task_id?: string; status?: string; planned_for?: string; started_at?: string; ended_at?: string; label?: string }> }>(
          "get_task_runs",
          { port: DAEMON_PORT }
        ),
      ]);
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
      const runsById = new Map(runs.map((run) => [run.id, run]));
      const runsByTaskId = new Map<string, typeof runs>();
      for (const run of runs) {
        const key = run.task_id;
        const prev = runsByTaskId.get(key);
        if (prev) prev.push(run);
        else runsByTaskId.set(key, [run]);
      }
      const isoDistance = (left?: string, right?: string) => {
        if (!left || !right) return Number.POSITIVE_INFINITY;
        const leftMs = Date.parse(left);
        const rightMs = Date.parse(right);
        if (Number.isNaN(leftMs) || Number.isNaN(rightMs)) return Number.POSITIVE_INFINITY;
        return Math.abs(leftMs - rightMs);
      };
      setScheduleReports((reportsData?.reports ?? []).map((r) => {
        const byId = r.task_run_id ? runsById.get(r.task_run_id) : undefined;
        let matchedRun = byId;
        if (!matchedRun && r.task_id) {
          const candidates = (runsByTaskId.get(r.task_id) ?? []).filter((candidate) => !r.schedule_id || candidate.schedule_id === r.schedule_id);
          if (candidates.length > 0) {
            if (r.ended_at) {
              matchedRun = candidates.reduce((best, candidate) => {
                const bestAnchor = best.ended_at ?? best.started_at ?? best.planned_for;
                const candidateAnchor = candidate.ended_at ?? candidate.started_at ?? candidate.planned_for;
                return isoDistance(r.ended_at, candidateAnchor) < isoDistance(r.ended_at, bestAnchor) ? candidate : best;
              });
            } else {
              matchedRun = candidates[0];
            }
          }
        }
        return {
          schedule_name: r.schedule_name ?? "",
          message: r.message ?? "Exécuté.",
          ended_at: r.ended_at,
          schedule_id: r.schedule_id,
          task_id: r.task_id,
          task_run_id: r.task_run_id,
          task_label: matchedRun?.label,
          status: matchedRun?.status,
          planned_for: matchedRun?.planned_for,
          started_at: matchedRun?.started_at,
          run_ended_at: matchedRun?.ended_at,
        };
      }));
    } catch {
      setScheduleReports([]);
    }
  }, []);

  const fetchPluginStatus = useCallback(async () => {
    setPluginStatusLoading(true);
    setPluginStatusError(null);
    try {
      let list: PluginStatusEntry[] = [];
      if (E2E_WEB) {
        const res = await fetch(e2eDaemonHttpUrl("/api/plugins"));
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        const j = (await res.json()) as unknown;
        list = Array.isArray(j) ? (j as PluginStatusEntry[]) : [];
      } else {
        const inv = await invoke<PluginStatusEntry[]>("get_plugins", { port: DAEMON_PORT });
        list = Array.isArray(inv) ? inv : [];
      }
      setPluginStatusList(list);
    } catch (e) {
      setPluginStatusError(String(e));
      setPluginStatusList([]);
    } finally {
      setPluginStatusLoading(false);
    }
  }, []);

  useEffect(() => {
    if (tab === "scheduled") fetchScheduleReports();
  }, [tab, fetchScheduleReports]);

  const resetPluginReputation = useCallback(async (pluginId?: string) => {
    setPluginReputationResetLoading(true);
    setPluginReputationResetError(null);
    setPluginReputationResetMessage(null);
    try {
      const trimmed = (pluginId ?? "").trim();
      const res = await fetch(e2eDaemonHttpUrl("/api/plugins/reputation/reset"), {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: trimmed ? JSON.stringify({ plugin_id: trimmed }) : JSON.stringify({}),
      });

      const text = await res.text();
      let payload: { message?: string } | null = null;
      try {
        payload = text ? (JSON.parse(text) as { message?: string }) : null;
      } catch {
        payload = null;
      }

      if (!res.ok) {
        throw new Error(payload?.message || text || `HTTP ${res.status}`);
      }

      setPluginReputationResetMessage(
        payload?.message || (trimmed ? t("settings.plugin_reputation_reset_ok") : t("settings.plugin_reputation_reset_all_ok"))
      );
      await fetchPluginStatus();
    } catch (e) {
      setPluginReputationResetError(String(e));
    } finally {
      setPluginReputationResetLoading(false);
    }
  }, [t, fetchPluginStatus]);

  const reloadPluginsFromDisk = useCallback(async () => {
    setPluginStatusError(null);
    try {
      if (E2E_WEB) {
        const res = await fetch(e2eDaemonHttpUrl("/api/plugins/reload"), { method: "POST" });
        if (!res.ok) throw new Error(await res.text());
      } else {
        await invoke("reload_plugins", { port: DAEMON_PORT });
      }
      await fetchPluginStatus();
    } catch (e) {
      setPluginStatusError(String(e));
    }
  }, [fetchPluginStatus]);

  const setPluginRowEnabled = useCallback(
    async (pluginId: string, enabled: boolean) => {
      const id = pluginId.trim();
      if (!id) return;
      setPluginTableBusyId(id);
      setPluginStatusError(null);
      try {
        if (E2E_WEB) {
          const action = enabled ? "enable" : "disable";
          const res = await fetch(e2eDaemonHttpUrl(`/api/plugins/${encodeURIComponent(id)}/${action}`), { method: "POST" });
          if (!res.ok) throw new Error(await res.text());
        } else {
          await invoke("set_plugin_enabled", { pluginId: id, enabled, port: DAEMON_PORT });
        }
        await fetchPluginStatus();
      } catch (e) {
        setPluginStatusError(String(e));
      } finally {
        setPluginTableBusyId(null);
      }
    },
    [fetchPluginStatus],
  );

  const uninstallPluginRow = useCallback(
    async (pluginId: string) => {
      const id = pluginId.trim();
      if (!id) return;
      const msg = t("settings.plugins_uninstall_confirm").replace("{id}", id);
      if (!window.confirm(msg)) return;
      setPluginTableBusyId(id);
      setPluginStatusError(null);
      try {
        if (E2E_WEB) {
          const res = await fetch(e2eDaemonHttpUrl(`/api/plugins/${encodeURIComponent(id)}/uninstall`), { method: "POST" });
          if (!res.ok) throw new Error(await res.text());
        } else {
          await invoke("uninstall_plugin", { pluginId: id, port: DAEMON_PORT });
        }
        await fetchPluginStatus();
      } catch (e) {
        setPluginStatusError(String(e));
      } finally {
        setPluginTableBusyId(null);
      }
    },
    [fetchPluginStatus, t],
  );

  const fetchSystemEndpoint = useCallback(async (path: string, init?: RequestInit): Promise<{ ok: boolean; status: number; text: string }> => {
    if (E2E_WEB) {
      const res = await fetch(e2eDaemonHttpUrl(path), init);
      return { ok: res.ok, status: res.status, text: await res.text() };
    }
    const method = (init?.method ?? "GET").toUpperCase();
    if (method === "GET") {
      return invoke<{ ok: boolean; status: number; text: string }>("daemon_get_text", {
        path,
        port: DAEMON_PORT,
      });
    }
    const body = typeof init?.body === "string" ? init.body : undefined;
    return invoke<{ ok: boolean; status: number; text: string }>("daemon_request", {
      method,
      path,
      body,
      port: DAEMON_PORT,
    });
  }, []);

  const requestSystemEndpoint = useCallback(
    async (method: string, path: string, body?: string) => fetchSystemEndpoint(path, { method, body }),
    [fetchSystemEndpoint],
  );

  const notify = useNotify();
  const [resumeBriefBusy, setResumeBriefBusy] = useState(false);

  const handleResumeBrief = useCallback(async () => {
    const sid = sessionId?.trim();
    if (!sid) {
      notify({
        level: "warning",
        title: locale === "en" ? "No active session" : "Aucune session active",
        source: "resume",
      });
      return;
    }
    setResumeBriefBusy(true);
    try {
      const q = new URLSearchParams({ session_id: sid });
      const res = await fetchSystemEndpoint(`/api/session/resume-brief?${q.toString()}`);
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}: ${res.text.slice(0, 200)}`);
      }
      let summary = res.text;
      try {
        const j = JSON.parse(res.text) as {
          short_term_turn_count?: number;
          compaction_count?: number;
          session_state?: { goals?: unknown; constraints?: unknown };
        };
        const turns = j.short_term_turn_count ?? 0;
        const comp = j.compaction_count ?? 0;
        summary =
          locale === "en"
            ? `Session resume: ${turns} turn(s), ${comp} compaction(s).`
            : `Reprise session : ${turns} tour(s), ${comp} compaction(s).`;
        const st = j.session_state;
        if (st && (st.goals != null || st.constraints != null)) {
          summary += `\n\n${JSON.stringify({ goals: st.goals, constraints: st.constraints }, null, 2)}`;
        }
      } catch {
        /* keep raw text */
      }
      if (tab === "chat") {
        setMessages((prev) => [...prev, { role: "system", text: summary }]);
      } else {
        notify({
          level: "info",
          title: locale === "en" ? "Session brief" : "Brief session",
          detail: summary.slice(0, 600),
          source: "resume",
        });
      }
    } catch (e) {
      notify({
        level: "error",
        title: locale === "en" ? "Resume failed" : "Échec reprise",
        detail: String(e),
        source: "resume",
      });
    } finally {
      setResumeBriefBusy(false);
    }
  }, [sessionId, fetchSystemEndpoint, locale, notify, tab]);

  const loadInstalledSkills = useCallback(async () => {
    setSkillsCatalogLoading(true);
    setSkillsCatalogText(null);
    try {
      const res = await fetchSystemEndpoint("/api/skills");
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const list = JSON.parse(res.text) as Array<{ name?: string; description?: string }>;
      if (!Array.isArray(list) || list.length === 0) {
        setSkillsCatalogText(locale === "en" ? "No skills installed." : "Aucun skill installé.");
      } else {
        setSkillsCatalogText(
          list
            .map((s) => `• ${s.name ?? "?"}${s.description ? ` — ${s.description}` : ""}`)
            .join("\n"),
        );
      }
    } catch (e) {
      setSkillsCatalogText(String(e));
    } finally {
      setSkillsCatalogLoading(false);
    }
  }, [fetchSystemEndpoint, locale]);

  const loadHandoffModels = useCallback(async () => {
    try {
      const providers = await invoke<Record<string, string[]>>("get_router_models", { port: DAEMON_PORT });
      const rows: Array<{ provider: string; model: string }> = [];
      for (const [provider, models] of Object.entries(providers ?? {})) {
        for (const model of models ?? []) rows.push({ provider, model });
      }
      setHandoffModels(rows);
    } catch (e) {
      setHandoffStatus(String(e));
      setHandoffModels([]);
    }
  }, []);

  const submitSessionHandoff = useCallback(async () => {
    const sid = sessionId?.trim();
    if (!sid) {
      setHandoffStatus(locale === "en" ? "No active session." : "Aucune session active.");
      return;
    }
    setHandoffBusy(true);
    setHandoffStatus(null);
    try {
      const body = JSON.stringify({
        session_id: sid,
        target_model: handoffTargetModel || undefined,
        target_provider: handoffTargetProvider || undefined,
        task_id: handoffTaskId.trim() || undefined,
      });
      const res = await requestSystemEndpoint("POST", "/api/session/handoff", body);
      if (!res.ok) throw new Error(res.text || `HTTP ${res.status}`);
      setHandoffStatus(locale === "en" ? "Handoff sent." : "Handoff envoyé.");
    } catch (e) {
      setHandoffStatus(String(e));
    } finally {
      setHandoffBusy(false);
    }
  }, [handoffTargetModel, handoffTargetProvider, handoffTaskId, locale, requestSystemEndpoint, sessionId]);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const res = await fetchSystemEndpoint("/api/cookbook/recommendations");
        if (cancelled || !res.ok) return;
        const j = JSON.parse(res.text) as {
          recommendations?: Array<{
            provider: string;
            model: string;
            price_input_per_million?: number | null;
            price_output_per_million?: number | null;
          }>;
          suggestions?: Array<{
            provider: string;
            model: string;
            price_input_per_million?: number | null;
            price_output_per_million?: number | null;
          }>;
          huggingface_local?: Array<{
            provider: string;
            model: string;
            price_input_per_million?: number | null;
            price_output_per_million?: number | null;
          }>;
        };
        const items = [...(j.recommendations ?? []), ...(j.suggestions ?? []), ...(j.huggingface_local ?? [])];
        if (!cancelled) setModelPricingLookup(buildCookbookPricingLookup(items));
      } catch {
        /* optional pricing data */
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [fetchSystemEndpoint]);

  useEffect(() => {
    if (tab !== "memory") return;
    let cancelled = false;
    void (async () => {
      try {
        const res = await fetchSystemEndpoint("/api/memory/recall-metrics");
        if (cancelled || !res.ok) return;
        const j = JSON.parse(res.text) as { hygiene_last_suggestions?: number };
        const n = j.hygiene_last_suggestions ?? 0;
        if (n > 0) {
          setMemoryHygieneHint(
            locale === "en"
              ? `Hygiene scan: ${n} possible duplicate cluster(s). Review long-term memory or run relation rebuild.`
              : `Hygiène mémoire : ${n} groupe(s) de doublons possibles. Vérifiez la mémoire long terme ou reconstruisez les relations.`,
          );
        } else {
          setMemoryHygieneHint(null);
        }
      } catch {
        if (!cancelled) setMemoryHygieneHint(null);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [tab, fetchSystemEndpoint, locale]);

  useEffect(() => {
    if (tab !== "memory") return;
    let cancelled = false;
    void (async () => {
      try {
        const res = await fetchSystemEndpoint("/api/memory/advanced-settings");
        if (cancelled || !res.ok) return;
        const j = JSON.parse(res.text) as { settings?: MemoryAdvancedSettings };
        if (j.settings) setMemoryAdvancedSettings(j.settings);
      } catch {
        if (!cancelled) setMemoryAdvancedSettings(null);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [tab, fetchSystemEndpoint]);

  const saveMemoryAdvancedSettings = useCallback(async () => {
    if (!memoryAdvancedSettings) return;
    setMemoryAdvancedSaving(true);
    setMemoryAdvancedMessage(null);
    try {
      const res = await fetchSystemEndpoint(
        "/api/memory/advanced-settings",
        {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(memoryAdvancedSettings),
        },
      );
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const j = JSON.parse(res.text) as { settings?: MemoryAdvancedSettings };
      if (j.settings) setMemoryAdvancedSettings(j.settings);
      setMemoryAdvancedMessage(locale === "en" ? "Advanced memory settings saved." : "Paramètres mémoire avancés enregistrés.");
    } catch (e) {
      setMemoryAdvancedMessage(String(e));
    } finally {
      setMemoryAdvancedSaving(false);
    }
  }, [memoryAdvancedSettings, fetchSystemEndpoint, locale]);

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

  const fetchProjectWorkspaces = useCallback(async (opts?: { clearError?: boolean }) => {
    setProjectWorkspacesLoading(true);
    if (opts?.clearError !== false) setProjectGraphError(null);
    try {
      const raw = await invoke<{
        workspaces?: ProjectWorkspaceRow[];
        total_node_count?: number;
        total_edge_count?: number;
      }>("list_project_workspaces", { port: DAEMON_PORT });
      const list = Array.isArray(raw?.workspaces) ? raw.workspaces : [];
      setProjectWorkspaces(
        list.map((w) => ({
          id: String(w.id ?? ""),
          name: String(w.name ?? ""),
          root_path: String(w.root_path ?? ""),
          created_at: typeof w.created_at === "string" ? w.created_at : undefined,
          node_count: typeof w.node_count === "number" ? w.node_count : undefined,
          edge_count: typeof w.edge_count === "number" ? w.edge_count : undefined,
          built_at: w.built_at === null || typeof w.built_at === "string" ? w.built_at : undefined,
          file_count: typeof w.file_count === "number" ? w.file_count : undefined,
          indexed_root: w.indexed_root === null || typeof w.indexed_root === "string" ? w.indexed_root : undefined,
        })),
      );
    } catch (e) {
      setProjectGraphError(String(e));
      setProjectWorkspaces([]);
    } finally {
      setProjectWorkspacesLoading(false);
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
        formality?: string | null;
        avatar?: string | null;
        rules?: string[];
        can_do?: string[];
        cannot_do?: string[];
        traits_override?: Record<string, number> | null;
        preferred_mode?: string | null;
        temperature?: number | null;
        system_prompt?: string | null;
      }>("get_agent_profile", { port: DAEMON_PORT });
      const to = data?.traits_override;
      const f = data?.formality;
      setAgentProfile({
        name: data?.name ?? "",
        personality: data?.personality ?? "",
        role: data?.role ?? "",
        gender: data?.gender ?? "",
        formality: f === "formal" || f === "informal" ? f : "",
        avatar: data?.avatar ?? "",
        rules: Array.isArray(data?.rules) ? data.rules : [],
        can_do: Array.isArray(data?.can_do) ? data.can_do : [],
        cannot_do: Array.isArray(data?.cannot_do) ? data.cannot_do : [],
        traits_override: to && typeof to === "object" ? { ...to } : {},
        preferred_mode: data?.preferred_mode ?? "",
        temperature: typeof data?.temperature === "number" ? String(data.temperature) : "",
        system_prompt: data?.system_prompt ?? "",
      });
      try {
        const idRes = await requestSystemEndpoint("GET", "/api/agent-identity");
        if (idRes.ok) {
          const parsed = JSON.parse(idRes.text) as {
            identity?: {
              name?: string;
              role?: string;
              tone?: string;
              values?: string[];
              constraints?: string[];
            };
          };
          const id = parsed.identity ?? {};
          setAgentConstitution({
            tone: id.tone ?? "",
            values: Array.isArray(id.values) ? id.values : [],
            constraints: Array.isArray(id.constraints) ? id.constraints : [],
          });
          if (id.name && !data?.name) {
            setAgentProfile((p) => ({ ...p, name: id.name ?? p.name }));
          }
          if (id.role && !data?.role) {
            setAgentProfile((p) => ({ ...p, role: id.role ?? p.role }));
          }
        }
      } catch {
        /* constitution optional */
      }
    } catch (e) {
      setAgentProfileError(String(e));
    } finally {
      setAgentProfileLoading(false);
    }
  }, [requestSystemEndpoint]);

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
    if (tab === "settings" && settingsSection === "system" && systemSubTab === "plugins") {
      fetchPluginStatus();
    }
  }, [tab, settingsSection, systemSubTab, fetchPluginStatus]);

  useEffect(() => {
    if (tab === "settings" && settingsSection === "data" && dataSourcesSubTab === "project_graph") {
      void fetchProjectWorkspaces();
    }
  }, [tab, settingsSection, dataSourcesSubTab, fetchProjectWorkspaces]);

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
    if (!eventVisualFullscreen) return;
    const onKey = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null;
      if (target?.closest("input, textarea, [contenteditable='true']")) return;

      if (e.key === "Escape") {
        e.preventDefault();
        setEventVisualFullscreen(null);
        return;
      }

      const isZoomIn = e.key === "+" || e.key === "=" || e.key === "Add";
      const isZoomOut = e.key === "-" || e.key === "Subtract";
      const isReset = e.key === "0" || e.key === "Numpad0";
      const isHelpToggle = e.key === "?" || (e.key === "/" && e.shiftKey);

      if (isZoomIn) {
        e.preventDefault();
        setEventVisualTransform((prev) => ({ ...prev, scale: Math.max(0.5, Math.min(8, prev.scale * 1.15)) }));
        return;
      }

      if (isZoomOut) {
        e.preventDefault();
        setEventVisualTransform((prev) => ({ ...prev, scale: Math.max(0.5, Math.min(8, prev.scale / 1.15)) }));
        return;
      }

      if (isReset) {
        e.preventDefault();
        resetEventVisualViewport();
        return;
      }

      if (isHelpToggle) {
        e.preventDefault();
        setEventVisualHelpOpen((prev) => !prev);
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [eventVisualFullscreen, resetEventVisualViewport]);

  useEffect(() => {
    if (!eventVisualFullscreen) return;
    resetEventVisualViewport();
  }, [eventVisualFullscreen, resetEventVisualViewport]);

  useEffect(() => {
    if (!eventVisualFullscreen) {
      setEventVisualHelpOpen(false);
    }
  }, [eventVisualFullscreen]);

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
/reload           — recharger plugins, tools_policy.yaml et llm_router.yaml (modèles)
/skills            — liste des skills installés
/skills list       — idem
/skills install <url> — installer un skill depuis une URL (GitHub ou hôte autorisé)
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
        await invoke<{ cancelled?: boolean }>("cancel_task", { taskId, port: DAEMON_PORT });
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
      const list = await invoke<Array<{ id?: string; name?: string; version?: string; enabled?: boolean; disabled_reason?: string | null; score?: number }>>("get_plugins", { port });
      if (!list?.length) return "Aucun plugin installé.";
      return list
        .map((p) => {
          const enabled = p.enabled !== false;
          const disabledReason = typeof p.disabled_reason === "string" ? p.disabled_reason : null;
          const status = enabled
            ? "enabled"
            : disabledReason === "reputation"
              ? "disabled (reputation)"
              : "disabled";
          const score = typeof p.score === "number" ? ` score=${p.score}` : "";
          return `${p.id ?? "?"} — ${p.name ?? "?"} (${p.version ?? "?"}) [${status}${score}]`;
        })
        .join("\n");
    }
    if (cmd === "reload") {
      const parts: string[] = [];
      try {
        await invoke("reload_plugins", { port });
        parts.push("Plugins rechargés.");
      } catch (e) {
        parts.push(`Plugins : erreur (${String(e)}).`);
      }
      try {
        const json = await invoke<{ reloaded?: boolean }>("reload_tools_policy", { port });
        parts.push(json?.reloaded ? "tools_policy.yaml rechargé." : "tools_policy : erreur.");
      } catch (e) {
        parts.push(`tools_policy : erreur (${String(e)}).`);
      }
      try {
        const json = await invoke<{ reloaded?: boolean }>("reload_router", { port });
        parts.push(json?.reloaded ? "llm_router.yaml rechargé (modèles/routes)." : "llm_router : erreur.");
      } catch (e) {
        parts.push(`llm_router : erreur (${String(e)}).`);
      }
      return parts.join(" ");
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
      if (sub === "install") {
        const skillUrl = parts[2]?.trim();
        if (!skillUrl) return "Usage: /skills install <url> (ex. /skills install https://github.com/BankrBot/skills/tree/main/bankr)";
        try {
          const json = await invoke<{ installed?: boolean; message?: string }>("install_skill", {
            url: skillUrl,
            port,
          });
          return json?.installed ? (json?.message ?? `Skill installé depuis ${skillUrl}.`) : (json?.message ?? `Échec de l'installation du skill depuis ${skillUrl}.`);
        } catch (err) {
          return `Impossible d'installer le skill : ${String(err)}`;
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
      return "Usage: /skills [list] — lister les skills ; /skills install <url> — installer ; /skills reload — recharger ; /skills uninstall <nom> — désinstaller.";
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

  const pollTaskDeps = useMemo(
    (): PollTaskUntilDoneDeps => ({
      daemonPort: DAEMON_PORT,
      sessionId: sessionId ?? "",
      sessionIdRef: sessionIdRef as unknown as MutableRefObject<string>,
      taskIdToSessionIdRef,
      ackTextByTaskRef,
      chatMapByTaskIdRef,
      humanInputAutoOpenedRef,
      replyWithTtsRef,
      selectedTaskIdForTodosRef,
      fetchTasksEventsRef,
      chatInputRef: chatInputRef as unknown as MutableRefObject<HTMLTextAreaElement | null>,
      akashaSessionIdKey: AKASHA_SESSION_ID_KEY,
      normalizeTaskEventsInvokeResponse,
      extractChatMapVisualFromTaskEvents,
      extractChatMapVisualFromAssistantText,
      findLastChatAssistantIndex: findLastChatAssistantIndex as PollTaskUntilDoneDeps["findLastChatAssistantIndex"],
      chatMapMessageCacheKey,
      applyChatStreamProgress,
      fetchTasksList,
      enrichUsageWithPricing,
      setRunningTaskChips,
      setRunningTaskEvents,
      setTasksEvents,
      setPendingHumanInput,
      setHumanInputModalTaskId,
      setChatMapByTaskId,
      setMessages: setMessages as PollTaskUntilDoneDeps["setMessages"],
      voiceTtsConfigured: !!voiceStatus?.tts_configured,
    }),
    [
      sessionId,
      applyChatStreamProgress,
      fetchTasksList,
      enrichUsageWithPricing,
      voiceStatus?.tts_configured,
    ],
  );

  const trackTaskUntilDone = useCallback(
    (taskId: string) => {
      void pollTaskUntilDone(taskId, pollTaskDeps);
    },
    [pollTaskDeps],
  );

  useEffect(() => {
    trackTaskUntilDoneRef.current = trackTaskUntilDone;
  }, [trackTaskUntilDone]);

  const resumedActiveTasksRef = useRef<Set<string>>(new Set());

  useEffect(() => {
    Object.assign(taskIdToSessionIdRef.current, loadTaskSessionMap());
  }, []);

  useEffect(() => {
    if (!health?.ok || !sessionId?.trim()) return;
    Object.assign(taskIdToSessionIdRef.current, loadTaskSessionMap());
    if (tasksList.length === 0) {
      void fetchTasksList({ silent: true });
      return;
    }
    for (const task of tasksList) {
      if (!isTaskActiveStatus(task.status)) continue;
      if (taskIdToSessionIdRef.current[task.id] !== sessionId) continue;
      if (resumedActiveTasksRef.current.has(task.id)) continue;
      resumedActiveTasksRef.current.add(task.id);
      setRunningTaskChips((prev) =>
        prev[task.id] !== undefined ? prev : { ...prev, [task.id]: { pct: 5, message: "en cours…" } },
      );
      setMessages((prev) => {
        if (prev.some((m) => m.taskId === task.id)) return prev;
        const label = task.label?.trim();
        const userText = label && !isGenericTaskLabel(label, task.id) ? label : null;
        const next = [...prev];
        if (userText && !prev.some((m) => m.role === "user" && m.text === userText)) {
          next.push({ role: "user", text: userText });
        }
        next.push({
          role: "assistant",
          text: "Reprise de la tâche en cours…",
          taskId: task.id,
          streaming: true,
        });
        return next;
      });
      trackTaskUntilDone(task.id);
    }
  }, [health?.ok, sessionId, tasksList, trackTaskUntilDone, fetchTasksList]);

  const handleSend = async (overrideMessage?: string, fromVoice?: boolean) => {
    const content = (overrideMessage ?? message).trim();
    const hasContent = content || attachments.length > 0;
    if (!hasContent || loading) return;

    if (fromVoice) replyWithTtsRef.current = true;
    const userMessage = content || "(Pièce(s) jointe(s))";
    const researchCtx = chatResearchContext;
    const noteCtx = chatNoteContext;
    let messageToSend = userMessage;
    if (researchCtx) {
      messageToSend = buildMessageWithResearchContext(userMessage, researchCtx, locale);
    } else if (noteCtx) {
      messageToSend = buildMessageWithNoteContext(userMessage, noteCtx, locale);
    }
    if (researchCtx) setChatResearchContext(null);
    if (noteCtx) setChatNoteContext(null);
    setMessages((prev) => {
      const cleaned = prev.filter((m) => !(m.role === "assistant" && m.streaming));
      return [...cleaned, { role: "user", text: userMessage }];
    });
    if (sessionId) {
      setChatThreads((prev) =>
        prev.map((th) =>
          th.id === sessionId
            ? { ...th, lastSnippet: userMessage.slice(0, 140), updatedAt: new Date().toISOString() }
            : th,
        ),
      );
    }
    if (overrideMessage === undefined) setMessage("");
    chatInputRef.current?.focus();

    if (userMessage.startsWith("/")) {
      const cmdLower = userMessage.replace(/^\//, "").trim().toLowerCase().split(/\s+/)[0] ?? "";
      if (cmdLower === "newsession" || cmdLower === "nouvelle" || (cmdLower === "session" && userMessage.toLowerCase().includes("nouvelle"))) {
        pendingNewSessionAfterSlashRef.current = true;
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
      const useNewSession = pendingNewSessionAfterSlashRef.current;
      if (useNewSession) pendingNewSessionAfterSlashRef.current = false;
      const sessionAtSend = sessionId;
      const runningIds = Object.keys(runningTaskChips);
      const steerTarget =
        chatDeliveryMode !== "immediate" && runningIds.length > 0 ? runningIds[0] : undefined;
      const ack = await invoke<{
        task_id: string;
        session_id: string;
        message: string;
        queued?: boolean;
      }>("send_message_ack", {
        message: messageToSend,
        sessionId: sessionId,
        attachments: attachmentsPayload,
        newSession: useNewSession ? true : undefined,
        queueMode: steerTarget ? chatDeliveryMode : undefined,
        targetTaskId: steerTarget,
        incognito: chatIncognito ? true : undefined,
        port: DAEMON_PORT,
      });
      if (ack?.queued) {
        setMessages((prev) => [
          ...prev,
          {
            role: "system",
            text:
              chatDeliveryMode === "steering"
                ? "Message mis en file steering (injection prioritaire)."
                : "Message mis en file follow-up (après le tour en cours).",
          },
        ]);
      }
      setLoading(false);
      if (ack?.session_id) {
        setSessionId(ack.session_id);
        sessionIdRef.current = ack.session_id;
        try {
          localStorage.setItem(AKASHA_SESSION_ID_KEY, ack.session_id);
        } catch {
          /* ignore */
        }
      }
      if (useNewSession && ack?.session_id) {
        const sid = ack.session_id;
        setChatThreads((prev) => {
          if (prev.some((x) => x.id === sid)) {
            return prev.map((x) => (x.id === sid ? { ...x, pendingTitle: true } : x));
          }
          const now = new Date().toISOString();
          return [{ id: sid, title: "", createdAt: now, updatedAt: now, pendingTitle: true }, ...prev];
        });
      }
      if (ack?.session_id && ack.task_id) {
        const sidT = ack.session_id;
        setChatThreads((prev) => {
          const now = new Date().toISOString();
          const th = prev.find((x) => x.id === sidT);
          if (th?.pendingTitle) {
            void (async () => {
              try {
                const r = await invoke<{ title?: string }>("suggest_thread_title", {
                  message: userMessage,
                  port: DAEMON_PORT,
                });
                const title = (r?.title ?? "").trim();
                if (!title) return;
                setChatThreads((p) =>
                  p.map((x) =>
                    x.id === sidT ? { ...x, title, pendingTitle: false, updatedAt: new Date().toISOString() } : x,
                  ),
                );
              } catch {
                /* ignore */
              }
            })();
          }
          return prev.map((x) => (x.id === sidT ? { ...x, updatedAt: now } : x));
        });
      }
      const ackText = ack?.message ?? "Request received. You can follow progress in the Tasks tab.";
      if (ack?.task_id) {
        // #region agent log
        fetch("http://127.0.0.1:7708/ingest/83a7f7de-74a3-4ba3-8a97-b0169801051e", {
          method: "POST",
          headers: { "Content-Type": "application/json", "X-Debug-Session-Id": "0d82aa" },
          body: JSON.stringify({
            sessionId: "0d82aa",
            location: "App.tsx:handleSend",
            message: "message_ack",
            hypothesisId: "B",
            data: {
              taskId: ack.task_id,
              sessionId: ack.session_id,
              queued: !!ack.queued,
              steerTarget: steerTarget ?? null,
              runningTaskIds: runningIds,
              chatDeliveryMode,
              sessionAtSend,
            },
            timestamp: Date.now(),
          }),
        }).catch(() => {});
        // #endregion
        const sidResolved = (ack.session_id || sessionAtSend || "").trim();
        if (sidResolved) {
          taskIdToSessionIdRef.current[ack.task_id] = sidResolved;
          persistTaskSession(ack.task_id, sidResolved);
        }
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
        setCollapsedRootTasks((prev) => ({ ...prev, [ack.task_id]: false }));
        setSubAgentPanelCollapsed(false);
        void fetchTasksList({ silent: true });
        trackTaskUntilDone(ack.task_id);
      }
    } catch (err) {
      setLoading(false);
      setMessages((prev) => [...prev, { role: "assistant", text: `Erreur : ${String(err)}`, error: true }]);
    }
    chatInputRef.current?.focus();
  };
  handleSendRef.current = handleSend;

  return (
    <div className={`app ui-mode-${uiMode}${mobileSidebarOpen ? " app--sidebar-open" : ""}`}>
      <AppNotificationsSync
        routerError={routerError}
        docError={docError}
        memoryError={memoryError}
        missionError={missionError}
        calendarTaskDetailError={calendarTaskDetailError}
        scheduleDetailError={scheduleDetailError}
        pluginStatusError={pluginStatusError}
        pluginReputationResetError={pluginReputationResetError}
        agentProfileError={agentProfileError}
        userProfileError={userProfileError}
        userRagError={userRagError}
        projectGraphError={projectGraphError}
      />
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
        {mobileSidebarOpen ? (
          <button
            type="button"
            className="sidebar-mobile-backdrop"
            aria-label={t("common.close")}
            onClick={() => setMobileSidebarOpen(false)}
          />
        ) : null}
        <aside className={`sidebar-left${mobileSidebarOpen ? " sidebar-left--open" : ""}${sidebarNavCollapsed ? " sidebar-left--collapsed" : ""}`} aria-label="Navigation principale">
          <div className="sidebar-left-top">
            <h1 className="logo">Akasha</h1>
            <p className="tagline">Local-first AI assistant</p>
          </div>
          <AppNavigation tab={tab} setTab={(t) => { setTab(t); setMobileSidebarOpen(false); }} t={t} collapsed={sidebarNavCollapsed} onToggleCollapse={() => setSidebarNavCollapsed((c) => !c)} />
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
            <button
              type="button"
              className="btn-secondary sidebar-resume-btn"
              disabled={!health?.ok || resumeBriefBusy || !sessionId?.trim()}
              onClick={() => void handleResumeBrief()}
              title={locale === "en" ? "Fetch session resume brief from daemon" : "Charger le brief de reprise depuis le daemon"}
            >
              {resumeBriefBusy ? "…" : locale === "en" ? "Resume" : "Reprendre"}
            </button>
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
                  <span className="header-pending-actions-icon" aria-hidden>
                    <MonoIcon name="warning" />
                  </span>
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
              <div className="view-header-main">
                <h2 className="view-title">
                  {t("tabs." + tab)}
                  <InfoTip label={t("tabs." + tab)} content={isSimpleMode ? t("settings.ui_mode_hint") : t("chat.follow_tasks")} />
                </h2>
              </div>
              <div className="view-header-actions">
                <button
                  type="button"
                  className="sidebar-mobile-toggle"
                  onClick={() => setMobileSidebarOpen((o) => !o)}
                  aria-expanded={mobileSidebarOpen}
                  aria-label={t("nav.toggle_sidebar")}
                >
                  ☰
                </button>
                <NotificationCenter />
                <PermissionsBell fetchEndpoint={fetchSystemEndpoint} locale={locale} />
                <SteeringQueueBell
                  taskId={activeSteeringTaskId}
                  fetchEndpoint={fetchSystemEndpoint}
                  locale={locale}
                />
                <Tooltip content={uiMode === "simple" ? t("settings.ui_mode_simple") : t("settings.ui_mode_expert")}>
                  <span className="view-mode-badge">{uiMode === "simple" ? t("settings.ui_mode_simple") : t("settings.ui_mode_expert")}</span>
                </Tooltip>
                <Tooltip content={health?.ok ? t("status.daemon_ok") : t("status.daemon_off")}>
                  <span
                    className={`daemon-status ${health?.ok ? "daemon-status-ok" : "daemon-status-off"}`}
                    role="status"
                    aria-live="polite"
                  >
                    {health?.ok ? t("status.daemon_ok") : t("status.daemon_off")}
                  </span>
                </Tooltip>
                {tab !== "tasks" && (
                  <Tooltip content={rightSidebarOpen ? t("sidebar.hide_tasks") : t("sidebar.show_tasks")}>
                    <button
                      type="button"
                      className="sidebar-right-toggle"
                      onClick={() => setRightSidebarOpen((o) => !o)}
                      aria-expanded={rightSidebarOpen}
                      aria-label={rightSidebarOpen ? t("sidebar.hide_tasks") : t("sidebar.show_tasks")}
                    >
                      {rightSidebarOpen ? "▐" : "▌"}
                    </button>
                  </Tooltip>
                )}
              </div>
            </header>
      <main className="main" id="main-content" tabIndex={-1}>
        {/* Onboarding: first steps modal (dismissible, "Ne plus afficher" stored in localStorage) */}
        {showOnboarding && (
          <OnboardingWizard
            locale={locale}
            daemonOk={!!health?.ok}
            onComplete={() => setShowOnboarding(false)}
            t={t}
          />
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
                          setHumanInputFreeText("");
                        } catch (e) {
                          console.error(e);
                        }
                      }}
                    >
                      {choice}
                    </button>
                  ))}
                </div>
              ) : null}
              <div className="human-input-free">
                {pendingHumanInput[humanInputModalTaskId].choices?.length ? (
                  <label htmlFor="human-input-free-textarea">{t("human_input.or_custom")}</label>
                ) : null}
                <textarea
                  id="human-input-free-textarea"
                  rows={3}
                  value={humanInputFreeText}
                  onChange={(e) => setHumanInputFreeText(e.target.value)}
                  placeholder={t("human_input.placeholder")}
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
              <button type="button" className="human-input-close" onClick={() => setHumanInputModalTaskId(null)} aria-label={t("common.close")}>
                ×
              </button>
            </div>
          </div>
        )}
        {handoffDialogOpen && (
          <div className="human-input-overlay" role="dialog" aria-modal="true">
            <div className="human-input-modal">
              <h2>{locale === "en" ? "Resume with another model" : "Reprendre avec un autre modèle"}</h2>
              <label>
                {locale === "en" ? "Provider" : "Provider"}
                <input
                  className="settings-input"
                  value={handoffTargetProvider}
                  onChange={(e) => setHandoffTargetProvider(e.target.value)}
                  placeholder={locale === "en" ? "optional" : "optionnel"}
                />
              </label>
              <label>
                {locale === "en" ? "Model" : "Modèle"}
                <input
                  className="settings-input"
                  list="handoff-models"
                  value={handoffTargetModel}
                  onChange={(e) => setHandoffTargetModel(e.target.value)}
                  placeholder="provider/model"
                />
                <datalist id="handoff-models">
                  {handoffModels.map((row) => (
                    <option key={`${row.provider}:${row.model}`} value={row.model}>
                      {row.provider}
                    </option>
                  ))}
                </datalist>
              </label>
              <label>
                task_id ({locale === "en" ? "optional" : "optionnel"})
                <input
                  className="settings-input"
                  value={handoffTaskId}
                  onChange={(e) => setHandoffTaskId(e.target.value)}
                />
              </label>
              <div className="onboarding-actions">
                <button type="button" className="btn-secondary" onClick={() => setHandoffDialogOpen(false)}>
                  {locale === "en" ? "Close" : "Fermer"}
                </button>
                <button type="button" className="btn-primary" disabled={handoffBusy} onClick={() => void submitSessionHandoff()}>
                  {handoffBusy ? "…" : locale === "en" ? "Send handoff" : "Envoyer le handoff"}
                </button>
              </div>
              {handoffStatus ? <p className="muted">{handoffStatus}</p> : null}
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
                          const url = e2eDaemonHttpUrl("/api/device/result");
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
            className="panel chat-panel chat-panel-centered"
          >
            {chatIncognito ? (
              <p className="chat-incognito-banner" role="status">
                {locale === "en" ? "Incognito — memory promotion disabled for this session (UI flag)." : "Incognito — promotion mémoire désactivée pour cette session (indicateur UI)."}
              </p>
            ) : null}
            {(companionBubbleText || messages.length > 0 || Object.keys(runningTaskChips).length > 0) && (
              <div className="chat-panel-top">
                <div className="chat-panel-toolbar">
                  {companionBubbleText ? (
                    <button
                      type="button"
                      className="chat-companion-toggle"
                      onClick={() => {
                        setCompanionOpen((open) => {
                          const next = !open;
                          try {
                            localStorage.setItem(CHAT_COMPANION_OPEN_KEY, next ? "1" : "0");
                          } catch {
                            /* ignore */
                          }
                          return next;
                        });
                      }}
                      aria-expanded={companionOpen}
                      aria-controls="chat-companion-message"
                      aria-label={t("chat.companion_label")}
                      title={t("chat.companion_label")}
                    >
                      <img src="/akasha-icon.png" alt="" className="chat-companion-icon" width={20} height={20} />
                    </button>
                  ) : null}
                  <div className="chat-panel-toolbar-actions">
                    <button
                      type="button"
                      className="btn-secondary chat-toolbar-btn"
                      onClick={() => {
                        setHandoffDialogOpen(true);
                        setHandoffStatus(null);
                        void loadHandoffModels();
                      }}
                    >
                      {locale === "en" ? "Resume with another model" : "Reprendre avec un autre modèle"}
                    </button>
                    {messages.length > 0 && (
                      <button type="button" className="btn-secondary chat-toolbar-btn" onClick={exportChatTranscript}>
                        {t("chat.export_transcript")}
                      </button>
                    )}
                    {Object.keys(runningTaskChips).length > 0 && (
                      <button type="button" className="btn-secondary chat-toolbar-btn" onClick={() => setTab("tasks")}>
                        {t("chat.active_tasks").replace("{{count}}", String(Object.keys(runningTaskChips).length))}
                      </button>
                    )}
                  </div>
                </div>
                {companionOpen && companionBubbleText ? (
                  <div className="chat-companion-row chat-companion-row--open" role="status" aria-live="polite">
                    <div id="chat-companion-message" className="chat-companion-bubble">
                      <span className="chat-companion-label">{t("chat.companion_label")}</span>
                      <span className="chat-companion-text">{companionBubbleText}</span>
                    </div>
                    {chatTipsEnabled && tipBannerText && !tipBannerDismissed ? (
                      <button
                        type="button"
                        className="chat-companion-dismiss"
                        onClick={() => {
                          setTipBannerDismissed(true);
                          setTipBannerText(null);
                        }}
                        aria-label={t("chat.tip_dismiss")}
                      >
                        ×
                      </button>
                    ) : null}
                  </div>
                ) : null}
              </div>
            )}
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
                <ChatRenderer
                  messages={messages}
                  parseAskUser={parseAskUserMessage}
                  onPathClick={handlePathClick}
                  userAvatar={userAvatar}
                  agentAvatar={agentProfile.avatar}
                  agentName={agentProfile.name || "Akasha"}
                  onOpenTaskDetail={openChatTaskDetail}
                  taskDetailLabel={t("chat.view_task_detail")}
                  renderAskUserChoice={(choice, j) => {
                    const pendingTaskIdForReply = Object.keys(pendingHumanInput)[0] ?? null;
                    return pendingTaskIdForReply ? (
                      <button
                        key={j}
                        type="button"
                        className="message-ask-user-choice-tag"
                        onClick={async () => {
                          try {
                            await invoke("post_task_human_reply", { taskId: pendingTaskIdForReply, response: choice, port: DAEMON_PORT });
                            setPendingHumanInput((prev) => {
                              const next = { ...prev };
                              delete next[pendingTaskIdForReply];
                              return next;
                            });
                            setHumanInputModalTaskId((c) => (c === pendingTaskIdForReply ? null : c));
                          } catch (e) {
                            console.error(e);
                          }
                        }}
                      >
                        {choice}
                      </button>
                    ) : (
                      <span key={j} className="message-ask-user-choice-tag">
                        {choice}
                      </span>
                    );
                  }}
                  renderMapVisual={(m) => {
                    const assistantMapVisual =
                      m.role === "assistant"
                        ? ((m.mapVisual ?? (m.taskId ? chatMapByTaskId[m.taskId] : undefined)) as EventAdvancedView | undefined)
                        : undefined;
                    if (!assistantMapVisual || assistantMapVisual.kind !== "map") return null;
                    return (
                      <div className="chat-message-map-embed">
                        <MapPluginEventView
                          visual={assistantMapVisual}
                          t={t}
                          width={460}
                          height={160}
                          toolbar="inline"
                          variant="chat"
                          showPanelHeading={false}
                          onFullscreen={() => setEventVisualFullscreen({ visual: assistantMapVisual, sourceEventType: "chat_map" })}
                          onExportCsv={() => exportAdvancedViewCsv(assistantMapVisual)}
                        />
                      </div>
                    );
                  }}
                />
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
                          : trimPreview(message ?? "en cours", isSimpleMode ? 72 : 140);
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
              {isSimpleMode && Object.keys(runningTaskChips).length > 0 && (
                <div className="chat-task-summary-bar">
                  <span>{t("chat.active_tasks").replace("{{count}}", String(Object.keys(runningTaskChips).length))}</span>
                  <button type="button" className="chat-task-summary-btn" onClick={() => setTab("tasks")}>{t("chat.open_tasks")}</button>
                </div>
              )}
              {!isSimpleMode && Object.keys(runningTaskChips).length > 0 && chatToolBatchSummary && (
                <div className="chat-tool-batch-summary" role="status">
                  {chatToolBatchSummary}
                </div>
              )}
              <div ref={chatEndRef} aria-hidden />
            </div>
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
                    <span className="chat-subagents-toggle-text">
                      {subAgentPanelCollapsed
                        ? (() => {
                            const total = Object.values(runningTaskEvents).flat().length;
                            return total > 0
                              ? t("chat.agent_activity_expand").replace("{{count}}", String(total))
                              : t("chat.agent_activity_expand_hint");
                          })()
                        : t("chat.agent_activity_collapse")}
                    </span>
                  </button>
                  {!subAgentPanelCollapsed && (
                    <div ref={subagentsDetailRef} id="subagents-detail" className="chat-subagents-detail" role="region" aria-label={t("chat.agent_activity_region")}>
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
                                  {t("chat.agent_discussion_heading").replace("{{id}}", rootTaskId.slice(-8))}
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
                                                  {s.agent_type && <span className="chat-subagents-plan-agent agent-kind-pill" data-agent-kind={classifyAgentKind(s.agent_type)}>{s.agent_type}</span>}
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
                                          {collapseStreamedProgressEvents(evs, tid).map((ev, idx) => (
                                            <li key={`${tid}-${idx}`} className="chat-subagents-event" data-type={ev.event_type} data-event-kind={classifyEventKind(ev.event_type)}>
                                              <span className="chat-subagents-event-dot" aria-hidden />
                                              <div className="chat-subagents-event-body">
                                                {(() => {
                                                  const summary = summarizeTaskEvent(ev);
                                                  return summary ? (
                                                    <p className="chat-subagents-event-summary">{summary}</p>
                                                  ) : null;
                                                })()}
                                                <div className="chat-subagents-event-topline">
                                                  <span className="chat-subagents-event-type event-kind-pill" data-event-kind={classifyEventKind(ev.event_type)}>{eventTypeBadgeLabel(ev.event_type)}</span>
                                                  {isDeterministicAutoToolEvent(ev.event_type) && (
                                                    <span className="event-auto-tool-badge">{t("tasks.auto_tool_badge")}</span>
                                                  )}
                                                  {ev.at && <span className="chat-subagents-event-at">{ev.at.slice(0, 19)}</span>}
                                                </div>
                                                <div className="chat-subagents-event-meta">
                                                  {ev.payload && typeof ev.payload === "object" && "agent" in ev.payload ? (
                                                    <span className="chat-subagents-event-agent">→ <span className="agent-kind-pill" data-agent-kind={classifyAgentKind(String((ev.payload as { agent?: string }).agent ?? ""))}>{String((ev.payload as { agent?: string }).agent ?? "")}</span></span>
                                                  ) : null}
                                                  {ev.payload && typeof ev.payload === "object" && (ev.event_type === "tool_call_started" || ev.event_type === "tool_call_finished") && "tool" in ev.payload ? (
                                                    <span className="chat-subagents-event-agent">— {String((ev.payload as { tool?: string }).tool ?? "")}</span>
                                                  ) : null}
                                                  {ev.payload && typeof ev.payload === "object" && ev.event_type === "tool_invoked" ? (() => {
                                                    const p = ev.payload as Record<string, unknown>;
                                                    const tool = typeof p.tool === "string" ? p.tool : null;
                                                    const success = typeof p.success === "boolean" ? p.success : null;
                                                    const preview = typeof p.result_preview === "string" && p.result_preview.trim() ? p.result_preview.trim() : null;
                                                    return (
                                                      <>
                                                        {tool && <span className="chat-subagents-event-agent">— {tool}{success !== null && <span className={`event-tool-result-badge ${success ? "event-tool-result-ok" : "event-tool-result-err"}`}>{success ? "✓" : "✗"}</span>}</span>}
                                                        {preview && <span className="chat-subagents-event-result-preview" title={p.result_preview as string}>{trimPreview(preview, 120)}</span>}
                                                      </>
                                                    );
                                                  })() : null}
                                                  {ev.payload && typeof ev.payload === "object" && (ev.event_type === "task_completed" || ev.event_type === "task_failed") ? (
                                                    (() => {
                                                      const usage = enrichUsageWithPricing(parseUsageFromEventPayload(ev.payload));
                                                      if (usage) {
                                                        return <ModelUsageBadge usage={usage} compact className="chat-subagents-event-usage" />;
                                                      }
                                                      const p = ev.payload as { model_used?: string | null };
                                                      return p.model_used ? (
                                                        <span className="chat-subagents-event-model">— {t("tasks.model_used")}: {p.model_used}</span>
                                                      ) : null;
                                                    })()
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
                                                        <span className="chat-subagents-event-agent">— {parts.join(" · ")}</span>
                                                      ) : null;
                                                    })()
                                                  ) : null}
                                                </div>
                                              </div>
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
                              setInlineHumanReplyText("");
                            } catch (e) {
                              console.error(e);
                            }
                          }}
                        >
                          {choice}
                        </button>
                      ))}
                    </div>
                  ) : null}
                  <div className="chat-inline-human-reply-free">
                    {pending.choices?.length ? (
                      <label htmlFor="inline-human-reply-textarea" className="chat-inline-human-reply-custom-label">
                        {t("human_input.or_custom")}
                      </label>
                    ) : (
                      <label htmlFor="inline-human-reply-textarea" className="sr-only">
                        {t("human_input.placeholder")}
                      </label>
                    )}
                    <div className="chat-inline-human-reply-free-row">
                      <textarea
                        id="inline-human-reply-textarea"
                        rows={3}
                        value={inlineHumanReplyText}
                        onChange={(e) => setInlineHumanReplyText(e.target.value)}
                        placeholder={t("human_input.placeholder")}
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
                        {t("human_input.submit")}
                      </button>
                    </div>
                  </div>
                </div>
              );
            })()}
            {chatResearchContext ? (
              <div className="chat-research-context-banner" role="status">
                <span>
                  {locale === "en"
                    ? `Discussing Deep Research report: ${chatResearchContext.topic}`
                    : `Discussion du rapport Deep Research : ${chatResearchContext.topic}`}
                </span>
                <button
                  type="button"
                  className="chat-research-context-dismiss"
                  onClick={() => setChatResearchContext(null)}
                  aria-label={locale === "en" ? "Clear report context" : "Retirer le contexte du rapport"}
                >
                  ×
                </button>
              </div>
            ) : null}
            {chatNoteContext ? (
              <div className="chat-note-context-banner" role="status">
                <span>
                  {locale === "en"
                    ? `Note context: ${chatNoteContext.title}`
                    : `Contexte note : ${chatNoteContext.title}`}
                </span>
                <button
                  type="button"
                  className="chat-research-context-dismiss"
                  onClick={() => setChatNoteContext(null)}
                  aria-label={locale === "en" ? "Clear note context" : "Retirer le contexte de la note"}
                >
                  ×
                </button>
              </div>
            ) : null}
            {notePickerOpen ? (
              <div className="chat-note-picker" role="dialog" aria-label={t("notes.picker_title")}>
                <div className="chat-note-picker-header">
                  <strong>{t("notes.picker_title")}</strong>
                  <button type="button" className="chat-research-context-dismiss" onClick={() => setNotePickerOpen(false)}>
                    ×
                  </button>
                </div>
                {notePickerLoading ? (
                  <p>{t("common.loading")}</p>
                ) : notePickerItems.length === 0 ? (
                  <p>{t("notes.picker_empty")}</p>
                ) : (
                  <ul className="chat-note-picker-list">
                    {notePickerItems.map((n) => (
                      <li key={n.id}>
                        <button type="button" onClick={() => void attachNoteToChat(n.id)}>
                          {n.title.trim() || t("notes.untitled")}
                        </button>
                      </li>
                    ))}
                  </ul>
                )}
              </div>
            ) : null}
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
            {chatPromptChipsEnabled && (
              <div className="chat-prompt-chips" role="group" aria-label={t("chat.prompt_chips_label")}>
                {promptChipLabels.map((label, i) => (
                  <button
                    key={i}
                    type="button"
                    className="chat-prompt-chip"
                    onClick={() => {
                      setMessage((m) => (m.trim() ? `${m.trim()} ${label}` : label));
                      chatInputRef.current?.focus();
                    }}
                  >
                    {label}
                  </button>
                ))}
              </div>
            )}
            <div className="chat-footer-tools">
            {Object.keys(runningTaskChips).length > 0 && (
              <Tooltip
                content={
                  locale === "en"
                    ? "Steering injects mid-task; follow-up runs after the current turn"
                    : "Steering : injection prioritaire ; follow-up : après le tour en cours"
                }
              >
                <div className="input-group chat-delivery-group" role="group" aria-label={locale === "en" ? "Delivery mode" : "Mode d'envoi"}>
                  <span className="input-group-addon" id="chat-delivery-mode-label">
                    {locale === "en" ? "Delivery" : "Envoi"}
                  </span>
                  <select
                    id="chat-delivery-mode"
                    className="input-group-field chat-delivery-select"
                    value={chatDeliveryMode}
                    onChange={(e) =>
                      setChatDeliveryMode(e.target.value as "immediate" | "steering" | "follow_up")
                    }
                    disabled={loading}
                    aria-labelledby="chat-delivery-mode-label"
                  >
                    <option value="immediate">{locale === "en" ? "Immediate" : "Immédiat"}</option>
                    <option value="steering">Steering</option>
                    <option value="follow_up">Follow-up</option>
                  </select>
                </div>
              </Tooltip>
            )}
            <ChatCompositionBar
              locale={locale}
              agentMode={chatAgentMode}
              onAgentModeChange={(v) => {
                setChatAgentMode(v);
                try {
                  localStorage.setItem(CHAT_AGENT_MODE_KEY, v ? "1" : "0");
                } catch {
                  /* ignore */
                }
              }}
              webSearchEnabled={chatWebSearch}
              onWebSearchChange={(v) => {
                setChatWebSearch(v);
                try {
                  localStorage.setItem(CHAT_WEB_SEARCH_KEY, v ? "1" : "0");
                } catch {
                  /* ignore */
                }
              }}
              incognito={chatIncognito}
              onIncognitoChange={(v) => {
                setChatIncognito(v);
                try {
                  localStorage.setItem(CHAT_INCOGNITO_KEY, v ? "1" : "0");
                } catch {
                  /* ignore */
                }
              }}
              disabled={loading}
            />
            </div>
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
              <div className="input-group" role="group" aria-label={locale === "en" ? "Compose message" : "Composer un message"}>
                <div className="input-group-prepend">
                  <button
                    type="button"
                    className="input-group-btn btn-secondary"
                    onClick={() => fileInputRef.current?.click()}
                    aria-label={locale === "en" ? "Attach file" : "Joindre un fichier"}
                    title={locale === "en" ? "Attach image or document" : "Joindre une image ou un document"}
                  >
                    <MonoIcon name="paperclip" />
                    <span className="input-group-btn-label">{locale === "en" ? "Attach" : "Joindre"}</span>
                  </button>
                  <button
                    type="button"
                    className="input-group-btn btn-secondary"
                    onClick={() => void openNotePicker()}
                    aria-label={t("notes.insert_in_chat")}
                    title={t("notes.insert_in_chat")}
                  >
                    <MonoIcon name="note" />
                    <span className="input-group-btn-label">{t("notes.insert_in_chat_short")}</span>
                  </button>
                  {voiceStatus?.stt_configured ? (
                    <button
                      type="button"
                      className={`input-group-btn btn-secondary chat-voice-btn ${voiceRecording ? "recording" : ""}`}
                      onClick={handleVoiceMessageToggle}
                      disabled={loading}
                      aria-label={
                        voiceRecording
                          ? locale === "en"
                            ? "Stop recording and send"
                            : "Arrêter l'enregistrement et envoyer"
                          : locale === "en"
                            ? "Voice message"
                            : "Message vocal"
                      }
                      title={voiceRecording ? (locale === "en" ? "Stop and send" : "Arrêter et envoyer") : locale === "en" ? "Voice message" : "Message vocal"}
                    >
                      {voiceRecording ? (
                        <span className="chat-voice-btn-inner">● {locale === "en" ? "Recording…" : "Enregistrement…"}</span>
                      ) : (
                        <MonoIcon name="mic" />
                      )}
                    </button>
                  ) : null}
                </div>
                <label htmlFor="chat-input" className="sr-only">
                  {locale === "en" ? "Your message" : "Votre message"}
                </label>
                <input
                  ref={chatInputRef}
                  id="chat-input"
                  type="text"
                  className="input-group-field"
                  value={message}
                  onChange={(e) => setMessage(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key !== "Enter" || e.shiftKey) return;
                    e.preventDefault();
                    if (e.altKey) {
                      setChatDeliveryMode("follow_up");
                    } else if (runningTaskChips && Object.keys(runningTaskChips).length > 0) {
                      setChatDeliveryMode("steering");
                    } else {
                      setChatDeliveryMode("immediate");
                    }
                    void handleSend();
                  }}
                  placeholder={locale === "en" ? "Your message…" : "Votre message…"}
                  disabled={loading}
                  aria-describedby="send-hint"
                />
                <div className="input-group-append">
                  <button
                    type="button"
                    className="input-group-btn btn-primary"
                    onClick={() => handleSend()}
                    disabled={loading || (!message.trim() && attachments.length === 0)}
                    aria-label={locale === "en" ? "Send message" : "Envoyer le message"}
                  >
                    {locale === "en" ? "Send" : "Envoyer"}
                  </button>
                </div>
              </div>
            </div>
            <p id="send-hint" className="hint sr-only">
              {locale === "en"
                ? "Enter to send (steering when a task is running; Alt+Enter for follow-up)"
                : "Entrée pour envoyer (steering si tâche active ; Alt+Entrée pour follow-up)"}
            </p>
          </section>
        )}

        {tab === "compare" && (
          <section id="panel-compare" role="tabpanel" aria-labelledby="tab-compare" className="panel compare-panel-wrap">
            <ComparePanel fetchEndpoint={fetchSystemEndpoint} locale={locale} />
          </section>
        )}

        <section
          id="panel-research"
          role="tabpanel"
          aria-labelledby="tab-research"
          className="panel research-panel"
          hidden={tab !== "research"}
          aria-hidden={tab !== "research"}
        >
          <DeepResearchPanel
            fetchEndpoint={fetchSystemEndpoint}
            locale={locale}
            onDiscussReport={discussResearchReport}
          />
        </section>

        {tab === "cookbook" && (
          <section id="panel-cookbook" role="tabpanel" aria-labelledby="tab-cookbook" className="panel cookbook-panel-wrap">
            <CookbookPanel fetchEndpoint={fetchSystemEndpoint} locale={locale} />
          </section>
        )}

        {tab === "notes" && (
          <section id="panel-notes" role="tabpanel" aria-labelledby="tab-notes" className="panel notes-panel-wrap">
            <NotesPanel t={t} locale={locale} daemonPort={DAEMON_PORT} onDiscussNote={discussNote} />
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
                    {(r.run_ended_at || r.ended_at) && (
                      <time className="scheduled-report-time" dateTime={r.run_ended_at ?? r.ended_at}>
                        {new Date(r.run_ended_at ?? r.ended_at ?? "").toLocaleString()}
                      </time>
                    )}
                    <div className="scheduled-report-metadata">
                      <span><strong>{t("scheduled.task")}:</strong> {r.task_label || r.task_id || t("scheduled.unknown")}</span>
                      <span><strong>{t("scheduled.planned_for")}:</strong> {r.planned_for ? new Date(r.planned_for).toLocaleString() : t("scheduled.unknown")}</span>
                      <span><strong>{t("scheduled.started_at")}:</strong> {r.started_at ? new Date(r.started_at).toLocaleString() : t("scheduled.unknown")}</span>
                      <span><strong>{t("scheduled.ended_at")}:</strong> {(r.run_ended_at || r.ended_at) ? new Date(r.run_ended_at ?? r.ended_at ?? "").toLocaleString() : t("scheduled.unknown")}</span>
                    </div>
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
            {!routerLoading && routerMetrics && (
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
            {!docLoading && docContent && (
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
            <div className="task-center-toolbar">
              <h2 className="panel-title task-center-toolbar-title">{t("tasks.title")}</h2>
              <div className="task-center-toolbar-actions">
                <Tooltip content={rightSidebarOpen ? t("tasks.hide_task_list") : t("tasks.show_task_list")}>
                  <button
                    type="button"
                    className="task-center-toolbar-btn sidebar-right-toggle"
                    onClick={() => setTaskSidebarOpen(!rightSidebarOpen)}
                    aria-expanded={rightSidebarOpen}
                    aria-label={rightSidebarOpen ? t("tasks.hide_task_list") : t("tasks.show_task_list")}
                  >
                    {rightSidebarOpen ? "▐" : "▌"} {rightSidebarOpen ? t("tasks.hide_task_list") : t("tasks.show_task_list")}
                  </button>
                </Tooltip>
                <button type="button" className="task-center-toolbar-btn btn-primary" onClick={() => setCreateTaskDialogOpen(true)}>
                  + {t("tasks.create_button")}
                </button>
                <button
                  type="button"
                  className="task-center-toolbar-btn refresh-btn"
                  onClick={() => void fetchTasksList()}
                  aria-label={t("sidebar.refresh_tasks")}
                  disabled={tasksLoading}
                >
                  {t("sidebar.refresh_tasks")}
                </button>
              </div>
            </div>
            {!tasksLoading && tasksList.length > 0 && (
              <div className="task-center-summary">
                <div className="task-center-summary-chips">
                  <span className="task-center-chip task-center-chip-running">{t("tasks.filter_active")}: {taskStatusCounts.running + taskStatusCounts.pending}</span>
                  <span className="task-center-chip task-center-chip-completed">{t("tasks.filter_completed")}: {taskStatusCounts.completed}</span>
                  {taskStatusCounts.failed > 0 && <span className="task-center-chip task-center-chip-failed">{t("common.error")}: {taskStatusCounts.failed}</span>}
                </div>
                {selectedTask && (
                  <div className="task-center-selected-summary">
                    <div className="task-center-selected-main">
                      <p className="task-center-selected-label">{t("tasks.selected_task_label")}</p>
                      <h3 className="task-center-selected-title">{taskDisplayLabel(selectedTask)}</h3>
                      <div className="task-center-selected-badges">
                        <span className={"task-tree-kind-badge " + (selectedTask.parent_task_id ? "task-tree-kind-badge-child" : "task-tree-kind-badge-root")}>
                          {selectedTask.parent_task_id ? t("tasks.subtask_badge") : t("tasks.root_badge")}
                        </span>
                        {selectedTaskHierarchy.length > 1 && (
                          <span className="task-tree-path-summary">
                            {selectedTaskHierarchy.map((task, index) => (
                              <span key={task.id} className="task-tree-path-segment">
                                {index > 0 && <span className="task-tree-path-separator" aria-hidden>›</span>}
                                <span>{taskDisplayLabel(task)}</span>
                              </span>
                            ))}
                          </span>
                        )}
                      </div>
                      <div className="task-center-selected-meta">
                        <span className={"activity-task-status-pill status-" + selectedTask.status}>{selectedTask.status}</span>
                        {selectedTask.assigned_agent && <span className="agent-kind-pill" data-agent-kind={classifyAgentKind(selectedTask.assigned_agent)}>{selectedTask.assigned_agent}</span>}
                        {selectedTaskSummary?.progressPct != null && selectedTask.status === "running" && (
                          <span className="task-live-progress-pill">
                            {t("tasks.progress_pct_summary").replace("{{pct}}", String(selectedTaskSummary.progressPct))}
                          </span>
                        )}
                        {selectedTask.created_at && <span>{formatRelativeTimeLabel(selectedTask.created_at, locale)}</span>}
                      </div>
                    </div>
                    {selectedTaskSummary?.routeHint ? (
                      <p className="task-center-selected-route">{selectedTaskSummary.routeHint}</p>
                    ) : null}
                    {selectedTaskSummary?.latestSummary ? (
                      <p className="task-center-selected-text">{selectedTaskSummary.latestSummary}</p>
                    ) : selectedTaskSummary?.runningChip?.message ? (
                      <p className="task-center-selected-text">{trimPreview(selectedTaskSummary.runningChip.message, 180)}</p>
                    ) : null}
                  </div>
                )}
              </div>
            )}
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
                      <TaskExecutionSteps
                        steps={taskExecutionView.steps}
                        emptyReason={taskExecutionView.emptyReason}
                        t={t}
                        classifyAgentKind={classifyAgentKind}
                        onSelectChildTask={(childTaskId) => {
                          const idx = tasksList.findIndex((x) => x.id === childTaskId);
                          if (idx >= 0) setTasksSelected(idx);
                        }}
                      />
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
                  <div className="task-events-controls">
                    <label className="task-events-debug-level-control" htmlFor="task-debug-level-select">
                      <span className="task-events-debug-level-label">{t("tasks.debug_orchestration_level_label")}</span>
                      <select
                        id="task-debug-level-select"
                        className="task-events-debug-level-select"
                        value={taskOrchestrationDebugLevel}
                        onChange={(e) => setTaskOrchestrationDebugLevelAndSave(e.target.value as TaskOrchestrationDebugLevel)}
                        aria-label={t("tasks.debug_orchestration_level_label")}
                      >
                        <option value="minimal">{t("tasks.debug_level_minimal")}</option>
                        <option value="normal">{t("tasks.debug_level_normal")}</option>
                        <option value="full">{t("tasks.debug_level_full")}</option>
                      </select>
                    </label>
                  </div>
                  <div className="task-panel-events-scroll">
                    {taskDiscussionHighlights.length > 0 && (
                      <div className="task-discussion-highlight" role="region" aria-label={t("tasks.agent_discussions_title")}>
                        <h4 className="chat-subagents-plan-title">{t("tasks.agent_discussions_title")}</h4>
                        <p className="task-events-hint">
                          {taskOrchestrationDebugLevel === "minimal"
                            ? t("tasks.agent_discussions_hint_minimal")
                            : taskOrchestrationDebugLevel === "normal"
                              ? t("tasks.agent_discussions_hint_normal")
                              : t("tasks.agent_discussions_hint_full")}
                        </p>
                        <ul className="activity-events-list" role="list">
                          {taskDiscussionHighlights.map(({ event, highlights }, i) => (
                            <li key={`discussion-${event.event_type}-${event.at}-${i}`} className="activity-event-card" data-event-kind={classifyEventKind(event.event_type)}>
                              <span className="activity-event-dot" aria-hidden />
                              <div className="activity-event-body">
                                {summarizeTaskEvent(event) && <p className="activity-event-summary activity-event-summary--lead">{summarizeTaskEvent(event)}</p>}
                                <div className="activity-event-topline">
                                  <strong className="event-kind-pill" data-event-kind={classifyEventKind(event.event_type)}>{eventTypeBadgeLabel(event.event_type)}</strong>
                                  {isDeterministicAutoToolEvent(event.event_type) && (
                                    <span className="event-auto-tool-badge">{t("tasks.auto_tool_badge")}</span>
                                  )}
                                  <span className="activity-event-at">{event.at}</span>
                                  {event.task_id && selectedTask && event.task_id !== selectedTask.id && (
                                    <span className="activity-event-subtask">{t("chat.sub_task")}{event.task_id.slice(-8)}</span>
                                  )}
                                </div>
                                {highlights.length > 0 && (
                                  <ul className="task-steps-list" role="list" aria-label={t("tasks.agent_discussions_title")}>
                                    {highlights.map((line, j) => (
                                      <li key={`discussion-line-${i}-${j}`} className="task-step task-step--pending">
                                        <span className="task-step-check" aria-hidden>•</span>
                                        <span className="task-step-title">{line}</span>
                                      </li>
                                    ))}
                                  </ul>
                                )}
                              </div>
                            </li>
                          ))}
                        </ul>
                      </div>
                    )}
                    {tasksList.length > 0 && tasksList[tasksSelected] && (() => {
                      const sel = tasksList[tasksSelected];
                      const canCancel = isTaskActiveStatus(sel.status);
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
                                  await invoke<{ cancelled?: boolean }>("cancel_task", { taskId: sel.id, port: DAEMON_PORT });
                                  void fetchTasksList({ silent: true });
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
                    {visibleTaskEvents.length === 0 ? (
                      <p className="empty-state">
                        {tasksList.length > 0 ? t("tasks.no_events") : t("tasks.select_task")}
                      </p>
                    ) : (
                      <>
                      {isSimpleMode && <p className="task-events-hint">{t("tasks.event_summary_hint")}</p>}
                      <ul className="activity-events-list" role="list">
                        {visibleTaskEvents.map((e, i) => (
                          <li key={`${e.event_type}-${e.at}-${i}`} className="activity-event-card" data-event-kind={classifyEventKind(e.event_type)}>
                            <span className="activity-event-dot" aria-hidden />
                            <div className="activity-event-body">
                              {summarizeTaskEvent(e) && <p className="activity-event-summary activity-event-summary--lead">{summarizeTaskEvent(e)}</p>}
                              <div className="activity-event-topline">
                                <strong className="event-kind-pill" data-event-kind={classifyEventKind(e.event_type)}>{eventTypeBadgeLabel(e.event_type)}</strong>
                                {isDeterministicAutoToolEvent(e.event_type) && (
                                  <span className="event-auto-tool-badge">{t("tasks.auto_tool_badge")}</span>
                                )}
                                <span className="activity-event-at">{e.at}</span>
                                {e.task_id && selectedTask && e.task_id !== selectedTask.id && (
                                  <span className="activity-event-subtask">{t("chat.sub_task")}{e.task_id.slice(-8)}</span>
                                )}
                              </div>
                              {(() => {
                                const metadata = extractModelMetadata(e);
                                if (metadata) {
                                  return (
                                    <div className="event-model-metadata">
                                      {metadata.thinking && (
                                        <div className="event-model-thinking">
                                          <strong className="metadata-label">{t("tasks.model_thinking_label")}</strong>
                                          <div className="thinking-content">{trimPreview(metadata.thinking, 500)}</div>
                                        </div>
                                      )}
                                      {metadata.response && (
                                        <div className="event-model-response">
                                          <strong className="metadata-label">{t("tasks.model_response_label")}</strong>
                                          <div className="response-content">{trimPreview(metadata.response, 500)}</div>
                                        </div>
                                      )}
                                      {(metadata.model || metadata.evalCount || metadata.totalDuration) && (
                                        <div className="event-model-metrics">
                                          <strong className="metadata-label">{t("tasks.model_metrics_label")}</strong>
                                          <dl className="metrics-list">
                                            {metadata.model && (
                                              <>
                                                <dt>{t("tasks.model_label")}</dt>
                                                <dd>{metadata.model}</dd>
                                              </>
                                            )}
                                            {metadata.doneReason && (
                                              <>
                                                <dt>{t("tasks.done_reason_label")}</dt>
                                                <dd>{metadata.doneReason}</dd>
                                              </>
                                            )}
                                            {metadata.evalCount && (
                                              <>
                                                <dt>{t("tasks.eval_count_label")}</dt>
                                                <dd>{metadata.evalCount}</dd>
                                              </>
                                            )}
                                            {metadata.promptEvalCount && (
                                              <>
                                                <dt>{t("tasks.prompt_eval_count_label")}</dt>
                                                <dd>{metadata.promptEvalCount}</dd>
                                              </>
                                            )}
                                            {metadata.totalDuration && (
                                              <>
                                                <dt>{t("tasks.total_duration_label")}</dt>
                                                <dd>{(metadata.totalDuration / 1000000000).toFixed(2)}s</dd>
                                              </>
                                            )}
                                            {metadata.loadDuration && (
                                              <>
                                                <dt>{t("tasks.load_duration_label")}</dt>
                                                <dd>{(metadata.loadDuration / 1000000).toFixed(2)}ms</dd>
                                              </>
                                            )}
                                            {metadata.promptEvalDuration && (
                                              <>
                                                <dt>{t("tasks.prompt_eval_duration_label")}</dt>
                                                <dd>{(metadata.promptEvalDuration / 1000000).toFixed(2)}ms</dd>
                                              </>
                                            )}
                                            {metadata.evalDuration && (
                                              <>
                                                <dt>{t("tasks.eval_duration_label")}</dt>
                                                <dd>{(metadata.evalDuration / 1000000).toFixed(2)}ms</dd>
                                              </>
                                            )}
                                          </dl>
                                        </div>
                                      )}
                                    </div>
                                  );
                                }
                                return null;
                              })()}
                              {(() => {
                                const visual = extractAdvancedViewData(e.payload);
                                if (!visual) return null;

                                if (visual.kind === "map") {
                                  return (
                                    <MapPluginEventView
                                      key={`map-ev-${e.at}-${i}`}
                                      visual={visual}
                                      t={t}
                                      width={460}
                                      height={180}
                                      toolbar="inline"
                                      onFullscreen={() => setEventVisualFullscreen({ visual, sourceEventType: e.event_type })}
                                      onExportCsv={() => exportAdvancedViewCsv(visual)}
                                    />
                                  );
                                }

                                const width = 460;
                                const height = 190;
                                const palette = ["#8b5cf6", "#22c55e", "#f97316", "#06b6d4", "#f59e0b", "#ec4899"];
                                const totalPoints = visual.series.reduce((acc, s) => acc + s.points.length, 0);
                                return (
                                  <div className="event-advanced-view event-advanced-view-chart">
                                    <strong className="metadata-label">
                                      {visual.title ?? (visual.kind === "graph" ? t("tasks.plugin_graph_title") : t("tasks.plugin_timeseries_title"))}
                                    </strong>
                                    <div className="event-advanced-toolbar">
                                      <button type="button" className="event-advanced-action-btn" onClick={() => setEventVisualFullscreen({ visual, sourceEventType: e.event_type })}>
                                        {t("tasks.open_fullscreen")}
                                      </button>
                                      <button type="button" className="event-advanced-action-btn" onClick={() => exportAdvancedViewCsv(visual)}>
                                        {t("tasks.export_csv")}
                                      </button>
                                    </div>
                                    <div className="event-advanced-metrics-inline" role="list">
                                      <span className="event-advanced-chip" role="listitem">{t("tasks.series_count_label")}: {visual.series.length}</span>
                                      <span className="event-advanced-chip" role="listitem">{t("tasks.points_count_label")}: {totalPoints}</span>
                                    </div>
                                    <svg
                                      className="event-advanced-chart"
                                      viewBox={`0 0 ${width} ${height}`}
                                      preserveAspectRatio="none"
                                      aria-label={visual.kind === "graph" ? t("tasks.plugin_graph_title") : t("tasks.plugin_timeseries_title")}
                                    >
                                      <rect x="0" y="0" width={width} height={height} rx="10" ry="10" className="event-advanced-chart-bg" />
                                      {visual.series.map((s, idx) => {
                                        const scaled = scalePoints(s.points, width, height);
                                        const pts = scaled.map((p) => `${p.x},${p.y}`).join(" ");
                                        return scaled.length >= 2 ? (
                                          <polyline key={`series-${idx}`} points={pts} className="event-advanced-series-line" style={{ stroke: palette[idx % palette.length] }} />
                                        ) : null;
                                      })}
                                    </svg>
                                  </div>
                                );
                              })()}
                              {e.payload != null && (isSimpleMode ? (
                                <details className="event-payload-details">
                                  <summary>{t("tasks.event_details")}</summary>
                                  <pre className="event-payload">{JSON.stringify(e.payload, null, 2)}</pre>
                                </details>
                              ) : (
                                <pre className="event-payload">{JSON.stringify(e.payload, null, 2)}</pre>
                              ))}
                            </div>
                          </li>
                        ))}
                      </ul>
                      </>
                    )}
                  </div>
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
              <button
                type="button"
                role="tab"
                aria-selected={calendarSubTab === "wakeups"}
                className={calendarSubTab === "wakeups" ? "active" : ""}
                onClick={() => setCalendarSubTab("wakeups")}
              >
                Rappels agent
              </button>
              <button
                type="button"
                role="tab"
                aria-selected={calendarSubTab === "external"}
                className={calendarSubTab === "external" ? "active" : ""}
                onClick={() => setCalendarSubTab("external")}
              >
                {locale === "en" ? "CalDAV / ICS" : "CalDAV / ICS"}
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
              <div className="calendar-grid-panel">
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
                      return (
                        <table className="calendar-grid-table calendar-grid-day" role="grid" aria-label="Calendrier jour">
                          <thead>
                            <tr>
                              <th scope="col" className="calendar-grid-col-time">Heure</th>
                              <th scope="col" className="calendar-grid-col-events">Événements</th>
                            </tr>
                          </thead>
                          <tbody>
                            {Array.from({ length: 24 }, (_, h) => {
                              const hourEvents = byHour[h];
                              const hasEvents = hourEvents.length > 0;
                              return (
                                <tr key={h} className={`calendar-grid-row${hasEvents ? " calendar-grid-row--active" : " calendar-grid-row--empty"}`}>
                                  <td className="calendar-grid-cell-time">{h}h00</td>
                                  <td className={`calendar-grid-cell-events${hasEvents ? " calendar-grid-cell-events--active" : " calendar-grid-cell-events--empty"}`}>
                                    {hasEvents && (
                                      <button
                                        type="button"
                                        className={`calendar-cell-view-all calendar-cell-view-all--slot ${calendarSlotAverageStatusClass(hourEvents)}`}
                                        onClick={() => openCellDetail(`${h}h00`, `day-${h}`, hourEvents)}
                                      >
                                        Voir tout ({hourEvents.length})
                                      </button>
                                    )}
                                  </td>
                                </tr>
                              );
                            })}
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
                            {Array.from({ length: 24 }, (_, hour) => {
                              const hourEventsByDay = dayKeys.map((key) => (byDay[key] ?? []).filter((e) => new Date(e.at).getHours() === hour));
                              const hourHasEvents = hourEventsByDay.some((evs) => evs.length > 0);
                              return (
                                <tr key={hour} className={`calendar-grid-row${hourHasEvents ? " calendar-grid-row--active" : " calendar-grid-row--empty"}`}>
                                  <td className="calendar-grid-cell-hour">{hour}h</td>
                                  {dayKeys.map((key, dayIndex) => {
                                    const cellEvents = hourEventsByDay[dayIndex];
                                    const hasEvents = cellEvents.length > 0;
                                    const slotLabel = `${key} ${hour}h`;
                                    return (
                                      <td key={key} className={`calendar-grid-cell-day${hasEvents ? " calendar-grid-cell-day--active" : " calendar-grid-cell-day--empty"}`}>
                                        {hasEvents && (
                                          <button
                                            type="button"
                                            className={`calendar-cell-view-all calendar-cell-view-all--slot ${calendarSlotAverageStatusClass(cellEvents)}`}
                                            onClick={() => openCellDetail(slotLabel, `${key}-${hour}`, cellEvents)}
                                          >
                                            Voir tout ({cellEvents.length})
                                          </button>
                                        )}
                                      </td>
                                    );
                                  })}
                                </tr>
                              );
                            })}
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
                      <div
                        className="calendar-grid-month"
                        role="grid"
                        aria-label="Calendrier mois"
                        style={{ "--calendar-month-rows": weeks.length } as CSSProperties}
                      >
                        {weekDayNames.map((wd) => (
                          <div key={wd} role="columnheader" className="calendar-grid-month-weekday">{wd}</div>
                        ))}
                        {weeks.map((weekRow, wi) =>
                          weekRow.map((key, di) => {
                            const cellEvents = key ? (byDay[key] ?? []) : [];
                            const slotLabel = key ? new Date(key + "T12:00:00").toLocaleDateString("fr-FR", { weekday: "short", day: "numeric", month: "short" }) : "";
                            return (
                              <div
                                key={`${wi}-${di}`}
                                role="gridcell"
                                className={`calendar-grid-cell-month${key ? "" : " calendar-grid-cell-month--empty"}`}
                              >
                                {key ? (
                                  <div className="calendar-cell-content">
                                    <span className="calendar-grid-day-num">{new Date(key + "T12:00:00").getDate()}</span>
                                    <div className="calendar-cell-inner">
                                      <ul className="calendar-grid-slot-events" role="list">
                                        {(calendarGridDedupedBySlot.byDate.get(key) ?? []).slice(0, 2).map(({ representative: e, count }, i) => (
                                          <li key={i} className={`calendar-event-block ${calendarGetEventStatusClass(e.status, e.type)}`} title={`${e.type}${e.type === "external" ? "" : ` — ${e.status}`}`} role="button" tabIndex={0} onClick={() => calendarOpenGridEvent(e)} onKeyDown={(ev) => { if (ev.key === "Enter" || ev.key === " ") { ev.preventDefault(); calendarOpenGridEvent(e); } }}>
                                            <span className="calendar-event-label">{calendarEventLabel(e)}{count > 1 ? ` (${count})` : ""}</span>
                                            <span className="calendar-event-time">{new Date(e.at).toLocaleTimeString("fr-FR", { hour: "2-digit", minute: "2-digit" })}</span>
                                          </li>
                                        ))}
                                      </ul>
                                    </div>
                                    <button type="button" className="calendar-cell-view-all" onClick={() => openCellDetail(slotLabel, key, cellEvents)}>Voir tout ({cellEvents.length})</button>
                                  </div>
                                ) : null}
                              </div>
                            );
                          })
                        )}
                      </div>
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
                              <button type="button" className={`calendar-event-block calendar-cell-detail-item ${calendarGetEventStatusClass(e.status, e.type)}`} style={{ width: "100%", textAlign: "left", cursor: e.task_id && e.type !== "external" ? "pointer" : "default" }} onClick={() => { if (e.task_id && e.type !== "external") { setCalendarCellDetail(null); setCalendarSelectedTaskId(e.task_id); } }}>
                                <span className="calendar-event-label">{calendarEventLabel(e)}{count > 1 ? ` (${count})` : ""}{e.type === "external" ? " · externe" : ""}</span>
                                <span className="calendar-event-time">{new Date(e.at).toLocaleString("fr-FR", { hour: "2-digit", minute: "2-digit", day: "numeric", month: "short" })}{e.type === "external" ? "" : ` — ${e.status}`}</span>
                              </button>
                            </li>
                          ))}
                        </ul>
                        {calendarCellDetail.events.length === 0 && <p className="muted">Aucune tâche pour ce créneau.</p>}
                      </div>
                    </div>
                  </div>
                )}
              </div>
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
            {!calendarLoading && calendarSubTab === "wakeups" && (
              <div className="calendar-wakeups-panel">
                <h3>Rappels agent (wakeups)</h3>
                <p className="calendar-wakeups-hint muted">
                  Messages programmés que l&apos;agent enverra dans la session courante à l&apos;heure prévue.
                </p>
                <form
                  className="calendar-wakeup-form"
                  onSubmit={(e) => {
                    e.preventDefault();
                    void (async () => {
                      const minutes = parseInt(wakeupFormMinutes, 10);
                      if (!sessionId || !wakeupFormMessage.trim() || !Number.isFinite(minutes) || minutes <= 0) return;
                      setWakeupSaving(true);
                      try {
                        const fire_at = new Date(Date.now() + minutes * 60_000).toISOString();
                        const res = await fetch(e2eDaemonHttpUrl("/api/wakeups"), {
                          method: "POST",
                          headers: { "Content-Type": "application/json" },
                          body: JSON.stringify({
                            session_id: sessionId,
                            message: wakeupFormMessage.trim(),
                            fire_at,
                          }),
                        });
                        if (res.ok) {
                          setWakeupFormMessage("");
                          await fetchWakeups();
                        }
                      } finally {
                        setWakeupSaving(false);
                      }
                    })();
                  }}
                >
                  <label className="calendar-wakeup-field">
                    <span>Dans (minutes)</span>
                    <input
                      type="number"
                      min={1}
                      max={525600}
                      value={wakeupFormMinutes}
                      onChange={(e) => setWakeupFormMinutes(e.target.value)}
                      disabled={wakeupSaving}
                    />
                  </label>
                  <label className="calendar-wakeup-field calendar-wakeup-field-grow">
                    <span>Message</span>
                    <input
                      type="text"
                      value={wakeupFormMessage}
                      onChange={(e) => setWakeupFormMessage(e.target.value)}
                      placeholder="Ex. Relancer la revue du PR"
                      disabled={wakeupSaving}
                    />
                  </label>
                  <button type="submit" className="refresh-btn" disabled={wakeupSaving || !sessionId}>
                    {wakeupSaving ? "…" : "Programmer"}
                  </button>
                </form>
                {!sessionId && (
                  <p className="muted">Ouvrez ou démarrez une session de chat pour créer un rappel.</p>
                )}
                {wakeups.length === 0 ? (
                  <p className="empty-state">Aucun rappel programmé.</p>
                ) : (
                  <ul className="calendar-wakeup-list" role="list">
                    {wakeups.map((w) => (
                      <li key={w.id} className={`calendar-wakeup-item calendar-wakeup-item--${w.status}`}>
                        <div className="calendar-wakeup-meta">
                          <strong>{new Date(w.fire_at).toLocaleString()}</strong>
                          <span className="calendar-wakeup-status">{w.status}</span>
                        </div>
                        <p className="calendar-wakeup-message">{w.message}</p>
                        {w.status === "pending" && (
                          <button
                            type="button"
                            className="calendar-wakeup-delete"
                            onClick={() => {
                              void (async () => {
                                try {
                                  await fetch(e2eDaemonHttpUrl(`/api/wakeups/${encodeURIComponent(w.id)}`), { method: "DELETE" });
                                  await fetchWakeups();
                                } catch {
                                  /* ignore */
                                }
                              })();
                            }}
                          >
                            {t("settings.delete")}
                          </button>
                        )}
                      </li>
                    ))}
                  </ul>
                )}
              </div>
            )}
            {!calendarLoading && calendarSubTab === "external" && (
              <CalDavAccountsPanel locale={locale} fetchEndpoint={fetchSystemEndpoint} />
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
                          <p className="muted">{t("notifications.check_center")}</p>
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
                          <p className="muted">{t("notifications.check_center")}</p>
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
            {memoryHygieneHint ? (
              <p className="memory-hygiene-hint" role="status">
                {memoryHygieneHint}
              </p>
            ) : null}
            {memoryAdvancedSettings ? (
              <details className="memory-advanced-settings">
                <summary>{t("memory.advanced_settings_title")}</summary>
                <div className="settings-list memory-advanced-settings-body">
                  <label className="memory-advanced-toggle">
                    <input
                      type="checkbox"
                      checked={memoryAdvancedSettings.multi_query ?? false}
                      onChange={(e) =>
                        setMemoryAdvancedSettings((s) => ({ ...s!, multi_query: e.target.checked }))
                      }
                    />
                    {t("memory.advanced_multi_query")}
                  </label>
                  <label className="memory-advanced-toggle">
                    <input
                      type="checkbox"
                      checked={memoryAdvancedSettings.hyde ?? false}
                      onChange={(e) =>
                        setMemoryAdvancedSettings((s) => ({ ...s!, hyde: e.target.checked }))
                      }
                    />
                    {t("memory.advanced_hyde")}
                  </label>
                  <label className="memory-advanced-toggle">
                    <input
                      type="checkbox"
                      checked={memoryAdvancedSettings.rrf ?? true}
                      onChange={(e) =>
                        setMemoryAdvancedSettings((s) => ({ ...s!, rrf: e.target.checked }))
                      }
                    />
                    {t("memory.advanced_rrf")}
                  </label>
                  <label className="memory-advanced-rollup">
                    <span>{t("memory.advanced_rollup_days")}</span>
                    <input
                      type="number"
                      min={0}
                      max={3650}
                      className="settings-input"
                      value={memoryAdvancedSettings.rollup_days ?? 90}
                      onChange={(e) =>
                        setMemoryAdvancedSettings((s) => ({
                          ...s!,
                          rollup_days: Math.max(0, parseInt(e.target.value, 10) || 0),
                        }))
                      }
                    />
                  </label>
                  <label className="memory-advanced-rollup">
                    <span>semantic_top_k</span>
                    <input
                      type="number"
                      min={1}
                      max={50}
                      className="settings-input"
                      value={memoryAdvancedSettings.semantic_top_k ?? 8}
                      onChange={(e) =>
                        setMemoryAdvancedSettings((s) => ({
                          ...s!,
                          semantic_top_k: Math.max(1, parseInt(e.target.value, 10) || 1),
                        }))
                      }
                    />
                  </label>
                  <label className="memory-advanced-rollup">
                    <span>graph_expand_hops</span>
                    <input
                      type="number"
                      min={0}
                      max={8}
                      className="settings-input"
                      value={memoryAdvancedSettings.graph_expand_hops ?? 2}
                      onChange={(e) =>
                        setMemoryAdvancedSettings((s) => ({
                          ...s!,
                          graph_expand_hops: Math.max(0, parseInt(e.target.value, 10) || 0),
                        }))
                      }
                    />
                  </label>
                  <label className="memory-advanced-rollup">
                    <span>user_rag_top_k</span>
                    <input
                      type="number"
                      min={0}
                      max={50}
                      className="settings-input"
                      value={memoryAdvancedSettings.user_rag_top_k ?? 6}
                      onChange={(e) =>
                        setMemoryAdvancedSettings((s) => ({
                          ...s!,
                          user_rag_top_k: Math.max(0, parseInt(e.target.value, 10) || 0),
                        }))
                      }
                    />
                  </label>
                  <label className="memory-advanced-rollup">
                    <span>workspace_graph_top_k</span>
                    <input
                      type="number"
                      min={0}
                      max={50}
                      className="settings-input"
                      value={memoryAdvancedSettings.workspace_graph_top_k ?? 6}
                      onChange={(e) =>
                        setMemoryAdvancedSettings((s) => ({
                          ...s!,
                          workspace_graph_top_k: Math.max(0, parseInt(e.target.value, 10) || 0),
                        }))
                      }
                    />
                  </label>
                  <button
                    type="button"
                    className="btn-secondary"
                    disabled={memoryAdvancedSaving}
                    onClick={() => void saveMemoryAdvancedSettings()}
                  >
                    {memoryAdvancedSaving ? t("common.loading") : t("memory.advanced_save")}
                  </button>
                  {memoryAdvancedMessage ? (
                    <p className="muted" role="status">{memoryAdvancedMessage}</p>
                  ) : null}
                  <p className="settings-doc muted">{t("memory.advanced_settings_hint")}</p>
                </div>
              </details>
            ) : null}
            <button
              type="button"
              className="refresh-btn"
              onClick={fetchMemory}
              aria-label="Rafraîchir la mémoire"
              disabled={memoryLoading}
            >
              Rafraîchir
            </button>
            {memoryLoading && (
              <p className="panel-loading" aria-busy="true">
                <span className="panel-loading-spinner" aria-hidden />
                {t("common.loading")}
              </p>
            )}
            {!memoryLoading && (
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
                        <div className="memory-list-scroll memory-search-results-scroll">
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

        {eventVisualFullscreen && (
          <div className="event-visual-overlay" role="dialog" aria-modal="true" aria-labelledby="event-visual-title" onClick={() => setEventVisualFullscreen(null)}>
            <div className="event-visual-modal" onClick={(e) => e.stopPropagation()}>
              <div className="event-visual-modal-header">
                <h2 id="event-visual-title" className="event-visual-modal-title">
                  {eventVisualFullscreen.visual.title
                    ?? (eventVisualFullscreen.visual.kind === "map"
                      ? t("tasks.plugin_map_title")
                      : eventVisualFullscreen.visual.kind === "graph"
                        ? t("tasks.plugin_graph_title")
                        : t("tasks.plugin_timeseries_title"))}
                </h2>
                <div className="event-visual-modal-actions">
                  <span className="event-visual-zoom-badge" title={t("tasks.zoom_level")}>{t("tasks.zoom_level")}: {eventVisualZoomPercent}%</span>
                  <button
                    type="button"
                    className="btn-secondary event-visual-help-btn"
                    aria-label={t("tasks.visual_help_toggle")}
                    title={t("tasks.visual_help_toggle")}
                    aria-pressed={eventVisualHelpOpen}
                    onClick={() => setEventVisualHelpOpen((prev) => !prev)}
                  >
                    ?
                  </button>
                  <button
                    type="button"
                    className="btn-secondary event-visual-zoom-btn"
                    onClick={() => setEventVisualTransform((prev) => ({ ...prev, scale: Math.max(0.5, Math.min(8, prev.scale / 1.15)) }))}
                    aria-label={t("tasks.zoom_out")}
                    title={t("tasks.zoom_out")}
                  >
                    −
                  </button>
                  <button
                    type="button"
                    className="btn-secondary event-visual-zoom-btn"
                    onClick={resetEventVisualViewport}
                    aria-label={t("tasks.reset_view")}
                    title={t("tasks.reset_view")}
                  >
                    {t("tasks.reset_view")}
                  </button>
                  <button
                    type="button"
                    className="btn-secondary"
                    onClick={resetEventVisualViewport}
                    aria-label={t("tasks.fit_to_screen")}
                    title={t("tasks.fit_to_screen")}
                  >
                    {t("tasks.fit_to_screen")}
                  </button>
                  <button
                    type="button"
                    className="btn-secondary event-visual-zoom-btn"
                    onClick={() => setEventVisualTransform((prev) => ({ ...prev, scale: Math.max(0.5, Math.min(8, prev.scale * 1.15)) }))}
                    aria-label={t("tasks.zoom_in")}
                    title={t("tasks.zoom_in")}
                  >
                    +
                  </button>
                  <button type="button" className="btn-secondary" onClick={() => exportAdvancedViewPng(eventVisualFullscreen.visual)}>
                    {t("tasks.export_png")}
                  </button>
                  <button type="button" className="btn-secondary" onClick={() => exportAdvancedViewCsv(eventVisualFullscreen.visual)}>
                    {t("tasks.export_csv")}
                  </button>
                  <button type="button" className="calendar-detail-modal-close" aria-label={t("common.close")} onClick={() => setEventVisualFullscreen(null)}>
                    ×
                  </button>
                </div>
              </div>
              <div className="event-visual-modal-body">
                <p className="event-visual-hint">{t("tasks.pan_zoom_hint")}</p>
                <p className="event-visual-hint event-visual-shortcuts-hint">{t("tasks.zoom_shortcuts_hint")}</p>
                {eventVisualHelpOpen && (
                  <div className="event-visual-help-panel" role="note" aria-label={t("tasks.visual_help_title")}>
                    <strong className="event-visual-help-title">{t("tasks.visual_help_title")}</strong>
                    <ul className="event-visual-help-list">
                      <li>{t("tasks.visual_help_mouse")}</li>
                      <li>{t("tasks.visual_help_touch")}</li>
                      <li>{t("tasks.visual_help_keyboard")}</li>
                    </ul>
                  </div>
                )}
                {eventVisualFullscreen.visual.kind === "map" ? (
                  <MapPluginEventView
                    visual={eventVisualFullscreen.visual}
                    t={t}
                    width={1200}
                    height={520}
                    toolbar="hidden"
                    layout="fullscreen"
                    showPanelHeading={false}
                    endPointR={4.2}
                    midPointR={3}
                    interactive={{
                      dragging: eventVisualDragging != null,
                      transform: `translate(${eventVisualTransform.tx} ${eventVisualTransform.ty}) scale(${eventVisualTransform.scale})`,
                      onWheel: handleEventVisualWheel,
                      onDoubleClick: resetEventVisualViewport,
                      onPointerDown: handleEventVisualPointerDown,
                      onPointerMove: handleEventVisualPointerMove,
                      onPointerUp: handleEventVisualPointerEnd,
                      onPointerCancel: handleEventVisualPointerEnd,
                      onPointerLeave: handleEventVisualPointerEnd,
                    }}
                  />
                ) : (
                  (() => {
                    const width = 1200;
                    const height = 560;
                    const palette = ["#8b5cf6", "#22c55e", "#f97316", "#06b6d4", "#f59e0b", "#ec4899"];
                    const totalPoints = eventVisualFullscreen.visual.series.reduce((acc, s) => acc + s.points.length, 0);
                    return (
                      <div className="event-advanced-view event-advanced-view-chart event-advanced-view-fullscreen">
                        <div className="event-advanced-metrics-inline" role="list">
                          <span className="event-advanced-chip" role="listitem">{t("tasks.series_count_label")}: {eventVisualFullscreen.visual.series.length}</span>
                          <span className="event-advanced-chip" role="listitem">{t("tasks.points_count_label")}: {totalPoints}</span>
                          <span className="event-advanced-chip" role="listitem">event: {eventVisualFullscreen.sourceEventType}</span>
                        </div>
                        <div
                          className={`event-visual-interactive-surface ${eventVisualDragging ? "is-dragging" : ""}`}
                          onWheel={handleEventVisualWheel}
                          onDoubleClick={resetEventVisualViewport}
                          onPointerDown={handleEventVisualPointerDown}
                          onPointerMove={handleEventVisualPointerMove}
                          onPointerUp={handleEventVisualPointerEnd}
                          onPointerCancel={handleEventVisualPointerEnd}
                          onPointerLeave={handleEventVisualPointerEnd}
                        >
                        <svg
                          className="event-advanced-chart"
                          viewBox={`0 0 ${width} ${height}`}
                          preserveAspectRatio="none"
                          aria-label={eventVisualFullscreen.visual.kind === "graph" ? t("tasks.plugin_graph_title") : t("tasks.plugin_timeseries_title")}
                        >
                          <rect x="0" y="0" width={width} height={height} rx="10" ry="10" className="event-advanced-chart-bg" />
                          <g transform={`translate(${eventVisualTransform.tx} ${eventVisualTransform.ty}) scale(${eventVisualTransform.scale})`}>
                            {eventVisualFullscreen.visual.series.map((s, idx) => {
                              const scaled = scalePoints(s.points, width, height);
                              const pts = scaled.map((p) => `${p.x},${p.y}`).join(" ");
                              return scaled.length >= 2 ? (
                                <polyline key={`series-modal-${idx}`} points={pts} className="event-advanced-series-line" style={{ stroke: palette[idx % palette.length] }} />
                              ) : null;
                            })}
                          </g>
                        </svg>
                        </div>
                      </div>
                    );
                  })()
                )}
              </div>
            </div>
          </div>
        )}

        {tab === "mission" && (
          <section
            id="panel-mission"
            role="tabpanel"
            aria-labelledby="tab-mission"
            className="panel memory-panel"
          >
            <h2 className="panel-title">
              {t("mission.title")}
              <InfoTip label={t("mission.title")} content={<>{t("mission.description")}<br /><br />{t("mission.intro_detail")}</>} />
            </h2>
            <button type="button" className="refresh-btn" onClick={() => void fetchMission()} disabled={missionLoading}>
              {missionLoading ? t("common.loading") : t("mission.refresh")}
            </button>
            {!missionLoading && mission && missionDraft && (
              <div className="memory-content-wrap mission-panel-body">
                <nav className="settings-tabs" role="tablist" aria-label={t("mission.title")} style={{ marginBottom: "0.75rem" }}>
                  <button
                    type="button"
                    role="tab"
                    aria-selected={missionSubTab === "goals"}
                    className={missionSubTab === "goals" ? "active" : ""}
                    onClick={() => setMissionSubTab("goals")}
                  >
                    {t("mission.section_goals")}
                  </button>
                  <button
                    type="button"
                    role="tab"
                    aria-selected={missionSubTab === "settings"}
                    className={missionSubTab === "settings" ? "active" : ""}
                    onClick={() => setMissionSubTab("settings")}
                  >
                    {t("mission.section_settings")}
                  </button>
                  <button
                    type="button"
                    role="tab"
                    aria-selected={missionSubTab === "status"}
                    className={missionSubTab === "status" ? "active" : ""}
                    onClick={() => setMissionSubTab("status")}
                  >
                    {t("mission.section_status")}
                  </button>
                </nav>
                {missionDirty && (
                  <p className="muted" role="status">
                    {t("mission.unsaved")}
                  </p>
                )}
                {missionSubTab === "goals" && (
                  <>
                    <label className="settings-field">
                      <input
                        type="checkbox"
                        checked={missionDraft.enabled}
                        onChange={(e) => setMissionDraft({ ...missionDraft, enabled: e.target.checked })}
                      />{" "}
                      {t("mission.enabled")}
                    </label>
                    <label className="settings-field">
                      {t("mission.context")}
                      <textarea
                        rows={5}
                        value={missionDraft.global_context}
                        onChange={(e) => setMissionDraft({ ...missionDraft, global_context: e.target.value })}
                      />
                    </label>
                    <label className="settings-field">
                      {t("mission.objective")}
                      <textarea
                        rows={5}
                        value={missionDraft.objective}
                        onChange={(e) => setMissionDraft({ ...missionDraft, objective: e.target.value })}
                      />
                    </label>
                    <label className="settings-field">
                      {t("mission.operating_rules")}
                      <span className="muted" style={{ display: "block", fontWeight: "normal", marginBottom: "0.25rem" }}>
                        {t("mission.operating_rules_hint")}
                      </span>
                      <textarea
                        rows={5}
                        value={missionDraft.operating_rules ?? ""}
                        onChange={(e) => setMissionDraft({ ...missionDraft, operating_rules: e.target.value })}
                      />
                    </label>
                    <h3 className="panel-subtitle" style={{ marginTop: "1rem" }}>
                      {t("mission.roles_title")}
                    </h3>
                    <p className="muted">{t("mission.roles_hint")}</p>
                    {(missionDraft.role_definitions ?? []).map((row, idx) => (
                      <div
                        key={idx}
                        className="settings-field"
                        style={{
                          border: "1px solid color-mix(in srgb, currentColor 18%, transparent)",
                          borderRadius: 8,
                          padding: "0.75rem",
                          marginBottom: "0.5rem",
                        }}
                      >
                        <label className="settings-field" style={{ marginBottom: "0.5rem" }}>
                          {t("mission.role_name")}
                          <input
                            type="text"
                            value={row.name}
                            onChange={(e) => {
                              const next = [...(missionDraft.role_definitions ?? [])];
                              next[idx] = { ...next[idx], name: e.target.value };
                              setMissionDraft({ ...missionDraft, role_definitions: next });
                            }}
                          />
                        </label>
                        <label className="settings-field" style={{ marginBottom: "0.5rem" }}>
                          {t("mission.role_responsibility")}
                          <textarea
                            rows={2}
                            value={row.responsibility}
                            onChange={(e) => {
                              const next = [...(missionDraft.role_definitions ?? [])];
                              next[idx] = { ...next[idx], responsibility: e.target.value };
                              setMissionDraft({ ...missionDraft, role_definitions: next });
                            }}
                          />
                        </label>
                        <label className="settings-field">
                          {t("mission.role_agent")}
                          <select
                            value={row.preferred_agent_type ?? ""}
                            onChange={(e) => {
                              const next = [...(missionDraft.role_definitions ?? [])];
                              const v = e.target.value;
                              next[idx] = {
                                ...next[idx],
                                preferred_agent_type: v === "" ? null : v,
                              };
                              setMissionDraft({ ...missionDraft, role_definitions: next });
                            }}
                          >
                            <option value="">{t("mission.role_agent_default")}</option>
                            {MISSION_HEARTBEAT_AGENT_TYPES.map((ag) => (
                              <option key={ag} value={ag}>
                                {ag}
                              </option>
                            ))}
                          </select>
                        </label>
                        <button
                          type="button"
                          className="btn-secondary"
                          style={{ marginTop: "0.5rem" }}
                          onClick={() => {
                            const next = [...(missionDraft.role_definitions ?? [])];
                            next.splice(idx, 1);
                            setMissionDraft({ ...missionDraft, role_definitions: next });
                          }}
                        >
                          {t("mission.remove_role")}
                        </button>
                      </div>
                    ))}
                    <button
                      type="button"
                      className="btn-secondary"
                      onClick={() =>
                        setMissionDraft({
                          ...missionDraft,
                          role_definitions: [
                            ...(missionDraft.role_definitions ?? []),
                            { name: "", responsibility: "", preferred_agent_type: null },
                          ],
                        })
                      }
                    >
                      {t("mission.add_role")}
                    </button>
                  </>
                )}
                {missionSubTab === "settings" && (
                  <>
                    <label className="settings-field">
                      {t("mission.horizon")}
                      <select
                        value={missionDraft.horizon}
                        onChange={(e) => setMissionDraft({ ...missionDraft, horizon: e.target.value })}
                      >
                        <option value="short">{t("mission.horizon_short")}</option>
                        <option value="medium">{t("mission.horizon_medium")}</option>
                        <option value="long">{t("mission.horizon_long")}</option>
                      </select>
                    </label>
                    <label className="settings-field">
                      {t("mission.heartbeat_minutes")}
                      <input
                        type="number"
                        min={1}
                        value={missionDraft.heartbeat_interval_minutes}
                        onChange={(e) =>
                          setMissionDraft({
                            ...missionDraft,
                            heartbeat_interval_minutes: Math.max(1, parseInt(e.target.value, 10) || 1),
                          })
                        }
                      />
                    </label>
                    <label className="settings-field">
                      {t("mission.heartbeat_task_type")}
                      <span className="muted" style={{ display: "block", fontWeight: "normal", marginBottom: "0.25rem" }}>
                        {t("mission.heartbeat_task_type_hint")}
                      </span>
                      <select
                        value={missionDraft.heartbeat_preferred_task_type ?? "project_manager"}
                        onChange={(e) =>
                          setMissionDraft({ ...missionDraft, heartbeat_preferred_task_type: e.target.value })
                        }
                      >
                        {MISSION_HEARTBEAT_AGENT_TYPES.map((ag) => (
                          <option key={ag} value={ag}>
                            {ag}
                          </option>
                        ))}
                      </select>
                    </label>
                    <label className="settings-field">
                      {t("mission.report_dir")}
                      <input
                        type="text"
                        value={missionDraft.report_dir}
                        onChange={(e) => setMissionDraft({ ...missionDraft, report_dir: e.target.value })}
                      />
                    </label>
                    <label className="settings-field">
                      {t("mission.session_id")}
                      <input
                        type="text"
                        value={missionDraft.session_id}
                        onChange={(e) => setMissionDraft({ ...missionDraft, session_id: e.target.value })}
                      />
                    </label>
                  </>
                )}
                {(missionSubTab === "goals" || missionSubTab === "settings") && (
                  <div className="schedule-detail-actions" style={{ marginTop: "1rem" }}>
                    <button
                      type="button"
                      className="btn-primary"
                      disabled={missionSaving || !missionDirty}
                      onClick={async () => {
                        setMissionSaving(true);
                        try {
                          const payload = {
                            enabled: missionDraft.enabled,
                            global_context: missionDraft.global_context,
                            horizon: missionDraft.horizon,
                            objective: missionDraft.objective,
                            heartbeat_interval_minutes: missionDraft.heartbeat_interval_minutes,
                            report_dir: missionDraft.report_dir,
                            session_id: missionDraft.session_id,
                            operating_rules: missionDraft.operating_rules ?? "",
                            role_definitions: missionDraft.role_definitions ?? [],
                            heartbeat_preferred_task_type:
                              missionDraft.heartbeat_preferred_task_type ?? "project_manager",
                          };
                          let j: MissionApi;
                          if (E2E_WEB) {
                            const r = await fetch(e2eDaemonHttpUrl("/api/autonomous-mission"), {
                              method: "PUT",
                              headers: { "Content-Type": "application/json" },
                              body: JSON.stringify(payload),
                            });
                            if (!r.ok) throw new Error(`HTTP ${r.status}`);
                            j = normalizeMissionApi((await r.json()) as MissionApi);
                          } else {
                            j = normalizeMissionApi(
                              (await invoke<MissionApi>("put_autonomous_mission", {
                                body: payload,
                                port: DAEMON_PORT,
                              })) as MissionApi
                            );
                          }
                          setMission(j);
                          setMissionDraft(cloneMission(j));
                        } catch (e) {
                          setMissionError(String(e));
                        } finally {
                          setMissionSaving(false);
                        }
                      }}
                    >
                      {missionSaving ? "…" : t("mission.save")}
                    </button>
                    <button
                      type="button"
                      className="btn-secondary"
                      disabled={missionSaving || !missionDirty}
                      onClick={() => mission && setMissionDraft(cloneMission(mission))}
                    >
                      {t("mission.discard")}
                    </button>
                  </div>
                )}
                {missionSubTab === "status" && (
                  <>
                    <p>
                      <strong>{t("mission.status")}:</strong> {mission.status}
                    </p>
                    {mission.report_path_absolute && (
                      <p className="muted">
                        <strong>{t("mission.report_path")}:</strong> {mission.report_path_absolute}
                      </p>
                    )}
                    {(mission.last_heartbeat_at || mission.next_heartbeat_approx_at) && (
                      <p className="muted">
                        {mission.last_heartbeat_at && (
                          <>
                            {t("mission.last_heartbeat")}: {mission.last_heartbeat_at}{" "}
                          </>
                        )}
                        {mission.next_heartbeat_approx_at && (
                          <>
                            · {t("mission.next_heartbeat")}: {mission.next_heartbeat_approx_at}
                          </>
                        )}
                      </p>
                    )}
                    <div className="schedule-detail-actions" style={{ marginTop: "0.75rem" }}>
                      <button
                        type="button"
                        className="btn-secondary"
                        disabled={missionSaving}
                        onClick={async () => {
                          try {
                            let j: MissionApi;
                            if (E2E_WEB) {
                              const r = await fetch(e2eDaemonHttpUrl("/api/autonomous-mission/pause"), {
                                method: "POST",
                              });
                              if (!r.ok) throw new Error(`HTTP ${r.status}`);
                              j = normalizeMissionApi((await r.json()) as MissionApi);
                            } else {
                              j = normalizeMissionApi(
                                (await invoke<MissionApi>("post_autonomous_mission_pause", {
                                  port: DAEMON_PORT,
                                })) as MissionApi
                              );
                            }
                            setMission(j);
                            setMissionDraft(cloneMission(j));
                          } catch (e) {
                            setMissionError(String(e));
                          }
                        }}
                      >
                        {t("mission.pause")}
                      </button>
                      <button
                        type="button"
                        className="btn-secondary"
                        disabled={missionSaving}
                        onClick={async () => {
                          try {
                            let j: MissionApi;
                            if (E2E_WEB) {
                              const r = await fetch(e2eDaemonHttpUrl("/api/autonomous-mission/resume"), {
                                method: "POST",
                              });
                              if (!r.ok) throw new Error(`HTTP ${r.status}`);
                              j = normalizeMissionApi((await r.json()) as MissionApi);
                            } else {
                              j = normalizeMissionApi(
                                (await invoke<MissionApi>("post_autonomous_mission_resume", {
                                  port: DAEMON_PORT,
                                })) as MissionApi
                              );
                            }
                            setMission(j);
                            setMissionDraft(cloneMission(j));
                          } catch (e) {
                            setMissionError(String(e));
                          }
                        }}
                      >
                        {t("mission.resume")}
                      </button>
                    </div>
                    <h3 className="panel-subtitle" style={{ marginTop: "1.5rem" }}>
                      {t("mission.activity_title")}
                    </h3>
                    {(() => {
                      const buckets = Array.from({ length: 7 }, () => 0);
                      const now = Date.now();
                      const dayMs = 86400000;
                      for (const ev of missionEvents) {
                        const t0 = new Date(ev.at).getTime();
                        const dayIdx = Math.floor((now - t0) / dayMs);
                        if (dayIdx >= 0 && dayIdx < 7) buckets[6 - dayIdx] += 1;
                      }
                      const max = Math.max(1, ...buckets);
                      const w = 280;
                      const h = 80;
                      const bw = w / 7;
                      return (
                        <svg
                          viewBox={`0 0 ${w} ${h}`}
                          className="event-advanced-chart"
                          width={w}
                          height={h}
                          aria-label={t("mission.chart_label")}
                        >
                          <rect x="0" y="0" width={w} height={h} rx="8" className="event-advanced-chart-bg" />
                          {buckets.map((n, i) => (
                            <rect
                              key={i}
                              x={i * bw + 2}
                              y={h - 4 - (n / max) * (h - 12)}
                              width={bw - 4}
                              height={Math.max(1, (n / max) * (h - 12))}
                              fill="currentColor"
                              opacity={0.55}
                            />
                          ))}
                        </svg>
                      );
                    })()}
                    <ul className="memory-turns-list" style={{ marginTop: "1rem" }}>
                      {missionEvents.slice(-20).map((ev) => (
                        <li key={ev.id} className="memory-turn memory-turn-assistant">
                          <span className="memory-turn-role">{ev.event_type}</span>
                          <div className="memory-turn-content">
                            <time dateTime={ev.at}>{ev.at}</time>
                            {ev.payload != null ? ` — ${JSON.stringify(ev.payload)}` : ""}
                          </div>
                        </li>
                      ))}
                    </ul>
                  </>
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
              <dd className="settings-theme-editor-cell">
                <ThemeEditorPanel theme={theme} t={t} />
              </dd>
              <dt>{t("settings.ui_mode")}</dt>
              <dd>
                <select
                  aria-label={t("settings.ui_mode")}
                  className="settings-theme-select"
                  value={uiMode}
                  onChange={(e) => setUiModeAndSave(e.target.value as UiMode)}
                >
                  <option value="simple">{t("settings.ui_mode_simple")}</option>
                  <option value="expert">{t("settings.ui_mode_expert")}</option>
                </select>
                <span className="settings-theme-hint">{t("settings.ui_mode_hint")}</span>
              </dd>
              <dt>{t("settings.ui_density")}</dt>
              <dd>
                <select
                  aria-label={t("settings.ui_density")}
                  className="settings-theme-select"
                  value={uiDensity}
                  onChange={(e) => {
                    const next = e.target.value as UiDensity;
                    setUiDensity(next);
                    try {
                      localStorage.setItem(DENSITY_STORAGE_KEY, next);
                    } catch {
                      /* ignore */
                    }
                  }}
                >
                  <option value="compact">{t("settings.ui_density_compact")}</option>
                  <option value="comfortable">{t("settings.ui_density_comfortable")}</option>
                  <option value="spacious">{t("settings.ui_density_spacious")}</option>
                </select>
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
              <dt>{t("settings.chat_tips")}</dt>
              <dd>
                <label className="settings-checkbox-label">
                  <input
                    type="checkbox"
                    checked={chatTipsEnabled}
                    onChange={(e) => setChatTipsEnabledAndSave(e.target.checked)}
                  />
                  <span>{t("settings.chat_tips_hint")}</span>
                </label>
              </dd>
              <dt>{t("settings.chat_prompt_chips")}</dt>
              <dd>
                <label className="settings-checkbox-label">
                  <input
                    type="checkbox"
                    checked={chatPromptChipsEnabled}
                    onChange={(e) => setChatPromptChipsEnabledAndSave(e.target.checked)}
                  />
                  <span>{t("settings.chat_prompt_chips_hint")}</span>
                </label>
              </dd>
              <dt>{t("settings.chat_buddy")}</dt>
              <dd>
                <label className="settings-checkbox-label">
                  <input
                    type="checkbox"
                    checked={buddyLineEnabled}
                    onChange={(e) => setBuddyLineEnabledAndSave(e.target.checked)}
                  />
                  <span>{t("settings.chat_buddy_hint")}</span>
                </label>
              </dd>
            </dl>
              </div>
            )}
            {settingsSection === "system" && (
              <div className="settings-section-content">
                <div className="settings-system-subtabs" role="tablist" aria-label={t("settings.section_system")}>
                  <button
                    type="button"
                    role="tab"
                    aria-selected={systemSubTab === "general"}
                    className={systemSubTab === "general" ? "active" : ""}
                    onClick={() => setSystemSubTab("general")}
                  >
                    {t("settings.system_subtab_general")}
                  </button>
                  <button
                    type="button"
                    role="tab"
                    aria-selected={systemSubTab === "plugins"}
                    className={systemSubTab === "plugins" ? "active" : ""}
                    onClick={() => setSystemSubTab("plugins")}
                  >
                    {t("settings.system_subtab_plugins")}
                  </button>
                  <button
                    type="button"
                    role="tab"
                    aria-selected={systemSubTab === "policy"}
                    className={systemSubTab === "policy" ? "active" : ""}
                    onClick={() => setSystemSubTab("policy")}
                  >
                    {t("settings.system_subtab_policy")}
                  </button>
                  <button
                    type="button"
                    role="tab"
                    aria-selected={systemSubTab === "connectors"}
                    className={systemSubTab === "connectors" ? "active" : ""}
                    onClick={() => setSystemSubTab("connectors")}
                  >
                    {t("settings.system_subtab_connectors")}
                  </button>
                  <button
                    type="button"
                    role="tab"
                    aria-selected={systemSubTab === "health"}
                    className={systemSubTab === "health" ? "active" : ""}
                    onClick={() => setSystemSubTab("health")}
                  >
                    {t("settings.system_subtab_health")}
                  </button>
                </div>

                {systemSubTab === "general" && (
                  <>
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
                      <dt>{t("settings.utility_model")}</dt>
                      <dd>
                        <p className="settings-doc muted">{t("settings.utility_model_hint")}</p>
                      </dd>
                    </dl>
                    <OpenClawMigrationPanel locale={locale} fetchEndpoint={fetchSystemEndpoint} />
                  </>
                )}

                {systemSubTab === "policy" && <ToolsPolicyPanel t={t} />}

                {systemSubTab === "connectors" && <ConnectorsPanel t={t} />}

                {systemSubTab === "plugins" && (
                  <>
                    <section className="settings-card">
                      <h4>{locale === "en" ? "Skills catalog" : "Catalogue skills"}</h4>
                      <p className="settings-doc muted">
                        {locale === "en"
                          ? "Browse community skills or list locally installed skills from the daemon."
                          : "Parcourir le catalogue communautaire ou lister les skills installés via le daemon."}
                      </p>
                      <div className="settings-row-actions">
                        <a
                          className="btn-secondary"
                          href="https://azerothl.github.io/Akasha_app/skills.html"
                          target="_blank"
                          rel="noopener noreferrer"
                        >
                          {locale === "en" ? "Open gallery" : "Ouvrir la galerie"}
                        </a>
                        <a
                          className="btn-secondary"
                          href="https://github.com/azerothl/Akasha_skills"
                          target="_blank"
                          rel="noopener noreferrer"
                        >
                          GitHub
                        </a>
                        <button
                          type="button"
                          className="btn-secondary"
                          disabled={skillsCatalogLoading}
                          onClick={() => void loadInstalledSkills()}
                        >
                          {skillsCatalogLoading
                            ? t("common.loading")
                            : locale === "en"
                              ? "List installed"
                              : "Lister installés"}
                        </button>
                      </div>
                      {skillsCatalogText ? (
                        <pre className="onboarding-doctor-output">{skillsCatalogText}</pre>
                      ) : null}
                    </section>
                    <PluginCatalogPanel
                      fetchEndpoint={fetchSystemEndpoint}
                      requestEndpoint={requestSystemEndpoint}
                      installedIds={new Set(pluginStatusList.map((p) => p.id ?? "").filter(Boolean))}
                      onInstalled={() => void fetchPluginStatus()}
                      locale={locale}
                    />
                    <h3 className="settings-subtitle">{t("settings.plugins_status_title")}</h3>
                    <p className="settings-doc muted">{t("settings.plugin_reputation_desc")}</p>
                    <div className="settings-plugin-status-header">
                      <div />
                      <div className="settings-row-actions">
                        <button type="button" className="btn-secondary" disabled={pluginStatusLoading} onClick={fetchPluginStatus}>
                          {pluginStatusLoading ? t("common.loading") : t("sidebar.refresh_tasks")}
                        </button>
                        <button type="button" className="btn-secondary" disabled={!!pluginTableBusyId} onClick={reloadPluginsFromDisk}>
                          {t("settings.plugins_reload")}
                        </button>
                      </div>
                    </div>
                    {!pluginStatusError && pluginStatusList.length === 0 && !pluginStatusLoading && (
                      <p className="settings-doc muted">{t("doctor.no_plugins")}</p>
                    )}
                    {pluginStatusList.length > 0 && (
                      <div className="settings-plugin-table-wrap">
                        <table className="settings-plugin-table">
                          <thead>
                            <tr>
                              <th>{t("settings.plugins_table_name")}</th>
                              <th>{t("settings.plugins_table_description")}</th>
                              <th>{t("settings.plugins_table_version")}</th>
                              <th>{t("settings.plugins_table_score")}</th>
                              <th>{t("settings.plugins_table_status")}</th>
                              <th>{t("settings.plugins_table_actions")}</th>
                            </tr>
                          </thead>
                          <tbody>
                            {pluginStatusList.map((p) => {
                              const id = p.id ?? "?";
                              const enabled = p.enabled !== false;
                              const reason = typeof p.disabled_reason === "string" ? p.disabled_reason : null;
                              const disabledByReputation = !enabled && reason === "reputation";
                              const disabledByManual = !enabled && reason === "manual";
                              const scoreVal = typeof p.score === "number" ? p.score : 100;
                              const busy = pluginTableBusyId === id;
                              return (
                                <tr key={id}>
                                  <td>
                                    <div className="settings-plugin-table-namecell">
                                      <span className="settings-plugin-table-name">{p.name ?? id}</span>
                                      <span className="settings-plugin-table-id muted">{id}</span>
                                    </div>
                                  </td>
                                  <td className="settings-plugin-desc-cell muted">{p.description?.trim() || "—"}</td>
                                  <td>{p.version ?? "—"}</td>
                                  <td>
                                    <div className="settings-plugin-score-wrap">
                                      <span className="settings-plugin-score-num">{scoreVal}/100</span>
                                      <div className="settings-plugin-score-bar" aria-hidden>
                                        <span style={{ width: `${Math.min(100, Math.max(0, scoreVal))}%` }} />
                                      </div>
                                    </div>
                                  </td>
                                  <td>
                                    <div className="settings-plugin-status-badges">
                                      <span className={`settings-plugin-status-pill ${enabled ? "settings-plugin-status-pill-ok" : "settings-plugin-status-pill-off"}`}>
                                        {enabled ? t("settings.plugins_status_enabled") : t("settings.plugins_status_disabled")}
                                      </span>
                                      {disabledByReputation && (
                                        <span className="settings-plugin-status-pill settings-plugin-status-pill-reputation">
                                          {t("settings.plugins_status_disabled_reputation")}
                                        </span>
                                      )}
                                      {disabledByManual && (
                                        <span className="settings-plugin-status-pill settings-plugin-status-pill-manual">
                                          {t("settings.plugins_status_disabled_manual")}
                                        </span>
                                      )}
                                    </div>
                                  </td>
                                  <td>
                                    <div className="settings-plugin-actions">
                                      {enabled ? (
                                        <button
                                          type="button"
                                          className="btn-secondary"
                                          disabled={busy || !!pluginTableBusyId || id === "?"}
                                          onClick={() => setPluginRowEnabled(id, false)}
                                        >
                                          {t("settings.plugins_action_disable")}
                                        </button>
                                      ) : null}
                                      {!enabled && reason !== "reputation" && (
                                        <button
                                          type="button"
                                          className="btn-secondary"
                                          disabled={busy || !!pluginTableBusyId || id === "?"}
                                          onClick={() => setPluginRowEnabled(id, true)}
                                        >
                                          {t("settings.plugins_action_enable")}
                                        </button>
                                      )}
                                      {disabledByReputation && (
                                        <button
                                          type="button"
                                          className="settings-link-btn"
                                          disabled={pluginReputationResetLoading || busy || !!pluginTableBusyId || id === "?"}
                                          onClick={() => resetPluginReputation(id)}
                                        >
                                          {t("settings.plugins_action_reset_reputation")}
                                        </button>
                                      )}
                                      <button
                                        type="button"
                                        className="settings-link-btn"
                                        disabled={busy || !!pluginTableBusyId || id === "?"}
                                        onClick={() => uninstallPluginRow(id)}
                                      >
                                        {t("settings.plugins_action_uninstall")}
                                      </button>
                                    </div>
                                  </td>
                                </tr>
                              );
                            })}
                          </tbody>
                        </table>
                      </div>
                    )}

                    <details className="health-card" style={{ marginTop: "1.25rem" }}>
                      <summary className="health-card-details-summary">{t("settings.plugins_reputation_section")}</summary>
                      <h4 className="settings-plugin-status-title" style={{ marginTop: "0.75rem" }}>{t("settings.plugin_reputation_title")}</h4>
                      <div className="settings-plugin-reputation-controls">
                        <div className="settings-plugin-reputation-row">
                          <label htmlFor="plugin-reputation-target" className="settings-label">
                            {t("settings.plugin_reputation_plugin_id")}
                          </label>
                          <input
                            id="plugin-reputation-target"
                            type="text"
                            className="settings-input"
                            value={pluginReputationTarget}
                            onChange={(e) => setPluginReputationTarget(e.target.value)}
                            placeholder="maps"
                          />
                          <button
                            type="button"
                            className="settings-link-btn"
                            disabled={pluginReputationResetLoading || !pluginReputationTarget.trim()}
                            onClick={() => resetPluginReputation(pluginReputationTarget)}
                          >
                            {pluginReputationResetLoading ? t("common.loading") : t("settings.plugin_reputation_reset_one")}
                          </button>
                        </div>
                        <div className="settings-plugin-reputation-row">
                          <button
                            type="button"
                            className="btn-secondary"
                            disabled={pluginReputationResetLoading}
                            onClick={() => resetPluginReputation()}
                          >
                            {t("settings.plugin_reputation_reset_all")}
                          </button>
                        </div>
                        {pluginReputationResetMessage && (
                          <p className="settings-plugin-reputation-feedback settings-plugin-reputation-feedback-ok" role="status">
                            {pluginReputationResetMessage}
                          </p>
                        )}
                      </div>
                    </details>
                  </>
                )}

                {systemSubTab === "health" && (
                  <SystemHealthPanel
                    sessionId={sessionId}
                    fetchEndpoint={fetchSystemEndpoint}
                    requestEndpoint={requestSystemEndpoint}
                    expert={uiMode === "expert"}
                    locale={locale}
                    labels={{
                      title: t("settings.system_health_title"),
                      resumeHeading: t("settings.system_health_resume"),
                      toolsHeading: t("settings.system_health_tools"),
                      noSession: t("settings.system_health_no_session"),
                      docsMatrix: t("settings.system_health_doc_matrix"),
                      docsWebhooks: t("settings.system_health_doc_webhooks"),
                      docsMcp: t("settings.system_health_doc_mcp"),
                      recallHeading: t("settings.system_health_recall"),
                      mcpHeading: t("settings.system_health_mcp_status"),
                      lifecycleHeading: t("settings.system_health_lifecycle"),
                      terminalHeading: t("settings.system_health_terminal"),
                      opsHeading: t("settings.system_health_ops"),
                      loadError: t("settings.system_health_load_error"),
                      detailsToggle: t("settings.system_health_details_toggle"),
                      summaryUnavailable: t("settings.system_health_summary_unavailable"),
                      editPolicy: t("settings.system_health_edit_policy"),
                    }}
                    onEditToolsPolicy={() => {
                      setSettingsSection("system");
                      setSystemSubTab("policy");
                    }}
                  />
                )}
              </div>
            )}
            {settingsSection === "agent" && (
              <div className="settings-section-content">
                <p className="settings-doc muted">{t("settings.agent_profile_desc")}</p>
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
                          <p className="settings-doc muted settings-identity-hint">
                            {t("settings.agent_identity_constitution_hint")}
                            <InfoTip label={t("settings.agent_subtab_identity")} content={t("settings.agent_identity_constitution_hint")} />
                          </p>
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
                            <dt>{t("settings.agent_profile_formality")}</dt>
                            <dd>
                              <select aria-label={t("settings.agent_profile_formality")} className="settings-theme-select" value={agentProfile.formality} onChange={(e) => setAgentProfile((p) => ({ ...p, formality: e.target.value }))}>
                                <option value="">{t("settings.agent_profile_formality_default")}</option>
                                <option value="formal">{t("settings.formality_formal")}</option>
                                <option value="informal">{t("settings.formality_informal")}</option>
                              </select>
                              <span className="settings-doc muted">{t("settings.agent_profile_formality_hint")}</span>
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
                            <dt>{t("settings.agent_constitution_tone")}</dt>
                            <dd>
                              <input
                                type="text"
                                aria-label={t("settings.agent_constitution_tone")}
                                className="settings-input"
                                maxLength={256}
                                value={agentConstitution.tone}
                                onChange={(e) => setAgentConstitution((c) => ({ ...c, tone: e.target.value.slice(0, 256) }))}
                                placeholder={t("settings.agent_constitution_tone_placeholder")}
                              />
                            </dd>
                            <dt>{t("settings.agent_constitution_values")}</dt>
                            <dd>
                              <div className="settings-list-editor">
                                {agentConstitution.values.length === 0 ? (
                                  <p className="settings-doc muted">{t("settings.agent_list_empty")}</p>
                                ) : (
                                  <ul className="settings-string-list">
                                    {agentConstitution.values.map((line, i) => (
                                      <li key={i} className="settings-list-item">
                                        <input
                                          type="text"
                                          className="settings-input"
                                          value={line}
                                          onChange={(e) => {
                                            const next = [...agentConstitution.values];
                                            next[i] = e.target.value;
                                            setAgentConstitution((c) => ({ ...c, values: next }));
                                          }}
                                        />
                                        <button
                                          type="button"
                                          className="settings-list-item-delete"
                                          onClick={() =>
                                            setAgentConstitution((c) => ({
                                              ...c,
                                              values: c.values.filter((_, j) => j !== i),
                                            }))
                                          }
                                          aria-label={t("settings.agent_delete_line")}
                                        >
                                          ×
                                        </button>
                                      </li>
                                    ))}
                                  </ul>
                                )}
                                <button
                                  type="button"
                                  className="btn-secondary"
                                  onClick={() =>
                                    setAgentConstitution((c) => ({ ...c, values: [...c.values, ""] }))
                                  }
                                >
                                  {t("settings.agent_add_line")}
                                </button>
                              </div>
                            </dd>
                            <dt>{t("settings.agent_constitution_constraints")}</dt>
                            <dd>
                              <div className="settings-list-editor">
                                {agentConstitution.constraints.length === 0 ? (
                                  <p className="settings-doc muted">{t("settings.agent_list_empty")}</p>
                                ) : (
                                  <ul className="settings-string-list">
                                    {agentConstitution.constraints.map((line, i) => (
                                      <li key={i} className="settings-list-item">
                                        <input
                                          type="text"
                                          className="settings-input"
                                          value={line}
                                          onChange={(e) => {
                                            const next = [...agentConstitution.constraints];
                                            next[i] = e.target.value;
                                            setAgentConstitution((c) => ({ ...c, constraints: next }));
                                          }}
                                        />
                                        <button
                                          type="button"
                                          className="settings-list-item-delete"
                                          onClick={() =>
                                            setAgentConstitution((c) => ({
                                              ...c,
                                              constraints: c.constraints.filter((_, j) => j !== i),
                                            }))
                                          }
                                          aria-label={t("settings.agent_delete_line")}
                                        >
                                          ×
                                        </button>
                                      </li>
                                    ))}
                                  </ul>
                                )}
                                <button
                                  type="button"
                                  className="btn-secondary"
                                  onClick={() =>
                                    setAgentConstitution((c) => ({ ...c, constraints: [...c.constraints, ""] }))
                                  }
                                >
                                  {t("settings.agent_add_line")}
                                </button>
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
                          <dt>{t("settings.agent_profile_system_prompt")}</dt>
                          <dd>
                            <textarea aria-label={t("settings.agent_profile_system_prompt")} className="settings-textarea" rows={6} maxLength={4000} value={agentProfile.system_prompt} onChange={(e) => setAgentProfile((p) => ({ ...p, system_prompt: e.target.value.slice(0, 4000) }))} placeholder={t("settings.agent_profile_system_prompt_hint")} />
                            <span className="settings-char-count">{agentProfile.system_prompt.length} / 4000</span>
                          </dd>
                          <dt>{t("settings.agent_profile_temperature")}</dt>
                          <dd>
                            <input type="number" aria-label={t("settings.agent_profile_temperature")} className="settings-input" min={0} max={2} step={0.05} value={agentProfile.temperature} onChange={(e) => setAgentProfile((p) => ({ ...p, temperature: e.target.value }))} placeholder="0.7" />
                            <span className="settings-doc muted">{t("settings.agent_profile_temperature_hint")}</span>
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
                    <button type="button" className="refresh-btn" disabled={agentProfileSaving} onClick={async () => { setAgentProfileSaving(true); setAgentProfileError(null); try { const traits = Object.keys(agentProfile.traits_override).length ? agentProfile.traits_override : undefined; const formality = agentProfile.formality === "formal" || agentProfile.formality === "informal" ? agentProfile.formality : null; const tempRaw = agentProfile.temperature.trim(); const temperature = tempRaw ? Math.min(2, Math.max(0, parseFloat(tempRaw))) : undefined; const profileName = agentProfile.name.trim().slice(0, AGENT_PROFILE_LIMITS.name); const profileRole = agentProfile.role.trim().slice(0, AGENT_PROFILE_LIMITS.role); await invoke("post_agent_profile", { body: { name: profileName || undefined, personality: agentProfile.personality.trim().slice(0, AGENT_PROFILE_LIMITS.personality) || undefined, role: profileRole || undefined, gender: (agentProfile.gender === "male" || agentProfile.gender === "female" || agentProfile.gender === "neutral") ? agentProfile.gender : undefined, formality, avatar: agentProfile.avatar || undefined, rules: agentProfile.rules, can_do: agentProfile.can_do, cannot_do: agentProfile.cannot_do, traits_override: traits, preferred_mode: agentProfile.preferred_mode.trim() || undefined, system_prompt: agentProfile.system_prompt.trim().slice(0, 4000) || undefined, temperature: Number.isFinite(temperature) ? temperature : undefined }, port: DAEMON_PORT }); const idBody = JSON.stringify({ name: profileName || undefined, role: profileRole || undefined, tone: agentConstitution.tone.trim() || undefined, values: agentConstitution.values.map((v) => v.trim()).filter(Boolean), constraints: agentConstitution.constraints.map((v) => v.trim()).filter(Boolean) }); const idRes = await requestSystemEndpoint("POST", "/api/agent-identity", idBody); if (!idRes.ok) throw new Error(idRes.text.slice(0, 200)); } catch (err) { setAgentProfileError(String(err)); } finally { setAgentProfileSaving(false); } }}>{agentProfileSaving ? t("common.loading") : t("settings.agent_profile_save")}</button>
                  </>
                )}
              </div>
            )}
            {settingsSection === "user" && (
              <div className="settings-section-content">
                <p className="settings-doc muted">{t("settings.user_profile_desc")}</p>
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
                <div className="settings-data-subtabs" role="tablist" aria-label={t("settings.section_data")}>
                  <button
                    type="button"
                    role="tab"
                    aria-selected={dataSourcesSubTab === "rag"}
                    className={dataSourcesSubTab === "rag" ? "active" : ""}
                    onClick={() => setDataSourcesSubTab("rag")}
                  >
                    {t("settings.section_data_sub_rag")}
                  </button>
                  <button
                    type="button"
                    role="tab"
                    aria-selected={dataSourcesSubTab === "project_graph"}
                    className={dataSourcesSubTab === "project_graph" ? "active" : ""}
                    onClick={() => setDataSourcesSubTab("project_graph")}
                  >
                    {t("settings.section_data_sub_graph")}
                  </button>
                </div>
                <div className="settings-data-scroll">
                  {dataSourcesSubTab === "rag" && (
                    <UserRagPanel
                      t={t}
                      locale={locale}
                      daemonPort={DAEMON_PORT}
                      documents={userRagDocuments}
                      loading={userRagLoading}
                      error={userRagError}
                      onError={setUserRagError}
                      onRefresh={fetchUserRagDocuments}
                      fetchEndpoint={fetchSystemEndpoint}
                      readFileAsBase64={readFileAsBase64}
                    />
                  )}
                  {dataSourcesSubTab === "project_graph" && (
                    <>
                      <h3 className="settings-subtitle">{t("settings.workspace_graph_title")}</h3>
                      <p className="settings-doc muted">{t("settings.workspace_graph_desc")}</p>
                      {projectGraphSuccess && (
                        <p className="settings-doc" role="status">{projectGraphSuccess}</p>
                      )}
                      <h4 className="settings-subheading">{t("settings.project_graph_add_title")}</h4>
                      <dl className="settings-list">
                        <dt>{t("settings.project_graph_name")}</dt>
                        <dd>
                          <input
                            type="text"
                            className="settings-input"
                            aria-label={t("settings.project_graph_name")}
                            value={newProjectWsName}
                            onChange={(e) => setNewProjectWsName(e.target.value)}
                            placeholder={t("settings.project_graph_name_placeholder")}
                          />
                        </dd>
                        <dt>{t("settings.project_graph_path")}</dt>
                        <dd>
                          <input
                            type="text"
                            className="settings-input"
                            aria-label={t("settings.project_graph_path")}
                            value={newProjectWsPath}
                            onChange={(e) => setNewProjectWsPath(e.target.value)}
                            placeholder="C:\path\to\project"
                          />
                        </dd>
                      </dl>
                      <button
                        type="button"
                        className="refresh-btn"
                        disabled={newProjectWsSubmitting || !newProjectWsName.trim() || !newProjectWsPath.trim()}
                        onClick={async () => {
                          setProjectGraphError(null);
                          setProjectGraphSuccess(null);
                          setNewProjectWsSubmitting(true);
                          try {
                            const out = await invoke<{
                              files_indexed?: number;
                              nodes?: number;
                              edges?: number;
                            }>("create_project_workspace", {
                              name: newProjectWsName.trim(),
                              rootPath: newProjectWsPath.trim(),
                              rebuild: true,
                              port: DAEMON_PORT,
                            });
                            const fi = typeof out?.files_indexed === "number" ? out.files_indexed : 0;
                            const nn = typeof out?.nodes === "number" ? out.nodes : 0;
                            const ee = typeof out?.edges === "number" ? out.edges : 0;
                            setProjectGraphSuccess(
                              t("settings.project_graph_created_ok")
                                .replace("{{files}}", String(fi))
                                .replace("{{nodes}}", String(nn))
                                .replace("{{edges}}", String(ee)),
                            );
                            setNewProjectWsName("");
                            setNewProjectWsPath("");
                            await fetchProjectWorkspaces({ clearError: false });
                          } catch (err) {
                            setProjectGraphSuccess(null);
                            setProjectGraphError(String(err));
                          } finally {
                            setNewProjectWsSubmitting(false);
                          }
                        }}
                      >
                        {newProjectWsSubmitting ? t("common.loading") : t("settings.project_graph_create")}
                      </button>
                      <h4 className="settings-subheading">{t("settings.project_graph_list_title")}</h4>
                      {projectWorkspacesLoading && <p className="panel-loading" aria-busy="true">{t("common.loading")}</p>}
                      {!projectWorkspacesLoading && projectWorkspaces.length === 0 && (
                        <p className="empty-state">{t("settings.project_graph_empty")}</p>
                      )}
                      {!projectWorkspacesLoading && projectWorkspaces.length > 0 && (
                        <ul className="settings-project-ws-list" role="list">
                          {projectWorkspaces.map((w) => (
                            <li key={w.id} className="settings-project-ws-card">
                              <div className="settings-project-ws-head">
                                <strong>{w.name}</strong>
                                <span className="settings-doc-meta muted">{w.id.slice(0, 8)}…</span>
                              </div>
                              <p className="settings-doc muted settings-project-ws-path">{w.root_path}</p>
                              <p className="settings-doc">
                                {typeof w.node_count === "number" && typeof w.edge_count === "number"
                                  ? t("settings.workspace_graph_stats")
                                      .replace("{{nodes}}", String(w.node_count))
                                      .replace("{{edges}}", String(w.edge_count))
                                  : null}
                                {w.built_at != null && w.built_at !== "" && (
                                  <>
                                    {" "}
                                    {t("settings.workspace_graph_built").replace("{{time}}", String(w.built_at))}
                                  </>
                                )}
                              </p>
                              <div className="settings-row-actions">
                                <button
                                  type="button"
                                  className="refresh-btn"
                                  disabled={projectWsBusyId === w.id}
                                  onClick={async () => {
                                    setProjectWsBusyId(w.id);
                                    setProjectGraphError(null);
                                    try {
                                      const out = await invoke<{
                                        files_indexed?: number;
                                        nodes?: number;
                                        edges?: number;
                                      }>("rebuild_project_workspace", {
                                        id: w.id,
                                        port: DAEMON_PORT,
                                      });
                                      const fi = typeof out?.files_indexed === "number" ? out.files_indexed : 0;
                                      const nn = typeof out?.nodes === "number" ? out.nodes : 0;
                                      const ee = typeof out?.edges === "number" ? out.edges : 0;
                                      setProjectGraphSuccess(
                                        t("settings.project_graph_rebuild_ok")
                                          .replace("{{files}}", String(fi))
                                          .replace("{{nodes}}", String(nn))
                                          .replace("{{edges}}", String(ee)),
                                      );
                                      await fetchProjectWorkspaces({ clearError: false });
                                    } catch (err) {
                                      setProjectGraphError(String(err));
                                    } finally {
                                      setProjectWsBusyId(null);
                                    }
                                  }}
                                >
                                  {projectWsBusyId === w.id ? t("common.loading") : t("settings.workspace_graph_rebuild")}
                                </button>
                                <a
                                  className="refresh-btn settings-link-btn"
                                  href={e2eDaemonHttpUrl(`/api/workspace-graph/workspaces/${encodeURIComponent(w.id)}/html`)}
                                  target="_blank"
                                  rel="noreferrer"
                                >
                                  {t("settings.workspace_graph_open_html")}
                                </a>
                                <button
                                  type="button"
                                  className="settings-doc-delete"
                                  disabled={projectWsBusyId === w.id}
                                  onClick={async () => {
                                    if (!window.confirm(t("settings.project_graph_confirm_delete").replace("{{name}}", w.name))) return;
                                    setProjectWsBusyId(w.id);
                                    setProjectGraphError(null);
                                    try {
                                      await invoke("delete_project_workspace", { id: w.id, port: DAEMON_PORT });
                                      setProjectGraphSuccess(t("settings.project_graph_deleted"));
                                      await fetchProjectWorkspaces({ clearError: false });
                                    } catch (err) {
                                      setProjectGraphError(String(err));
                                    } finally {
                                      setProjectWsBusyId(null);
                                    }
                                  }}
                                >
                                  {t("settings.delete")}
                                </button>
                              </div>
                            </li>
                          ))}
                        </ul>
                      )}
                    </>
                  )}
                </div>
                <p className="settings-doc">{t("settings.config_note")}</p>
              </div>
            )}
          </section>
        )}
      </main>
          </div>
        </div>

        {rightSidebarOpen && (
          <aside className="sidebar-right" aria-label={tab === "chat" ? t("sidebar.threads_panel") : t("sidebar.tasks_panel")}>
            <div className="sidebar-right-header">
              <h3 className="sidebar-right-title">{tab === "chat" ? t("chat.threads_title") : t("tabs.tasks")}</h3>
              <button
                type="button"
                className="sidebar-right-close"
                onClick={() => (tab === "tasks" ? setTaskSidebarOpen(false) : setRightSidebarOpen(false))}
                aria-label={tab === "chat" ? t("sidebar.hide_tasks") : t("sidebar.hide_tasks")}
              >
                ×
              </button>
            </div>
            <div className="sidebar-right-content">
              {tab === "chat" ? (
                <>
                  <button type="button" className="refresh-btn sidebar-right-refresh" onClick={createChatThread}>
                    {t("chat.new_thread")}
                  </button>
                  <div className="sidebar-right-search-wrap">
                    <input
                      type="search"
                      className="sidebar-right-search"
                      placeholder={locale === "en" ? "Search sessions..." : "Rechercher une session..."}
                      value={chatThreadSearch}
                      onChange={(e) => setChatThreadSearch(e.target.value)}
                      aria-label={locale === "en" ? "Search sessions" : "Rechercher une session"}
                    />
                  </div>
                  {chatThreadFolders.length > 0 ? (
                    <div className="sidebar-right-filters" role="tablist" aria-label={locale === "en" ? "Session folders" : "Dossiers de session"}>
                      <button
                        type="button"
                        role="tab"
                        aria-selected={!chatThreadFolderFilter}
                        className={!chatThreadFolderFilter ? "selected" : ""}
                        onClick={() => setChatThreadFolderFilter("")}
                      >
                        {locale === "en" ? "All" : "Tous"}
                      </button>
                      {chatThreadFolders.map((f) => (
                        <button
                          key={f}
                          type="button"
                          role="tab"
                          aria-selected={chatThreadFolderFilter === f}
                          className={chatThreadFolderFilter === f ? "selected" : ""}
                          onClick={() => setChatThreadFolderFilter(f)}
                        >
                          {f}
                        </button>
                      ))}
                    </div>
                  ) : null}
                  <div className="sidebar-right-search-wrap">
                    <input
                      type="text"
                      className="sidebar-right-search"
                      placeholder={locale === "en" ? "Folder for active session" : "Dossier pour la session active"}
                      value={chatThreads.find((th) => th.id === sessionId)?.folder ?? ""}
                      onChange={(e) => setActiveThreadFolder(e.target.value)}
                      aria-label={locale === "en" ? "Session folder" : "Dossier de session"}
                    />
                  </div>
                  {filteredChatThreads.length === 0 ? (
                    <p className="empty-state">{t("chat.threads_empty")}</p>
                  ) : (
                    <ul className="sidebar-right-task-list" role="list">
                      {filteredChatThreads
                        .map((th) => {
                          const active = sessionId === th.id;
                          return (
                            <li key={th.id} className={"sidebar-right-task-card" + (active ? " selected" : "")}>
                              <div
                                className="sidebar-right-task-card-inner"
                                role="button"
                                tabIndex={0}
                                onClick={() => void selectChatThread(th.id)}
                                onKeyDown={(e) => {
                                  if (e.key === "Enter" || e.key === " ") {
                                    e.preventDefault();
                                    void selectChatThread(th.id);
                                  }
                                }}
                              >
                                <div className="sidebar-right-task-card-head">
                                  <span className="sidebar-right-task-card-title" title={chatThreadLabel(th)}>
                                    {chatThreadLabel(th)}
                                  </span>
                                </div>
                                <div className="sidebar-right-task-meta">
                                  {th.folder?.trim() ? (
                                    <span className="sidebar-right-task-folder">{th.folder.trim()}</span>
                                  ) : null}
                                  <span className="sidebar-right-task-relative">{formatRelativeTimeLabel(th.updatedAt, locale)}</span>
                                </div>
                              </div>
                              <button
                                type="button"
                                className="sidebar-right-task-view-btn"
                                onClick={(e) => {
                                  e.stopPropagation();
                                  void deleteChatThread(th.id);
                                }}
                              >
                                {t("chat.delete_thread")}
                              </button>
                            </li>
                          );
                        })}
                    </ul>
                  )}
                </>
              ) : (
                <>
              <button
                type="button"
                className="refresh-btn sidebar-right-refresh"
                onClick={() => void fetchTasksList()}
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
                  {taskTreeData.branchIds.size > 0 && (
                    <div className="task-tree-toolbar task-tree-toolbar-sidebar" role="group" aria-label={t("tasks.tree_actions")}>
                      <button
                        type="button"
                        className="task-tree-toolbar-btn"
                        onClick={() => setAllTaskBranchesCollapsed(false)}
                      >
                        {t("tasks.expand_all")}
                      </button>
                      <button
                        type="button"
                        className="task-tree-toolbar-btn"
                        onClick={() => setAllTaskBranchesCollapsed(true)}
                      >
                        {t("tasks.collapse_all")}
                      </button>
                    </div>
                  )}
                </>
              )}
              {tasksLoading && (
                <p className="panel-loading" aria-busy="true">{t("common.loading")}</p>
              )}
              {!tasksLoading && tasksList.length === 0 && (
                <p className="empty-state">{t("tasks.empty")}</p>
              )}
              {!tasksLoading && tasksList.length > 0 && taskTreeRows.length === 0 && (
                <p className="empty-state">{t("tasks.no_match_filter")}</p>
              )}
              {tab === "tasks" && eventTriggers.length > 0 && (
                <div className="sidebar-right-triggers">
                  <h4 className="sidebar-right-triggers-title">{t("tasks.triggers_heading")}</h4>
                  <ul className="sidebar-right-triggers-list" role="list">
                    {eventTriggers.map((tr) => (
                      <li key={tr.id} className={"sidebar-right-trigger-item" + (tr.enabled ? "" : " disabled")}>
                        <span className="sidebar-right-trigger-name">{tr.name}</span>
                        <span className="sidebar-right-trigger-type">{t("tasks.trigger_type_" + tr.trigger_type)}</span>
                      </li>
                    ))}
                  </ul>
                </div>
              )}
              {!tasksLoading && taskTreeRows.length > 0 && (
                <ul className="sidebar-right-task-list" role="list">
                  {taskTreeRows.map((row) => {
                    const task = row.task;
                    const isSelected = tasksList[tasksSelected]?.id === task.id;
                    const isAncestor = !isSelected && selectedTaskAncestorIds.has(task.id);
                    const isActiveRoot = selectedTaskRootId === task.id;
                    const runningChip = task.status === "running" ? runningTaskChips[task.id] : undefined;
                    const hierarchyLabel = row.depth > 0 ? t("tasks.subtask_badge") : t("tasks.root_badge");
                    const childCountLabel = t("tasks.children_count").replace("{{count}}", String(row.visibleChildCount));
                    const createdLabel = task.created_at ? (() => {
                      try {
                        const d = new Date(task.created_at);
                        return d.toLocaleString(undefined, { dateStyle: "short", timeStyle: "short" });
                      } catch {
                        return task.created_at;
                      }
                    })() : null;
                    return (
                      <li key={task.id} className={"sidebar-right-task-card" + (isSelected ? " selected" : "") + (isAncestor ? " sidebar-right-task-card-ancestor" : "") + (isActiveRoot ? " sidebar-right-task-card-active-root" : "") + (row.depth > 0 ? " sidebar-right-task-card-child" : " sidebar-right-task-card-root") }>
                        <div
                          className={"sidebar-right-task-card-inner task-tree-row" + (row.depth > 0 ? " task-tree-row-child" : " task-tree-row-root")}
                          role="button"
                          tabIndex={0}
                          onClick={() => { setTasksSelected(tasksList.findIndex((x) => x.id === task.id)); setTab("tasks"); }}
                          onKeyDown={(e) => {
                            if (e.key === "Enter" || e.key === " ") {
                              e.preventDefault();
                              setTasksSelected(tasksList.findIndex((x) => x.id === task.id));
                              setTab("tasks");
                            }
                            if (e.key === "ArrowRight" && row.hasChildren && row.isCollapsed) {
                              e.preventDefault();
                              toggleTaskBranch(task.id);
                            }
                            if (e.key === "ArrowLeft" && row.hasChildren && !row.isCollapsed) {
                              e.preventDefault();
                              toggleTaskBranch(task.id);
                            }
                          }}
                          style={{ marginLeft: `${row.depth * 1.1}rem` }}
                        >
                          <div className="sidebar-right-task-card-head">
                            <div className="sidebar-right-task-card-heading">
                              <div className="task-tree-title-row">
                                {row.hasChildren ? (
                                  <button
                                    type="button"
                                    className="task-tree-toggle"
                                    aria-label={(row.isCollapsed ? t("tasks.expand_branch") : t("tasks.collapse_branch")) + ": " + taskDisplayLabel(task)}
                                    aria-expanded={!row.isCollapsed}
                                    onClick={(e) => {
                                      e.stopPropagation();
                                      toggleTaskBranch(task.id);
                                    }}
                                  >
                                    <span aria-hidden>{row.isCollapsed ? "▶" : "▼"}</span>
                                  </button>
                                ) : (
                                  <span className="task-tree-toggle-spacer" aria-hidden>
                                    {row.depth > 0 ? "•" : ""}
                                  </span>
                                )}
                                <span className="sidebar-right-task-card-title" title={taskDisplayLabel(task)}>
                                  {taskDisplayLabel(task)}
                                </span>
                              </div>
                              <div className="task-tree-meta-row">
                                <span className={"task-tree-kind-badge " + (row.depth > 0 ? "task-tree-kind-badge-child" : "task-tree-kind-badge-root")}>
                                  {hierarchyLabel}
                                </span>
                                {row.hasChildren && <span className="task-tree-child-count">{childCountLabel}</span>}
                              </div>
                            </div>
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
                          {isSimpleMode && runningChip?.message && (
                            <p className="sidebar-right-task-snippet">{trimPreview(runningChip.message, 96)}</p>
                          )}
                          <div className="sidebar-right-task-meta">
                            {!isSimpleMode && <span className="sidebar-right-task-id">{t("tasks.task_id_prefix")}{task.id.slice(-8)}</span>}
                            {createdLabel && <span className="sidebar-right-task-created">{createdLabel}</span>}
                            {task.created_at && <span className="sidebar-right-task-relative">{formatRelativeTimeLabel(task.created_at, locale)}</span>}
                            {task.assigned_agent && <span className="sidebar-right-task-agent agent-kind-pill" data-agent-kind={classifyAgentKind(task.assigned_agent)}>{task.assigned_agent}</span>}
                          </div>
                          <button
                            type="button"
                            className="sidebar-right-task-view-btn"
                            onClick={(e) => { e.stopPropagation(); setTasksSelected(tasksList.findIndex((x) => x.id === task.id)); setTab("tasks"); }}
                          >
                            {isSimpleMode ? t("tasks.open") : t("tasks.view_task")}
                          </button>
                        </div>
                      </li>
                    );
                  })}
                </ul>
              )}
                </>
              )}
            </div>
          </aside>
        )}
      </div>
      <CreateTaskDialog
        open={createTaskDialogOpen}
        onClose={() => setCreateTaskDialogOpen(false)}
        sessionId={sessionId ?? ""}
        t={t}
        eventTriggersEnabled
        onImmediateCreated={(taskId) => {
          taskIdToSessionIdRef.current[taskId] = sessionId ?? "";
          persistTaskSession(taskId, sessionId ?? "");
          setRunningTaskChips((prev) => ({ ...prev, [taskId]: { pct: 0, message: "en cours…" } }));
          setRunningTaskEvents((prev) => (prev[taskId] ? prev : { ...prev, [taskId]: [] }));
          trackTaskUntilDone(taskId);
          void fetchTasksList({ selectTaskId: taskId });
          setTaskPanelSections((prev) => ({ ...prev, events: true }));
        }}
        onScheduleCreated={() => {
          void fetchEventTriggers();
        }}
        onTriggerCreated={() => {
          void fetchEventTriggers();
        }}
      />
    </div>
  );
}

export default App;
