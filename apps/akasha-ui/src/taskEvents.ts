export type TaskEventLike = {
  event_type: string;
  payload?: unknown;
  task_id?: string;
};

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
