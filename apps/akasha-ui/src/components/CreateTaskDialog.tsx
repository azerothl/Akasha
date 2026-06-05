import { useCallback, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

const DAEMON_PORT = 3876;

export type CreateTaskDialogProps = {
  open: boolean;
  onClose: () => void;
  sessionId: string;
  t: (key: string) => string;
  onImmediateCreated?: (taskId: string) => void;
  onScheduleCreated?: (scheduleId: string) => void;
  onTriggerCreated?: () => void;
  eventTriggersEnabled?: boolean;
};

type TabId = "immediate" | "recurring" | "oneshot" | "trigger";

export function CreateTaskDialog({
  open,
  onClose,
  sessionId,
  t,
  onImmediateCreated,
  onScheduleCreated,
  onTriggerCreated,
  eventTriggersEnabled = true,
}: CreateTaskDialogProps) {
  const [tab, setTab] = useState<TabId>("immediate");
  const [message, setMessage] = useState("");
  const [priority, setPriority] = useState<"normal" | "high">("normal");
  const [scheduleName, setScheduleName] = useState("");
  const [schedulePrompt, setSchedulePrompt] = useState("");
  const [intervalSeconds, setIntervalSeconds] = useState("3600");
  const [rrule, setRrule] = useState("");
  const [oneShotAt, setOneShotAt] = useState("");
  const [triggerName, setTriggerName] = useState("");
  const [triggerType, setTriggerType] = useState("webhook");
  const [triggerPrompt, setTriggerPrompt] = useState("");
  const [triggerFilter, setTriggerFilter] = useState("{}");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const resetAndClose = useCallback(() => {
    setError(null);
    setSubmitting(false);
    onClose();
  }, [onClose]);

  const handleSubmit = useCallback(async () => {
    setError(null);
    setSubmitting(true);
    try {
      if (tab === "immediate") {
        const msg = message.trim();
        if (!msg) {
          setError(t("tasks.create_message_required"));
          return;
        }
        const ack = await invoke<{ message?: string; task_id?: string }>("send_message_ack", {
          message: msg,
          sessionId,
          port: DAEMON_PORT,
          ...(priority === "high" ? { priority: "high" } : {}),
        });
        if (ack?.task_id) onImmediateCreated?.(ack.task_id);
        resetAndClose();
        return;
      }

      if (tab === "recurring" || tab === "oneshot") {
        const name = scheduleName.trim();
        const desc = schedulePrompt.trim();
        if (!name || !desc) {
          setError(t("tasks.create_schedule_fields_required"));
          return;
        }
        let body: Record<string, unknown> = {
          name,
          description: desc,
          enabled: true,
          timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC",
          channel_context: desc,
        };
        if (tab === "recurring") {
          const interval = parseInt(intervalSeconds, 10);
          if (rrule.trim()) {
            body.rrule = rrule.trim();
          } else {
            body.interval_seconds = isNaN(interval) ? 3600 : interval;
          }
        } else {
          if (!oneShotAt) {
            setError(t("tasks.create_oneshot_date_required"));
            return;
          }
          body.start_at = new Date(oneShotAt).toISOString();
          body.rrule = "FREQ=DAILY;COUNT=1";
          body.interval_seconds = null;
        }
        const data = await invoke<{ id?: string }>("create_schedule_extended", { body, port: DAEMON_PORT });
        if (data?.id) onScheduleCreated?.(data.id);
        resetAndClose();
        return;
      }

      if (tab === "trigger") {
        const name = triggerName.trim();
        const prompt = triggerPrompt.trim();
        if (!name || !prompt) {
          setError(t("tasks.create_trigger_fields_required"));
          return;
        }
        let filter: unknown = {};
        try {
          filter = JSON.parse(triggerFilter || "{}");
        } catch {
          setError(t("tasks.create_trigger_filter_invalid"));
          return;
        }
        await invoke("create_event_trigger", {
          body: {
            name,
            enabled: true,
            trigger_type: triggerType,
            filter,
            prompt_template: prompt,
            cooldown_seconds: 60,
          },
          port: DAEMON_PORT,
        });
        onTriggerCreated?.();
        resetAndClose();
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setSubmitting(false);
    }
  }, [
    tab,
    message,
    priority,
    scheduleName,
    schedulePrompt,
    intervalSeconds,
    rrule,
    oneShotAt,
    triggerName,
    triggerType,
    triggerPrompt,
    triggerFilter,
    sessionId,
    t,
    onImmediateCreated,
    onScheduleCreated,
    onTriggerCreated,
    resetAndClose,
  ]);

  if (!open) return null;

  const tabs: { id: TabId; label: string; disabled?: boolean }[] = [
    { id: "immediate", label: t("tasks.create_tab_immediate") },
    { id: "recurring", label: t("tasks.create_tab_recurring") },
    { id: "oneshot", label: t("tasks.create_tab_oneshot") },
    { id: "trigger", label: t("tasks.create_tab_trigger"), disabled: !eventTriggersEnabled },
  ];

  return (
    <div className="human-input-overlay create-task-overlay" role="dialog" aria-modal="true" aria-labelledby="create-task-title">
      <div className="human-input-modal create-task-modal">
        <h2 id="create-task-title">{t("tasks.create_title")}</h2>
        <div className="create-task-tabs" role="tablist">
          {tabs.map((item) => (
            <button
              key={item.id}
              type="button"
              role="tab"
              aria-selected={tab === item.id}
              className={"create-task-tab" + (tab === item.id ? " active" : "") + (item.disabled ? " disabled" : "")}
              disabled={item.disabled}
              onClick={() => !item.disabled && setTab(item.id)}
            >
              {item.label}
            </button>
          ))}
        </div>

        <div className="create-task-body">
          {tab === "immediate" && (
            <>
              <label className="create-task-field">
                <span>{t("tasks.create_message_label")}</span>
                <textarea value={message} onChange={(e) => setMessage(e.target.value)} rows={4} />
              </label>
              <label className="create-task-field">
                <span>{t("tasks.create_priority_label")}</span>
                <select value={priority} onChange={(e) => setPriority(e.target.value as "normal" | "high")}>
                  <option value="normal">{t("tasks.create_priority_normal")}</option>
                  <option value="high">{t("tasks.create_priority_high")}</option>
                </select>
              </label>
            </>
          )}

          {tab === "recurring" && (
            <>
              <label className="create-task-field">
                <span>{t("tasks.create_schedule_name")}</span>
                <input type="text" value={scheduleName} onChange={(e) => setScheduleName(e.target.value)} />
              </label>
              <label className="create-task-field">
                <span>{t("tasks.create_schedule_prompt")}</span>
                <textarea value={schedulePrompt} onChange={(e) => setSchedulePrompt(e.target.value)} rows={3} />
              </label>
              <label className="create-task-field">
                <span>{t("tasks.create_interval_label")}</span>
                <input type="number" min={60} value={intervalSeconds} onChange={(e) => setIntervalSeconds(e.target.value)} />
              </label>
              <label className="create-task-field">
                <span>{t("tasks.create_rrule_label")}</span>
                <input type="text" value={rrule} onChange={(e) => setRrule(e.target.value)} placeholder="FREQ=DAILY;BYHOUR=9" />
              </label>
            </>
          )}

          {tab === "oneshot" && (
            <>
              <label className="create-task-field">
                <span>{t("tasks.create_schedule_name")}</span>
                <input type="text" value={scheduleName} onChange={(e) => setScheduleName(e.target.value)} />
              </label>
              <label className="create-task-field">
                <span>{t("tasks.create_schedule_prompt")}</span>
                <textarea value={schedulePrompt} onChange={(e) => setSchedulePrompt(e.target.value)} rows={3} />
              </label>
              <label className="create-task-field">
                <span>{t("tasks.create_oneshot_at")}</span>
                <input type="datetime-local" value={oneShotAt} onChange={(e) => setOneShotAt(e.target.value)} />
              </label>
            </>
          )}

          {tab === "trigger" && (
            <>
              <label className="create-task-field">
                <span>{t("tasks.create_trigger_name")}</span>
                <input type="text" value={triggerName} onChange={(e) => setTriggerName(e.target.value)} />
              </label>
              <label className="create-task-field">
                <span>{t("tasks.create_trigger_type")}</span>
                <select value={triggerType} onChange={(e) => setTriggerType(e.target.value)}>
                  <option value="webhook">{t("tasks.trigger_type_webhook")}</option>
                  <option value="task_failed">{t("tasks.trigger_type_task_failed")}</option>
                  <option value="filesystem">{t("tasks.trigger_type_filesystem")}</option>
                  <option value="model_available">{t("tasks.trigger_type_model_available")}</option>
                  <option value="daemon_error">{t("tasks.trigger_type_daemon_error")}</option>
                </select>
              </label>
              <label className="create-task-field">
                <span>{t("tasks.create_trigger_filter")}</span>
                <textarea value={triggerFilter} onChange={(e) => setTriggerFilter(e.target.value)} rows={2} />
              </label>
              <label className="create-task-field">
                <span>{t("tasks.create_trigger_prompt")}</span>
                <textarea value={triggerPrompt} onChange={(e) => setTriggerPrompt(e.target.value)} rows={3} />
              </label>
            </>
          )}
        </div>

        {error && <p className="error-inline">{error}</p>}

        <div className="create-task-actions">
          <button type="button" className="btn-secondary" onClick={resetAndClose} disabled={submitting}>
            {t("tasks.create_cancel")}
          </button>
          <button type="button" className="btn-primary" onClick={() => void handleSubmit()} disabled={submitting}>
            {submitting ? t("common.loading") : t("tasks.create_submit")}
          </button>
        </div>
      </div>
    </div>
  );
}
