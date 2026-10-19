/** Older daemons may have truncated a private prompt before the actual request. */
export function taskTitle(label: string | undefined, fallback: string): string {
  const text = label?.trim() ?? "";
  return !text || /^\[(?:Composer mode:[^\]]+\]\s|Task\]\s+Objective:|Orchestrated — disk deliverables REQUIRED\]|Pièce\(s\) jointe\(s\) à ce message)/.test(text)
    ? fallback : text;
}

export function taskStatus(status: string, locale: "fr" | "en"): string {
  const labels: Record<string, [string, string]> = {
    queued: ["En file d’attente", "Queued"],
    waiting_user_input: ["Votre réponse est attendue", "Waiting for your input"],
    interrupted: ["Interrompue", "Interrupted"],
    pending: ["En attente", "Pending"], running: ["En cours", "Running"],
    paused: ["En pause", "Paused"], completed: ["Terminée", "Completed"],
    failed: ["Échec", "Failed"], cancelled: ["Annulée", "Cancelled"],
  };
  return labels[status.toLowerCase()]?.[locale === "en" ? 1 : 0] ?? status;
}
