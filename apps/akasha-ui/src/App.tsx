import { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

const DAEMON_PORT = 3876;

type Tab = "chat" | "router" | "settings" | "docs" | "tasks" | "calendar" | "memory";

/** French label for Activity event types (delegation, progress, etc.). */
function eventTypeLabel(typ: string): string {
  const labels: Record<string, string> = {
    user_request_received: "Demande reçue",
    acknowledgment_sent: "Accusé de réception envoyé",
    task_created: "Tâche créée",
    task_started: "Tâche démarrée",
    task_decomposed: "Tâche décomposée (délégation à des sous-agents)",
    sub_agent_spawned: "Délégué à un agent spécialisé",
    progress_update: "Progression",
    task_progress_updated: "Progression mise à jour",
    task_step_completed: "Étape terminée",
    task_completed: "Tâche terminée",
    task_failed: "Tâche en échec",
    task_run_created: "Run planifié créé",
    task_run_scheduled: "Run planifié",
    schedule_created: "Récurrence créée",
    schedule_updated: "Récurrence mise à jour",
    schedule_deleted: "Récurrence supprimée",
  };
  return labels[typ] ?? typ;
}

/** Format duration in seconds as "X min Y s" or "Y s". */
function formatDurationSec(sec: number): string {
  const total = Math.round(sec);
  if (total < 60) return `${total} s`;
  const m = Math.floor(total / 60);
  const s = total % 60;
  return s > 0 ? `${m} min ${s} s` : `${m} min`;
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
  const [tab, setTab] = useState<Tab>("chat");
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
  const [schedules, setSchedules] = useState<Array<{ id: string; name: string; enabled: boolean; interval_seconds?: number }>>([]);
  const [taskRuns, setTaskRuns] = useState<Array<{
    id: string;
    schedule_id?: string;
    task_id: string;
    status: string;
    planned_for: string;
    started_at?: string;
    ended_at?: string;
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
  const [calendarRunsCollapsed, setCalendarRunsCollapsed] = useState(false);
  const [memoryShortTerm, setMemoryShortTerm] = useState<Array<{ role: string; content: string }>>([]);
  const [memoryLongTerm, setMemoryLongTerm] = useState<Array<{ content: string; created_at: string; source: string }>>([]);
  const [memoryLongTermAvailable, setMemoryLongTermAvailable] = useState(false);
  const [memoryLoading, setMemoryLoading] = useState(false);
  const [memoryError, setMemoryError] = useState<string | null>(null);
  const [scheduleReports, setScheduleReports] = useState<Array<{ schedule_name: string; message: string; ended_at?: string }>>([]);
  const [sessionId, setSessionId] = useState<string | null>(null);
  const chatEndRef = useRef<HTMLDivElement>(null);
  const chatInputRef = useRef<HTMLInputElement>(null);

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

  // Scroll chat to last message and keep focus on input
  useEffect(() => {
    chatEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages, loading]);
  useEffect(() => {
    if (tab === "chat") chatInputRef.current?.focus();
  }, [tab]);

  const fetchRouterMetrics = useCallback(async () => {
    setRouterLoading(true);
    setRouterError(null);
    try {
      const data = await invoke<RouterMetrics>("get_router_metrics", {
        port: DAEMON_PORT,
      });
      setRouterMetrics(data as RouterMetrics);
    } catch (e) {
      setRouterError(String(e));
      setRouterMetrics(null);
    } finally {
      setRouterLoading(false);
    }
  }, []);

  useEffect(() => {
    if (tab === "router") fetchRouterMetrics();
  }, [tab, fetchRouterMetrics]);

  const fetchDocs = useCallback(async () => {
    setDocLoading(true);
    setDocError(null);
    try {
      const content = await invoke<string>("get_docs", { port: DAEMON_PORT });
      setDocContent(content);
    } catch (e) {
      setDocError(String(e));
      setDocContent(null);
    } finally {
      setDocLoading(false);
    }
  }, []);

  useEffect(() => {
    if (tab === "docs") fetchDocs();
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
        invoke<{ task_runs?: Array<{ id?: string; schedule_id?: string; task_id?: string; status?: string; planned_for?: string; started_at?: string; ended_at?: string }> }>("get_task_runs", { port: DAEMON_PORT }),
      ]);
      setSchedules((schedData?.schedules ?? []).map((s) => ({ id: s.id ?? "", name: s.name ?? "", enabled: s.enabled ?? false, interval_seconds: s.interval_seconds })));
      setTaskRuns((runsData?.task_runs ?? []).map((r) => ({
        id: r.id ?? "",
        schedule_id: r.schedule_id,
        task_id: r.task_id ?? "",
        status: r.status ?? "?",
        planned_for: r.planned_for ?? "",
        started_at: r.started_at,
        ended_at: r.ended_at,
      })));
    } catch {
      setSchedules([]);
      setTaskRuns([]);
    } finally {
      setCalendarLoading(false);
    }
  }, []);

  useEffect(() => {
    if (tab === "tasks") fetchTasksList();
  }, [tab, fetchTasksList]);

  useEffect(() => {
    const task = tasksList[tasksSelected];
    if (task?.id) fetchTasksEvents(task.id);
    else setTasksEvents([]);
  }, [tasksList, tasksSelected, fetchTasksEvents]);

  useEffect(() => {
    if (tab === "calendar") fetchCalendar();
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
        invoke<{ entries?: Array<{ content: string; created_at: string; source: string }>; long_term_available?: boolean }>("get_memory_long_term", {
          limit: 50,
          port: DAEMON_PORT,
        }),
      ]);
      setMemoryShortTerm(shortRes?.turns ?? []);
      setMemoryLongTerm(longRes?.entries ?? []);
      setMemoryLongTermAvailable(longRes?.long_term_available ?? false);
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
    if (tab === "memory") fetchMemory();
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
/task create "msg" — créer une tâche (envoie le message au daemon)
/schedule create NOM INTERVAL_SEC "description" — créer une récurrence
/schedule delete SCHEDULE_ID — supprimer une récurrence
/status           — état du daemon
/stop TASK_ID     — annuler une tâche (en cours ou en attente)
/cancel TASK_ID   — idem que /stop
/doctor           — diagnostic (daemon, ollama, vault, spec)
/advice           — conseil diagnostic (RAG + modèle)
/metrics          — métriques du routeur LLM
/models           — liste des modèles (tous les providers)
/config list      — variables (akasha.env)
/config get KEY   — valeur d'une variable
/config set K V   — définir variable
/vault list       — clés du vault (noms uniquement)
/plugins          — liste des plugins
/reload           — recharger les plugins
/restart          — redémarrer le daemon
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
    if (cmd === "plugins") {
      const list = await invoke<Array<{ id?: string; name?: string; version?: string }>>("get_plugins", { port });
      if (!list?.length) return "Aucun plugin installé.";
      return list.map((p) => `${p.id ?? "?"} — ${p.name ?? "?"} (${p.version ?? "?"})`).join("\n");
    }
    if (cmd === "reload") {
      await invoke("reload_plugins", { port });
      return "Plugins rechargés.";
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
    if (cmd === "models") {
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

  const handleSend = async () => {
    if (!message.trim() || loading) return;

    const userMessage = message.trim();
    setMessages((prev) => [...prev, { role: "user", text: userMessage }]);
    setMessage("");
    chatInputRef.current?.focus();

    if (userMessage.startsWith("/")) {
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
    try {
      const ack = await invoke<{ task_id: string; session_id: string; message: string }>("send_message_ack", {
        message: userMessage,
        session_id: sessionId,
        port: DAEMON_PORT,
      });
      if (ack?.session_id) setSessionId(ack.session_id);
      const ackText = ack?.message ?? "Je prends en compte votre demande.";
      setMessages((prev) => [...prev, { role: "assistant", text: ackText + (ack?.task_id ? " Tu peux suivre l'avancement dans l'onglet Tâches." : "") }]);
      if (ack?.task_id) {
        setRunningTaskChips((prev) => ({ ...prev, [ack.task_id]: { pct: 0, message: "en cours…" } }));
        fetchTasksList();
        const taskId = ack.task_id;
        const pollUntilDone = async () => {
          const maxWait = 600;
          for (let i = 0; i < maxWait; i++) {
            await new Promise((r) => setTimeout(r, 1500));
            try {
              const raw = await invoke<string>("get_task_status", { taskId, port: DAEMON_PORT });
              const status = JSON.parse(raw) as { status?: string; progress?: Array<{ progress_pct?: number; message?: string }> };
              const pct = status?.progress?.slice(-1)[0]?.progress_pct ?? 0;
              const msg = status?.progress?.slice(-1)[0]?.message ?? "";
              setRunningTaskChips((prev) => (prev[taskId] !== undefined ? { ...prev, [taskId]: { pct, message: msg } } : prev));
              if (status?.status === "completed") {
                setRunningTaskChips((prev) => {
                  const next = { ...prev };
                  delete next[taskId];
                  return next;
                });
                const finalMsg = status?.progress?.slice(-1)[0]?.message ?? "Terminé.";
                setMessages((prev) => [...prev, { role: "assistant", text: finalMsg }]);
                return;
              }
              if (status?.status === "failed") {
                setRunningTaskChips((prev) => {
                  const next = { ...prev };
                  delete next[taskId];
                  return next;
                });
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
          setMessages((prev) => [...prev, { role: "assistant", text: "Délai dépassé. Consultez l'onglet Tâches." }]);
        };
        pollUntilDone();
      }
    } catch (err) {
      setMessages((prev) => [...prev, { role: "assistant", text: `Erreur : ${String(err)}`, error: true }]);
    }
    chatInputRef.current?.focus();
  };

  return (
    <div className="app">
      <header className="header">
        <h1 className="logo">Akasha</h1>
        <p className="tagline">Local-first AI assistant</p>
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
        <nav className="tabs" role="tablist" aria-label="Sections">
          <button
            role="tab"
            aria-selected={tab === "chat"}
            aria-controls="panel-chat"
            id="tab-chat"
            className={tab === "chat" ? "active" : ""}
            onClick={() => setTab("chat")}
          >
            Chat
          </button>
          <button
            role="tab"
            aria-selected={tab === "router"}
            aria-controls="panel-router"
            id="tab-router"
            className={tab === "router" ? "active" : ""}
            onClick={() => setTab("router")}
          >
            Routeur
          </button>
          <button
            role="tab"
            aria-selected={tab === "docs"}
            aria-controls="panel-docs"
            id="tab-docs"
            className={tab === "docs" ? "active" : ""}
            onClick={() => setTab("docs")}
          >
            Documentation
          </button>
          <button
            role="tab"
            aria-selected={tab === "tasks"}
            aria-controls="panel-tasks"
            id="tab-tasks"
            className={tab === "tasks" ? "active" : ""}
            onClick={() => setTab("tasks")}
          >
            Tâches
          </button>
          <button
            role="tab"
            aria-selected={tab === "calendar"}
            aria-controls="panel-calendar"
            id="tab-calendar"
            className={tab === "calendar" ? "active" : ""}
            onClick={() => setTab("calendar")}
          >
            Calendrier
          </button>
          <button
            role="tab"
            aria-selected={tab === "memory"}
            aria-controls="panel-memory"
            id="tab-memory"
            className={tab === "memory" ? "active" : ""}
            onClick={() => setTab("memory")}
          >
            Mémoire
          </button>
          <button
            role="tab"
            aria-selected={tab === "settings"}
            aria-controls="panel-settings"
            id="tab-settings"
            className={tab === "settings" ? "active" : ""}
            onClick={() => setTab("settings")}
          >
            Paramètres
          </button>
        </nav>
      </header>

      <main className="main">
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
                      <div className="text" style={{ whiteSpace: "pre-wrap" }}>
                        <strong>« {r.schedule_name} »</strong> — {r.message}
                      </div>
                    </div>
                  ))}
                  {messages.map((m, i) => (
                    <div
                      key={i}
                      className={`message ${m.role} ${m.error ? "error" : ""}`}
                    >
                      <span className="role" aria-hidden>
                        {m.role === "user" ? "Vous" : m.role === "system" ? "Système" : "Akasha"}
                      </span>
                      <div className="text" style={{ whiteSpace: "pre-wrap" }}>
                        {m.text}
                      </div>
                    </div>
                  ))}
                </>
              )}
              {Object.keys(runningTaskChips).length > 0 && (
                <div className="chat-chips" role="status">
                  {Object.entries(runningTaskChips).map(([tid, { pct, message }]) => (
                    <span key={tid} className="task-chip">
                      Task #{tid.slice(-8)} {pct != null ? `(${pct}%)` : ""} {message ?? "en cours"}
                    </span>
                  ))}
                </div>
              )}
              <div ref={chatEndRef} aria-hidden />
            </div>
            <div className="input-area">
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
                disabled={loading || !message.trim()}
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

        {tab === "router" && (
          <section
            id="panel-router"
            role="tabpanel"
            aria-labelledby="tab-router"
            className="panel router-panel"
          >
            <h2 className="panel-title">Métriques du routeur LLM</h2>
            {routerLoading && (
              <p className="loading-inline" aria-busy="true">
                Chargement…
              </p>
            )}
            {routerError && (
              <div className="error-banner" role="alert">
                {routerError}
              </div>
            )}
            {!routerLoading && !routerError && routerMetrics && (
              <>
                <button
                  type="button"
                  className="refresh-btn"
                  onClick={fetchRouterMetrics}
                  aria-label="Rafraîchir les métriques"
                >
                  Rafraîchir
                </button>
                {Object.keys(routerMetrics).length === 0 ? (
                  <p className="empty-state">
                    Aucune requête enregistrée. Envoyez un message dans le Chat
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
            <h2 className="panel-title">Documentation utilisateur</h2>
            {docLoading && (
              <p className="loading-inline" aria-busy="true">
                Chargement…
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
                  <ReactMarkdown remarkPlugins={[remarkGfm]}>
                    {docContent}
                  </ReactMarkdown>
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
            <h2 className="panel-title">Tâches (Task Center)</h2>
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
              <p className="loading-inline" aria-busy="true">
                Chargement…
              </p>
            )}
            {!tasksLoading && (
              <div className="activity-panel-body">
                <div className="activity-tasks-block">
                  <h3>Liste des tâches</h3>
                  {tasksList.length === 0 ? (
                    <p className="empty-state">Aucune tâche. Envoyez un message dans le Chat.</p>
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
                      {tasksList.length > 0 ? "Aucun événement pour cette tâche." : "Sélectionnez une tâche."}
                    </p>
                  ) : (
                    <ul className="activity-events-list" role="list">
                      {tasksEvents.map((e, i) => (
                        <li key={i}>
                          <strong>{eventTypeLabel(e.event_type)}</strong> @ {e.at}
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
            <h2 className="panel-title">Calendrier (récurrences)</h2>
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
                Chargement…
              </p>
            )}
            {!calendarLoading && (
              <>
                <h3>Récurrences</h3>
                {schedules.length === 0 ? (
                  <p className="empty-state">Aucune récurrence. Créez-en via l'API ou un outil.</p>
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
                <h3 className="calendar-runs-header">
                  Lancements récents
                  {taskRuns.length > 0 && (
                    <button
                      type="button"
                      className="calendar-runs-toggle"
                      onClick={() => setCalendarRunsCollapsed((c) => !c)}
                      aria-expanded={!calendarRunsCollapsed}
                    >
                      {calendarRunsCollapsed ? "Déplier" : "Plier"}
                    </button>
                  )}
                </h3>
                {taskRuns.length === 0 ? (
                  <p className="empty-state">Aucun run.</p>
                ) : calendarRunsCollapsed ? (
                  <p className="empty-state">Liste repliée ({taskRuns.length} run(s)). Cliquez sur « Déplier » pour afficher.</p>
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
                                    <span className="run-id">{r.id.slice(-8)}</span> {r.status}
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
                              return parent ? <p><strong>Récurrence parente:</strong> {parent.name || parent.id.slice(0, 8)}</p> : null;
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
                                  <div className="task-detail-reply-content">{last}</div>
                                </div>
                              ) : null;
                            })()}
                            {calendarTaskDetail.progress && calendarTaskDetail.progress.length > 0 && (
                              <div className="task-detail-progress">
                                <strong>Progression / étapes:</strong>
                                <p className="task-detail-progress-hint">
                                  Les lignes « État » sont des étapes intermédiaires ; le pourcentage indique l’avancement.
                                </p>
                                <ul>
                                  {calendarTaskDetail.progress.map((p, i) => (
                                    <li key={i}>
                                      {p.progress_pct != null && p.progress_pct > 0
                                        ? `${p.progress_pct}% — `
                                        : "État: "}
                                      {p.message ?? ""}
                                    </li>
                                  ))}
                                </ul>
                              </div>
                            )}
                          </>
                        ) : (
                          <p className="loading-inline">Chargement…</p>
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
                                <strong>Demande envoyée aux agents à chaque itération:</strong>
                                <div className="task-detail-reply-content">
                                  {scheduleDetail.channel_context ?? scheduleDetail.description}
                                </div>
                              </div>
                            ) : (
                              <p className="muted">Aucune demande configurée pour cette récurrence (channel_context et description vides).</p>
                            )}
                            {scheduleDetail.rrule && (
                              <p className="schedule-rrule"><strong>Règle:</strong> <code>{scheduleDetail.rrule}</code></p>
                            )}
                          </>
                        ) : (
                          <p className="loading-inline">Chargement…</p>
                        )}
                      </div>
                    </div>
                  </div>
                )}
              </>
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
            <h2 className="panel-title">Mémoire</h2>
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
              <p className="loading-inline" aria-busy="true">
                Chargement…
              </p>
            )}
            {!memoryLoading && !memoryError && (
              <>
                <h3 className="memory-section-title">Court terme (session)</h3>
                <p className="muted">
                  Derniers échanges de la session courante (utilisée par l’orchestrateur pour le contexte).
                </p>
                {memoryShortTerm.length === 0 ? (
                  <p className="empty-state">Aucun tour en mémoire court terme.</p>
                ) : (
                  <ul className="memory-turns-list">
                    {memoryShortTerm.map((t, i) => (
                      <li key={i} className={`memory-turn memory-turn-${t.role}`}>
                        <span className="memory-turn-role">{t.role}</span>
                        <div className="memory-turn-content">{t.content}</div>
                      </li>
                    ))}
                  </ul>
                )}
                <h3 className="memory-section-title">Long terme</h3>
                {!memoryLongTermAvailable ? (
                  <p className="muted">Mémoire long terme non disponible (embeddings non configurés ou désactivés).</p>
                ) : memoryLongTerm.length === 0 ? (
                  <p className="empty-state">Aucune entrée en mémoire long terme.</p>
                ) : (
                  <ul className="memory-long-term-list">
                    {memoryLongTerm.map((e, i) => (
                      <li key={i} className="memory-long-term-item">
                        <div className="memory-long-term-content">{e.content}</div>
                        <div className="memory-long-term-meta">
                          {e.created_at} {e.source ? ` · ${e.source}` : ""}
                        </div>
                      </li>
                    ))}
                  </ul>
                )}
              </>
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
            <h2 className="panel-title">Paramètres</h2>
            <dl className="settings-list">
              <dt>Port du daemon</dt>
              <dd>
                <code>{DAEMON_PORT}</code> (défaut)
              </dd>
              <dt>Répertoire de données</dt>
              <dd>
                <code>%LOCALAPPDATA%\akasha</code> (Windows) ou{" "}
                <code>~/.local/share/akasha</code> (Linux/macOS)
              </dd>
            </dl>
            <p className="settings-doc">
              Configuration : variables d’environnement <code>AKASHA_*</code>,{" "}
              <code>OLLAMA_HOST</code>. Voir l’onglet Documentation pour le guide complet.
            </p>
          </section>
        )}
      </main>
    </div>
  );
}

export default App;
