import { useState } from "react";
import { MonoIcon } from "./MonoIcon";
import { SteeringQueuePanel } from "./SteeringQueuePanel";

type Props = {
  taskId: string | null;
  fetchEndpoint: (path: string, init?: RequestInit) => Promise<{ ok: boolean; status: number; text: string }>;
  locale: "fr" | "en";
};

export function SteeringQueueBell({ taskId, fetchEndpoint, locale }: Props) {
  const [open, setOpen] = useState(false);
  const [pending, setPending] = useState(0);

  return (
    <div className="steering-queue-bell-wrap">
      <button
        type="button"
        className="steering-queue-bell-trigger"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        title={locale === "en" ? "Steering / follow-up queue" : "File steering / follow-up"}
      >
        <MonoIcon name="queue" />
        {pending > 0 ? <span className="steering-queue-bell-badge">{pending}</span> : null}
      </button>
      {open ? (
        <div className="steering-queue-bell-drawer" role="dialog">
          <div className="steering-queue-bell-head">
            <strong>{locale === "en" ? "Message queue" : "File de messages"}</strong>
            <button type="button" onClick={() => setOpen(false)} aria-label={locale === "en" ? "Close" : "Fermer"}>
              ×
            </button>
          </div>
          <SteeringQueuePanel
            taskId={taskId}
            fetchEndpoint={fetchEndpoint}
            locale={locale}
            onQueueChange={setPending}
          />
        </div>
      ) : null}
    </div>
  );
}
