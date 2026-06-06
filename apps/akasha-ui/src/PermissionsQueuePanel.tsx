import { useCallback, useEffect, useState } from "react";
import { useNotifyOnMessage } from "./notifications/useNotifyOnMessage";

type LocaleId = "fr" | "en";

export type PermissionQueueItem = {
  id: string;
  task_id: string;
  tool: string;
  scope: string;
  action: string;
  description: string;
  rationale: string;
  urgency: string;
  status: string;
  created_at: string;
  updated_at: string;
  decision_note?: string | null;
  decision_source?: string | null;
};

type Props = {
  fetchEndpoint: (path: string, init?: RequestInit) => Promise<{ ok: boolean; status: number; text: string }>;
  locale: LocaleId;
  decisionSource?: string;
};

function parseItems(jsonStr: string): PermissionQueueItem[] {
  try {
    const j = JSON.parse(jsonStr) as { items?: PermissionQueueItem[] };
    return Array.isArray(j.items) ? j.items : [];
  } catch {
    return [];
  }
}

export function PermissionsQueuePanel({ fetchEndpoint, locale, decisionSource = "ui_tauri" }: Props) {
  const [items, setItems] = useState<PermissionQueueItem[]>([]);
  const [loading, setLoading] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);

  useNotifyOnMessage(err, "error", locale === "en" ? "Permissions" : "Permissions");

  const load = useCallback(async () => {
    setLoading(true);
    setErr(null);
    try {
      const res = await fetchEndpoint("/api/permissions/queue?status=pending&limit=50");
      if (!res.ok) {
        setErr(`${locale === "en" ? "HTTP" : "HTTP"} ${res.status}`);
        setItems([]);
        return;
      }
      setItems(parseItems(res.text).filter((i) => i.status === "pending"));
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [fetchEndpoint, locale]);

  useEffect(() => {
    void load();
    const id = window.setInterval(() => void load(), 8000);
    return () => window.clearInterval(id);
  }, [load]);

  const decide = async (id: string, action: "approve" | "deny") => {
    setBusyId(id);
    try {
      const res = await fetchEndpoint(`/api/permissions/queue/${id}/${action}`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ decision_source: decisionSource }),
      });
      if (!res.ok) {
        setErr(`${action} HTTP ${res.status}`);
        return;
      }
      await load();
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setBusyId(null);
    }
  };

  if (loading && items.length === 0) {
    return <p className="muted">{locale === "en" ? "Loading permission queue…" : "Chargement de la file permissions…"}</p>;
  }

  return (
    <div className="permissions-queue-panel">
      {items.length === 0 ? (
        <p className="muted">
          {locale === "en" ? "No pending permission requests." : "Aucune demande d’approbation en attente."}
        </p>
      ) : (
        <ul className="permissions-queue-list">
          {items.map((item) => (
            <li key={item.id} className="permissions-queue-item">
              <div className="permissions-queue-item-head">
                <strong>{item.tool}</strong>
                <span className="muted"> · {item.action}</span>
              </div>
              <p className="permissions-queue-desc">{item.description || item.rationale}</p>
              <p className="muted permissions-queue-meta">
                {locale === "en" ? "Task" : "Tâche"} …{item.task_id.slice(-8)} · {item.scope}
              </p>
              <div className="permissions-queue-actions">
                <button
                  type="button"
                  className="btn-primary"
                  disabled={busyId === item.id}
                  onClick={() => void decide(item.id, "approve")}
                >
                  {locale === "en" ? "Approve" : "Approuver"}
                </button>
                <button
                  type="button"
                  className="btn-secondary"
                  disabled={busyId === item.id}
                  onClick={() => void decide(item.id, "deny")}
                >
                  {locale === "en" ? "Deny" : "Refuser"}
                </button>
              </div>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
