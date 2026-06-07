import type { ExecutionStep } from "../tasks/buildExecutionSteps";
import { executionProgress } from "../tasks/buildExecutionSteps";

type Props = {
  steps: ExecutionStep[];
  emptyReason: "none" | "direct" | "no_selection";
  t: (key: string) => string;
  classifyAgentKind: (agent: string) => string;
  onSelectChildTask?: (childTaskId: string) => void;
};

export function TaskExecutionSteps({ steps, emptyReason, t, classifyAgentKind, onSelectChildTask }: Props) {
  const progress = executionProgress(steps);

  if (emptyReason === "no_selection") {
    return <p className="empty-state task-steps-empty">{t("tasks.select_task")}</p>;
  }

  if (steps.length === 0) {
    return <p className="empty-state task-steps-empty">{t("tasks.steps_empty_direct")}</p>;
  }

  return (
    <>
      {progress.total > 0 && (
        <div className="task-steps-summary">
          <div className="task-steps-summary-topline">
            <span>
              {t("tasks.step_done")}: {progress.done}/{progress.total}
            </span>
            <span>{progress.progressPct}%</span>
          </div>
          <div className="task-steps-summary-bar">
            <div className="task-steps-summary-bar-fill" style={{ width: `${progress.progressPct}%` }} />
          </div>
        </div>
      )}
      <ul className="task-steps-list" role="list" aria-label={t("tasks.steps_title")}>
        {steps.map((step) => {
          const clickable = Boolean(step.childTaskId && onSelectChildTask);
          return (
            <li
              key={step.key}
              className={`task-step task-step--${step.status} task-step--${step.source}`}
            >
              <span className="task-step-check" aria-hidden>
                {step.status === "done" || step.status === "completed"
                  ? "☑"
                  : step.status === "cancelled" || step.status === "failed"
                    ? "⊘"
                    : step.status === "running"
                      ? "◉"
                      : "☐"}
              </span>
              <div className="task-step-body">
                <div className="task-step-title-row">
                  {step.stepId && <span className="task-step-id">{step.stepId}</span>}
                  {step.agent && (
                    <span className="agent-kind-pill task-step-agent" data-agent-kind={classifyAgentKind(step.agent)}>
                      {step.agent}
                    </span>
                  )}
                  <span className="task-step-source-badge">{t(`tasks.step_source_${step.source}`)}</span>
                </div>
                {clickable ? (
                  <button
                    type="button"
                    className="task-step-title task-step-title-btn"
                    onClick={() => onSelectChildTask!(step.childTaskId!)}
                  >
                    {step.title}
                  </button>
                ) : (
                  <span className="task-step-title">{step.title}</span>
                )}
              </div>
              <span className={"task-step-badge activity-task-status-pill status-" + step.status}>
                {step.status === "done" || step.status === "completed"
                  ? t("tasks.step_done")
                  : step.status === "cancelled"
                    ? t("tasks.step_cancelled")
                    : step.status === "failed"
                      ? t("common.error")
                      : step.status === "running"
                        ? t("tasks.step_running")
                        : t("tasks.step_pending")}
              </span>
            </li>
          );
        })}
      </ul>
    </>
  );
}
