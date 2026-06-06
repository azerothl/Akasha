export type TaskEventLike = {
  event_type: string;
  payload?: unknown;
  task_id?: string;
};

export type TaskEventRow = TaskEventLike & {
  at: string;
};

export function normalizeTaskStatus(status: string | undefined): string {
  return (status ?? "pending").trim().toLowerCase();
}

export function isTaskActiveStatus(status: string): boolean {
  const s = normalizeTaskStatus(status);
  return s === "pending" || s === "queued" || s === "running" || s === "waiting_user_input";
}

export function isTaskTerminalStatus(status: string): boolean {
  const s = normalizeTaskStatus(status);
  return s === "completed" || s === "failed" || s === "cancelled" || s === "interrupted";
}

/** Union by stable key; keeps the richest timeline when polls overlap. */
export function mergeTaskEvents<T extends TaskEventRow>(prev: T[], incoming: T[]): T[] {
  if (incoming.length === 0) return prev;
  const byKey = new Map<string, T>();
  const keyOf = (e: T) =>
    `${e.task_id ?? ""}\0${e.event_type}\0${e.at}\0${JSON.stringify(e.payload ?? null)}`;
  for (const e of prev) byKey.set(keyOf(e), e);
  for (const e of incoming) byKey.set(keyOf(e), e);
  return [...byKey.values()].sort((a, b) => a.at.localeCompare(b.at));
}

/**
 * Keep one stream entry per (task_id, progress_pct).
 * Later streamed chunks replace earlier ones to avoid noisy duplicates.
 */
export function collapseStreamedProgressEvents<T extends TaskEventLike>(
  events: T[],
  fallbackTaskId = "root",
): T[] {
  const out: T[] = [];
  const indexByStreamKey = new Map<string, number>();
  for (const ev of events) {
    if (ev.event_type !== "progress_update" || !ev.payload || typeof ev.payload !== "object") {
      out.push(ev);
      continue;
    }
    const payload = ev.payload as Record<string, unknown>;
    const pct = typeof payload.progress_pct === "number" ? payload.progress_pct : null;
    const message = typeof payload.message === "string" ? payload.message.trim() : "";
    if (pct == null || !message) {
      out.push(ev);
      continue;
    }
    const payloadTaskId = typeof payload.task_id === "string" && payload.task_id.trim() ? payload.task_id.trim() : "";
    const taskId = payloadTaskId || ev.task_id || fallbackTaskId;
    const key = `${taskId}\0${pct}`;
    const existingIdx = indexByStreamKey.get(key);
    if (existingIdx == null) {
      indexByStreamKey.set(key, out.length);
      out.push(ev);
    } else {
      out[existingIdx] = ev;
    }
  }
  return out;
}
