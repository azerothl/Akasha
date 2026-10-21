import { useEffect } from "react";

const KEY = "akasha_panel_reload_draft";
export const PRESERVE_DRAFT_EVENT = "akasha:preserve-draft-before-reload";

/** Used only for an explicit recovery reload; not a persistent draft history. */
export function loadRecoveryDraft(): string {
  try {
    const saved = JSON.parse(sessionStorage.getItem(KEY) ?? "null");
    return saved && saved.sessionId === localStorage.getItem("akasha_session_id") && typeof saved.message === "string"
      ? saved.message : "";
  } catch { return ""; }
}

export function useChatDraftRecovery(message: string, sessionId: string | null) {
  useEffect(() => { try { sessionStorage.removeItem(KEY); } catch { /* optional recovery storage */ } }, []);
  useEffect(() => {
    const preserve = (event: Event) => {
      try { sessionStorage.setItem(KEY, JSON.stringify({ message, sessionId })); }
      catch { event.preventDefault(); }
    };
    window.addEventListener(PRESERVE_DRAFT_EVENT, preserve);
    return () => window.removeEventListener(PRESERVE_DRAFT_EVENT, preserve);
  }, [message, sessionId]);
}
