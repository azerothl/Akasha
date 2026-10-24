import { useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

export type PendingQuestion = { question: string; context: string; choices?: string[]; request_id?: string };

/** One shared submission path for the dialog, inline form and chat choices. */
export function useHumanReply(options: {
  pending: Record<string, PendingQuestion>;
  port: number;
  english: boolean;
  refresh: () => Promise<void>;
  accepted: (taskId: string, requestId?: string) => void;
}) {
  const lock = useRef(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  async function submit(taskId: string, response: string) {
    if (lock.current || !response.trim()) return;
    const question = options.pending[taskId];
    if (!question) return;
    lock.current = true;
    setBusy(true);
    setError(null);
    try {
      const reply = await invoke<{ ok?: boolean }>("post_task_human_reply", {
        taskId, requestId: question.request_id, response: response.trim(), port: options.port,
      });
      if (reply?.ok !== true) throw new Error("Reply was not acknowledged by the daemon");
      options.accepted(taskId, question.request_id);
    } catch (cause) {
      const detail = String(cause);
      const stale = /409|stale_approval_request/.test(detail);
      const expired = /404|410|no_pending_human_input|human_input_expired/.test(detail);
      setError(options.english
        ? stale ? "This question changed. Review the current question before replying again."
          : expired ? "This question has expired or the task has stopped. Refresh the task before resuming it."
          : `Unable to send your answer. Your text is preserved; check the daemon connection and try again. ${detail}`
        : stale ? "La question a changé. Vérifiez la question actuelle avant de répondre à nouveau."
          : expired ? "Cette question a expiré ou la tâche s’est arrêtée. Actualisez la tâche avant de la reprendre."
          : `Impossible d’envoyer votre réponse. Votre texte est conservé ; vérifiez la connexion au daemon puis réessayez. ${detail}`);
      // Never replay an approval against a different request_id.
      await options.refresh().catch(() => {});
    } finally {
      lock.current = false;
      setBusy(false);
    }
  }
  return { submit, busy, error, clearError: () => setError(null) };
}
