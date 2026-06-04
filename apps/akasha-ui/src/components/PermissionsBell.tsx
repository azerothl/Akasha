import { useCallback, useEffect, useState } from "react";
import { PermissionsQueuePanel, type PermissionQueueItem } from "../PermissionsQueuePanel";

type Props = {
  fetchEndpoint: (path: string, init?: RequestInit) => Promise<{ ok: boolean; status: number; text: string }>;
  locale: "fr" | "en";
  onOpenChange?: (open: boolean) => void;
};

function countPending(text: string): number {
  try {
    const j = JSON.parse(text) as { items?: PermissionQueueItem[] };
    return Array.isArray(j.items) ? j.items.filter((i) => i.status === "pending").length : 0;
  } catch {
    return 0;
  }
}

export function PermissionsBell({ fetchEndpoint, locale, onOpenChange }: Props) {
  const [pending, setPending] = useState(0);
  const [open, setOpen] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const res = await fetchEndpoint("/api/permissions/queue?status=pending&limit=50");
      if (res.ok) setPending(countPending(res.text));
    } catch {
      /* ignore */
    }
  }, [fetchEndpoint]);

  useEffect(() => {
    void refresh();
    const id = window.setInterval(() => void refresh(), 8000);
    return () => window.clearInterval(id);
  }, [refresh]);

  const toggle = () => {
    setOpen((o) => {
      const next = !o;
      onOpenChange?.(next);
      return next;
    });
  };

  return (
    <div className="permissions-bell-wrap">
      <button
        type="button"
        className="permissions-bell-trigger"
        onClick={toggle}
        aria-expanded={open}
        aria-haspopup="dialog"
        title={locale === "en" ? "Pending tool approvals" : "Approbations d’outils en attente"}
      >
        <span aria-hidden>🔐</span>
        {pending > 0 ? <span className="permissions-bell-badge">{pending}</span> : null}
      </button>
      {open ? (
        <div className="permissions-bell-drawer" role="dialog" aria-label={locale === "en" ? "Permission queue" : "File permissions"}>
          <div className="permissions-bell-drawer-head">
            <strong>{locale === "en" ? "Permissions" : "Permissions"}</strong>
            <button type="button" className="permissions-bell-close" onClick={() => setOpen(false)} aria-label={locale === "en" ? "Close" : "Fermer"}>
              ×
            </button>
          </div>
          <PermissionsQueuePanel fetchEndpoint={fetchEndpoint} locale={locale} />
        </div>
      ) : null}
    </div>
  );
}
