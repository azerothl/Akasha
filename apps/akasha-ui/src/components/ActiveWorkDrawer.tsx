import { Fragment } from "react";

type ActiveTask = {
  id: string;
  status: string;
  label?: string;
  session_id?: string | null;
  parent_task_id?: string | null;
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
}: Props) {
  const en = locale === "en";
  if (tasks.length === 0) return null;
  const { roots, childrenOf } = groupTasks(tasks);
  const renderRow = (t: ActiveTask, nested: boolean) => (
    <li key={t.id} className={`active-work-item${nested ? " active-work-item-child" : ""}`}>
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
        {!nested && (childrenOf.get(t.id)?.length ?? 0) > 0 && onCancelChildren ? (
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
  );
  return (
    <div className="active-work-drawer" role="region" aria-label={en ? "Active work" : "Travail actif"}>
      <div className="active-work-drawer-header">
        <strong>{en ? "Active work" : "Travail actif"}</strong>
        <span className="muted">({tasks.length})</span>
      </div>
      <ul className="active-work-list">
        {roots.map((t) => (
          <Fragment key={t.id}>
            {renderRow(t, false)}
            {(childrenOf.get(t.id) ?? []).map((c) => renderRow(c, true))}
          </Fragment>
        ))}
      </ul>
    </div>
  );
}
