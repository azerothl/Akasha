import { useCallback, useEffect, useState } from "react";

export type QueuedMessageRow = {
  id: string;
  mode: string;
  text: string;
  queued_at: string;
};

type QueueSnapshot = {
  task_id?: string;
  steering?: QueuedMessageRow[];
  follow_up?: QueuedMessageRow[];
};

type Props = {
  taskId: string | null;
  fetchEndpoint: (path: string, init?: RequestInit) => Promise<{ ok: boolean; status: number; text: string }>;
  locale: "fr" | "en";
  onQueueChange?: (pending: number) => void;
};

function pendingTotal(snap: QueueSnapshot): number {
  return (snap.steering?.length ?? 0) + (snap.follow_up?.length ?? 0);
}

export function SteeringQueuePanel({ taskId, fetchEndpoint, locale, onQueueChange }: Props) {
  const [snap, setSnap] = useState<QueueSnapshot | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    if (!taskId?.trim()) {
      setSnap(null);
      onQueueChange?.(0);
      return;
    }
    try {
      const res = await fetchEndpoint(`/api/tasks/${encodeURIComponent(taskId)}/queue`);
      if (!res.ok) {
        setError(res.text || `HTTP ${res.status}`);
        return;
      }
      const j = JSON.parse(res.text) as QueueSnapshot;
      setSnap(j);
      setError(null);
      onQueueChange?.(pendingTotal(j));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [taskId, fetchEndpoint, onQueueChange]);

  useEffect(() => {
    void refresh();
    const id = window.setInterval(() => void refresh(), 4000);
    return () => window.clearInterval(id);
  }, [refresh]);

  const flush = async () => {
    if (!taskId?.trim()) return;
    setBusy(true);
    try {
      const res = await fetchEndpoint(`/api/tasks/${encodeURIComponent(taskId)}/queue`, {
        method: "DELETE",
      });
      if (!res.ok) {
        setError(res.text || `HTTP ${res.status}`);
        return;
      }
      await refresh();
    } finally {
      setBusy(false);
    }
  };

  if (!taskId?.trim()) {
    return (
      <p className="steering-queue-empty">
        {locale === "en" ? "No running task selected." : "Aucune tâche active sélectionnée."}
      </p>
    );
  }

  const steering = snap?.steering ?? [];
  const followUp = snap?.follow_up ?? [];
  const total = steering.length + followUp.length;

  return (
    <div className="steering-queue-panel">
      {error ? <p className="steering-queue-error">{error}</p> : null}
      {total === 0 ? (
        <p className="steering-queue-empty">
          {locale === "en" ? "Queue empty." : "File vide."}
        </p>
      ) : (
        <>
          {steering.length > 0 ? (
            <div className="steering-queue-section">
              <strong>{locale === "en" ? "Steering (next turn)" : "Steering (prochain tour)"}</strong>
              <ul>
                {steering.map((m) => (
                  <li key={m.id}>
                    <span className="steering-queue-text">{m.text}</span>
                    <span className="steering-queue-meta">{m.queued_at}</span>
                  </li>
                ))}
              </ul>
            </div>
          ) : null}
          {followUp.length > 0 ? (
            <div className="steering-queue-section">
              <strong>{locale === "en" ? "Follow-up (after task)" : "Follow-up (après tâche)"}</strong>
              <ul>
                {followUp.map((m) => (
                  <li key={m.id}>
                    <span className="steering-queue-text">{m.text}</span>
                    <span className="steering-queue-meta">{m.queued_at}</span>
                  </li>
                ))}
              </ul>
            </div>
          ) : null}
        </>
      )}
      <button type="button" className="steering-queue-flush" disabled={busy || total === 0} onClick={() => void flush()}>
        {locale === "en" ? "Clear queue" : "Vider la file"}
      </button>
    </div>
  );
}
