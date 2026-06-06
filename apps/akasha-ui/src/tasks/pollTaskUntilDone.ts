import { invoke } from "@tauri-apps/api/core";
import type { Dispatch, MutableRefObject, SetStateAction } from "react";
import { resolveTaskUsage, type ModelUsageStats } from "../modelUsage";
import { isTaskTerminalStatus, mergeTaskEvents, normalizeTaskStatus, type TaskEventRow } from "../taskEvents";

type TaskChip = { pct?: number; message?: string };

type ChatMapVisual = {
  kind: "map";
  title?: string;
  summary?: string;
  geometryKind?: "great_circle_estimate" | "road_network";
  points: Array<{ x: number; y: number }>;
  distanceM?: number;
  durationS?: number;
};

type ChatMessageRow = {
  role: string;
  text: string;
  taskId?: string;
  mapVisual?: ChatMapVisual;
  usage?: unknown;
  error?: boolean;
  streaming?: boolean;
};

export type PollTaskUntilDoneDeps = {
  daemonPort: number;
  sessionId: string;
  sessionIdRef: MutableRefObject<string>;
  taskIdToSessionIdRef: MutableRefObject<Record<string, string>>;
  ackTextByTaskRef: MutableRefObject<Record<string, string>>;
  chatMapByTaskIdRef: MutableRefObject<Record<string, ChatMapVisual>>;
  humanInputAutoOpenedRef: MutableRefObject<Set<string>>;
  replyWithTtsRef: MutableRefObject<boolean>;
  selectedTaskIdForTodosRef: MutableRefObject<string | null>;
  fetchTasksEventsRef: MutableRefObject<(taskId: string) => Promise<void>>;
  chatInputRef: MutableRefObject<HTMLTextAreaElement | null>;
  akashaSessionIdKey: string;
  normalizeTaskEventsInvokeResponse: (data: unknown) => Array<{
    event_type?: string;
    payload?: unknown;
    at?: string;
    task_id?: string;
  }>;
  extractChatMapVisualFromTaskEvents: (events: TaskEventRow[]) => ChatMapVisual | null;
  extractChatMapVisualFromAssistantText: (text: string) => ChatMapVisual | null;
  findLastChatAssistantIndex: (messages: ChatMessageRow[], taskId: string) => number;
  chatMapMessageCacheKey: (sessionId: string, message: string) => string;
  applyChatStreamProgress: (taskId: string, msg: string) => void;
  fetchTasksList: (options?: { silent?: boolean; selectTaskId?: string }) => Promise<void>;
  enrichUsageWithPricing: (usage: ModelUsageStats | null | undefined) => ModelUsageStats | undefined;
  setRunningTaskChips: Dispatch<SetStateAction<Record<string, TaskChip>>>;
  setRunningTaskEvents: Dispatch<SetStateAction<Record<string, TaskEventRow[]>>>;
  setTasksEvents: Dispatch<SetStateAction<TaskEventRow[]>>;
  setPendingHumanInput: Dispatch<
    SetStateAction<Record<string, { question: string; context: string; choices?: string[] }>>
  >;
  setHumanInputModalTaskId: Dispatch<SetStateAction<string | null>>;
  setChatMapByTaskId: Dispatch<SetStateAction<Record<string, ChatMapVisual>>>;
  setMessages: Dispatch<SetStateAction<ChatMessageRow[]>>;
  voiceTtsConfigured: boolean;
};

/** Progress messages emitted before real LLM/tool output — not shown as the final chat reply. */
function isStartupProgressMessage(msg: string): boolean {
  const m = msg.trim();
  if (!m) return true;
  return (
    m.startsWith("Analyzing your request") ||
    m.startsWith("Still spinning") ||
    m.startsWith("Still working")
  );
}

export async function pollTaskUntilDone(taskId: string, deps: PollTaskUntilDoneDeps): Promise<void> {
  const maxWait = 600;
  const MIN_INTERVAL = 1500;
  const MAX_INTERVAL = 5000;
  let pollIntervalMs = MIN_INTERVAL;
  let ticksWithoutChange = 0;
  let lastStatus = "";
  let lastMsg = "";
  let stallHintShown = false;

  for (let i = 0; i < maxWait; i++) {
    await new Promise((r) => setTimeout(r, pollIntervalMs));
    try {
      const [raw, eventsPayloadRaw, humanInputData] = await Promise.all([
        invoke<string>("get_task_status", { taskId, port: deps.daemonPort }),
        invoke<unknown>("get_task_events", { taskId, port: deps.daemonPort }).catch(() => null),
        invoke<{ question?: string; context?: string; choices?: string[] }>("get_task_human_input", {
          taskId,
          port: deps.daemonPort,
        }).catch(() => null),
      ]);
      const eventsData = { events: deps.normalizeTaskEventsInvokeResponse(eventsPayloadRaw ?? {}) };
      const status = JSON.parse(raw) as {
        status?: string;
        progress?: Array<{ progress_pct?: number; message?: string }>;
        last_turn_tokens_in?: unknown;
        last_turn_tokens_out?: unknown;
        last_turn_cost_usd?: unknown;
        last_turn_latency_ms?: unknown;
        last_turn_model_used?: unknown;
      };
      const pct = status?.progress?.slice(-1)[0]?.progress_pct ?? 0;
      const msg = status?.progress?.slice(-1)[0]?.message ?? "";
      const currentStatus = normalizeTaskStatus(status?.status);
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
      deps.setRunningTaskChips((prev) => {
        if (prev[taskId] === undefined) return prev;
        const cur = prev[taskId]!;
        if (cur.pct === pct && cur.message === msg) return prev;
        return { ...prev, [taskId]: { pct, message: msg } };
      });
      const events = (eventsData?.events ?? []).map((e) => ({
        event_type: e.event_type ?? "?",
        payload: e.payload,
        at: e.at ?? "",
        task_id: e.task_id,
      }));
      deps.setRunningTaskEvents((prev) => {
        if (prev[taskId] === undefined) return prev;
        const merged = mergeTaskEvents(prev[taskId]!, events);
        try {
          if (JSON.stringify(prev[taskId]) === JSON.stringify(merged)) return prev;
        } catch {
          /* ignore */
        }
        return { ...prev, [taskId]: merged };
      });
      if (deps.selectedTaskIdForTodosRef.current === taskId) {
        deps.setTasksEvents((prev) => mergeTaskEvents(prev, events));
      }
      const usageSnapshot = deps.enrichUsageWithPricing(resolveTaskUsage(status, events));
      if (usageSnapshot && deps.taskIdToSessionIdRef.current[taskId] === deps.sessionIdRef.current) {
        deps.setMessages((prev) => {
          const idx = deps.findLastChatAssistantIndex(prev, taskId);
          if (idx < 0) return prev;
          if (prev[idx]?.usage === usageSnapshot) return prev;
          const next = [...prev];
          next[idx] = { ...next[idx]!, usage: usageSnapshot };
          return next;
        });
      }
      const chatMapVis =
        deps.extractChatMapVisualFromTaskEvents(events) ?? deps.extractChatMapVisualFromAssistantText(msg);
      if (chatMapVis) {
        deps.chatMapByTaskIdRef.current[taskId] = chatMapVis;
        deps.setChatMapByTaskId((prev) => (prev[taskId] === chatMapVis ? prev : { ...prev, [taskId]: chatMapVis }));
      }
      const taskForActiveChat = deps.taskIdToSessionIdRef.current[taskId] === deps.sessionIdRef.current;
      if (chatMapVis && taskForActiveChat) {
        deps.setMessages((prev) => {
          const idx = deps.findLastChatAssistantIndex(prev, taskId);
          if (idx < 0) return prev;
          if (prev[idx]?.mapVisual === chatMapVis) return prev;
          const next = [...prev];
          next[idx] = { ...next[idx]!, mapVisual: chatMapVis };
          return next;
        });
      }
      if (humanInputData?.question) {
        deps.setPendingHumanInput((prev) => ({
          ...prev,
          [taskId]: {
            question: humanInputData.question ?? "",
            context: humanInputData.context ?? "",
            choices: humanInputData.choices,
          },
        }));
        if (!deps.humanInputAutoOpenedRef.current.has(taskId)) {
          deps.humanInputAutoOpenedRef.current.add(taskId);
          deps.setHumanInputModalTaskId(taskId);
        }
      } else {
        deps.setPendingHumanInput((prev) => {
          const next = { ...prev };
          delete next[taskId];
          return next;
        });
        deps.humanInputAutoOpenedRef.current.delete(taskId);
      }
      if (!isTaskTerminalStatus(currentStatus) && msg && !isStartupProgressMessage(msg)) {
        deps.applyChatStreamProgress(taskId, msg);
      } else if (
        currentStatus === "running" &&
        taskForActiveChat &&
        isStartupProgressMessage(msg) &&
        ticksWithoutChange >= 12 &&
        !stallHintShown
      ) {
        stallHintShown = true;
        deps.applyChatStreamProgress(
          taskId,
          "Tâche en cours… (assemblage du contexte ou appel au modèle — patientez quelques instants).",
        );
      }
      if (currentStatus === "completed") {
        deps.setRunningTaskChips((prev) => {
          const next = { ...prev };
          delete next[taskId];
          return next;
        });
        deps.setRunningTaskEvents((prev) => {
          if (prev[taskId] === undefined) return prev;
          const next = { ...prev };
          delete next[taskId];
          return next;
        });
        void deps.fetchTasksList({ silent: true });
        deps.setPendingHumanInput((prev) => {
          const next = { ...prev };
          delete next[taskId];
          return next;
        });
        deps.humanInputAutoOpenedRef.current.delete(taskId);
        deps.setHumanInputModalTaskId((c) => (c === taskId ? null : c));
        const finalMsg = status?.progress?.slice(-1)[0]?.message ?? "Terminé.";
        const doneMapVis =
          deps.extractChatMapVisualFromTaskEvents(events) ??
          deps.extractChatMapVisualFromAssistantText(finalMsg) ??
          deps.chatMapByTaskIdRef.current[taskId] ??
          null;
        if (doneMapVis) {
          deps.chatMapByTaskIdRef.current[taskId] = doneMapVis;
          deps.setChatMapByTaskId((prev) => (prev[taskId] === doneMapVis ? prev : { ...prev, [taskId]: doneMapVis }));
        }
        if (taskForActiveChat) {
          const turnUsage = deps.enrichUsageWithPricing(resolveTaskUsage(status, events));
          let storedMapVisual: ChatMapVisual | undefined;
          deps.setMessages((prev) => {
            const idx = deps.findLastChatAssistantIndex(prev, taskId);
            if (idx >= 0) {
              const next = [...prev];
              const keepMap = next[idx]!.mapVisual ?? doneMapVis ?? deps.chatMapByTaskIdRef.current[taskId] ?? undefined;
              storedMapVisual = keepMap;
              next[idx] = {
                role: "assistant",
                text: finalMsg,
                taskId,
                mapVisual: keepMap,
                usage: turnUsage ?? next[idx]!.usage,
              };
              return next;
            }
            storedMapVisual = doneMapVis ?? undefined;
            return [...prev, { role: "assistant", text: finalMsg, taskId, mapVisual: doneMapVis ?? undefined, usage: turnUsage }];
          });
          if (storedMapVisual && finalMsg.trim()) {
            try {
              const sid =
                (typeof deps.sessionId === "string" && deps.sessionId.trim()) ||
                localStorage.getItem(deps.akashaSessionIdKey) ||
                "";
              if (sid) {
                localStorage.setItem(
                  deps.chatMapMessageCacheKey(sid, finalMsg.trim()),
                  JSON.stringify(storedMapVisual),
                );
              }
            } catch {
              /* ignore */
            }
          }
          delete deps.ackTextByTaskRef.current[taskId];
        }
        if (deps.replyWithTtsRef.current && deps.voiceTtsConfigured && finalMsg?.trim()) {
          deps.replyWithTtsRef.current = false;
          invoke<{ data_url?: string }>("voice_tts", { text: finalMsg, port: deps.daemonPort })
            .then((r) => {
              const url = r?.data_url;
              if (url) {
                const audio = new Audio(url);
                audio.play().catch(() => {});
              }
            })
            .catch(() => {});
        }
        if (deps.selectedTaskIdForTodosRef.current === taskId) {
          void deps.fetchTasksEventsRef.current(taskId);
        }
        requestAnimationFrame(() => deps.chatInputRef.current?.focus());
        return;
      }
      if (currentStatus === "failed") {
        deps.setRunningTaskChips((prev) => {
          const next = { ...prev };
          delete next[taskId];
          return next;
        });
        deps.setRunningTaskEvents((prev) => {
          if (prev[taskId] === undefined) return prev;
          const next = { ...prev };
          delete next[taskId];
          return next;
        });
        void deps.fetchTasksList({ silent: true });
        deps.setPendingHumanInput((prev) => {
          const next = { ...prev };
          delete next[taskId];
          return next;
        });
        deps.humanInputAutoOpenedRef.current.delete(taskId);
        deps.setHumanInputModalTaskId((c) => (c === taskId ? null : c));
        deps.replyWithTtsRef.current = false;
        if (taskForActiveChat) {
          const turnUsage = deps.enrichUsageWithPricing(resolveTaskUsage(status, events));
          deps.setMessages((prev) => {
            const idx = deps.findLastChatAssistantIndex(prev, taskId);
            const MAX_FAILURE_CHAT_CHARS = 2500;
            const baseMsg = msg?.trim() ? msg.trim() : "Tâche en échec.";
            const finalMsg =
              baseMsg.length > MAX_FAILURE_CHAT_CHARS
                ? baseMsg.slice(0, MAX_FAILURE_CHAT_CHARS).trimEnd() + "…"
                : baseMsg;
            if (idx >= 0) {
              const next = [...prev];
              next[idx] = { role: "assistant", text: finalMsg, error: true, taskId, usage: turnUsage };
              return next;
            }
            return [...prev, { role: "assistant", text: finalMsg, error: true, taskId, usage: turnUsage }];
          });
          delete deps.ackTextByTaskRef.current[taskId];
        }
        if (deps.selectedTaskIdForTodosRef.current === taskId) {
          void deps.fetchTasksEventsRef.current(taskId);
        }
        requestAnimationFrame(() => deps.chatInputRef.current?.focus());
        return;
      }
    } catch {
      /* ignore */
    }
  }
  deps.setRunningTaskChips((prev) => {
    const next = { ...prev };
    delete next[taskId];
    return next;
  });
  deps.setRunningTaskEvents((prev) => {
    if (prev[taskId] === undefined) return prev;
    const next = { ...prev };
    delete next[taskId];
    return next;
  });
  if (deps.taskIdToSessionIdRef.current[taskId] === deps.sessionIdRef.current) {
    deps.setMessages((prev) => {
      const idx = deps.findLastChatAssistantIndex(prev, taskId);
      if (idx >= 0) {
        const next = [...prev];
        next[idx] = { role: "assistant", text: "Délai dépassé. Consultez Tâches.", taskId };
        return next;
      }
      return [...prev, { role: "assistant", text: "Délai dépassé. Consultez Tâches.", taskId }];
    });
    delete deps.ackTextByTaskRef.current[taskId];
  }
  requestAnimationFrame(() => deps.chatInputRef.current?.focus());
}
