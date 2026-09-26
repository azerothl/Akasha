import { Fragment, useState } from "react";

type ActiveTask = {
  id: string;
  status: string;
  label?: string;
  session_id?: string | null;
  parent_task_id?: string | null;
  assigned_agent?: string;
};

type Props = {
  locale: "fr" | "en";
  tasks: ActiveTask[];
  onOpenTask: (taskId: string) => void;
  onOpenSession?: (sessionId: string) => void;
  onCancel: (taskId: string) => void;
  onPause?: (taskId: string) => void;
  /** Cancel all active children of a parent (B4 subagent threads). */
  onCancelChildren?: (parentTaskId: string) => void;
  /** Cancel parent + all descendants (P6-B4 aggregated cancel). */
  onCancelTree?: (rootTaskId: string) => void;
};

function groupTasks(tasks: ActiveTask[]): { roots: ActiveTask[]; childrenOf: Map<string, ActiveTask[]> } {
  const ids = new Set(tasks.map((t) => t.id));
  const childrenOf = new Map<string, ActiveTask[]>();
  const roots: ActiveTask[] = [];
  for (const t of tasks) {
    const p = t.parent_task_id?.trim();
    if (p && ids.has(p)) {
      const list = childrenOf.get(p) ?? [];
      list.push(t);
      childrenOf.set(p, list);
    } else {
      roots.push(t);
    }
  }
  return { roots, childrenOf };
}

export function ActiveWorkDrawer({
  locale,
  tasks,
  onOpenTask,
  onOpenSession,
  onCancel,
  onPause,
  onCancelChildren,
  onCancelTree,
}: Props) {
  const en = locale === "en";
  const [inspectId, setInspectId] = useState<string | null>(null);
  if (tasks.length === 0) return null;
  const { roots, childrenOf } = groupTasks(tasks);
  const renderRow = (t: ActiveTask, nested: boolean) => {
    const kids = childrenOf.get(t.id) ?? [];
    const inspecting = inspectId === t.id;
    return (
      <Fragment key={t.id}>
        <li className={`active-work-item${nested ? " active-work-item-child" : ""}`}>
          <button type="button" className="active-work-label" onClick={() => onOpenTask(t.id)}>
            <span className="active-work-status">{t.status}</span>
            <span className="active-work-title">
              {nested ? "↳ " : ""}
              {t.label?.trim() || t.id.slice(0, 8)}
            </span>
          </button>
          <div className="active-work-actions">
            {t.session_id && onOpenSession ? (
              <button type="button" className="btn-secondary btn-tiny" onClick={() => onOpenSession(t.session_id!)}>
                {en ? "Session" : "Session"}
              </button>
            ) : null}
            {!nested && kids.length > 0 ? (
              <button
                type="button"
                className="btn-secondary btn-tiny"
                aria-expanded={inspecting}
                aria-controls={inspecting ? `active-work-inspect-${t.id}` : undefined}
                onClick={() => setInspectId(inspecting ? null : t.id)}
              >
                {en ? (inspecting ? "Hide" : "Inspect") : inspecting ? "Masquer" : "Inspecter"}
              </button>
            ) : null}
            {!nested && kids.length > 0 && onCancelTree ? (
              <button type="button" className="btn-secondary btn-tiny" onClick={() => onCancelTree(t.id)}>
                {en ? "Cancel all" : "Tout annuler"}
              </button>
            ) : null}
            {!nested && kids.length > 0 && onCancelChildren ? (
              <button type="button" className="btn-secondary btn-tiny" onClick={() => onCancelChildren(t.id)}>
                {en ? "Cancel children" : "Annuler enfants"}
              </button>
            ) : null}
            {onPause ? (
              <button type="button" className="btn-secondary btn-tiny" onClick={() => onPause(t.id)}>
                {en ? "Pause" : "Pause"}
              </button>
            ) : null}
            <button type="button" className="btn-secondary btn-tiny" onClick={() => onCancel(t.id)}>
              {en ? "Cancel" : "Annuler"}
            </button>
          </div>
        </li>
        {inspecting && kids.length > 0 ? (
          <li
            id={`active-work-inspect-${t.id}`}
            className="active-work-inspect"
            role="region"
            aria-label={en ? "Subagent threads" : "Threads sous-agents"}
          >
            <p className="active-work-inspect-title muted">
              {en
                ? `${kids.length} subagent thread(s)`
                : `${kids.length} thread(s) sous-agent`}
            </p>
            <ul className="active-work-inspect-list">
              {kids.map((c) => (
                <li key={c.id} className="active-work-inspect-row">
                  <button type="button" className="active-work-inspect-open" onClick={() => onOpenTask(c.id)}>
                    <span className="active-work-status">{c.status}</span>
                    <span>{c.label?.trim() || c.id.slice(0, 8)}</span>
                    {c.assigned_agent ? (
                      <span className="muted active-work-inspect-agent">{c.assigned_agent}</span>
                    ) : null}
                  </button>
                  <button type="button" className="btn-secondary btn-tiny" onClick={() => onCancel(c.id)}>
                    {en ? "Cancel" : "Annuler"}
                  </button>
                </li>
              ))}
            </ul>
          </li>
        ) : null}
        {kids.map((c) => renderRow(c, true))}
      </Fragment>
    );
  };
  return (
    <div className="active-work-drawer" role="region" aria-label={en ? "Active work" : "Travail actif"}>
      <div className="active-work-drawer-header">
        <strong>{en ? "Active work" : "Travail actif"}</strong>
        <span className="muted">({tasks.length})</span>
      </div>
      <ul className="active-work-list">{roots.map((t) => renderRow(t, false))}</ul>
    </div>
  );
}
