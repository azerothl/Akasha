/** Helpers inspired by Claude Code UX (export transcript, filename from first prompt) — no proprietary code. */

export type ExportableMessage = { role: "user" | "assistant" | "system"; text: string };

/** Plain-text export of the visible chat (for download). */
export function exportChatPlainText(messages: ExportableMessage[]): string {
  const lines: string[] = [];
  for (const m of messages) {
    const label = m.role === "user" ? "User" : m.role === "assistant" ? "Assistant" : "System";
    lines.push(`--- ${label} ---`);
    lines.push((m.text ?? "").trimEnd());
    lines.push("");
  }
  return lines.join("\n").trimEnd() + "\n";
}

/** First line of first user message, sanitized for use in a filename (ASCII-ish). */
export function sanitizeFilenameFromFirstUser(messages: ExportableMessage[], maxLen = 48): string {
  const first = messages.find((m) => m.role === "user");
  if (!first?.text?.trim()) return "";
  const line = first.text.split("\n")[0]?.trim() ?? "";
  const s = line
    .toLowerCase()
    .replace(/[^a-z0-9\s-]/gi, "")
    .replace(/\s+/g, "-")
    .replace(/-+/g, "-")
    .replace(/^-|-$/g, "");
  if (!s) return "";
  return s.length > maxLen ? s.slice(0, maxLen - 1) + "-" : s;
}

export function defaultExportBasename(messages: ExportableMessage[]): string {
  const ts = new Date();
  const pad = (n: number) => String(n).padStart(2, "0");
  const stamp = `${ts.getFullYear()}-${pad(ts.getMonth() + 1)}-${pad(ts.getDate())}-${pad(ts.getHours())}${pad(ts.getMinutes())}${pad(ts.getSeconds())}`;
  const fromUser = sanitizeFilenameFromFirstUser(messages);
  return fromUser ? `akasha-chat-${fromUser}` : `akasha-chat-${stamp}`;
}

/** Heuristic one-line summary for a list of tool names (deterministic, no LLM). */
export function heuristicToolBatchSummary(toolNames: string[]): string | null {
  if (!toolNames.length) return null;
  const counts = new Map<string, number>();
  const bucket = (n: string) => {
    const x = n.toLowerCase();
    if (x.includes("read") || x === "read_file") return "read";
    if (x.includes("write") || x.includes("edit") || x.includes("patch") || x.includes("replace")) return "write";
    if (x.includes("grep") || x.includes("search")) return "search";
    if (x.includes("run") || x === "run_command") return "shell";
    if (x.includes("web") || x.includes("fetch")) return "web";
    return "other";
  };
  for (const n of toolNames) {
    const b = bucket(n);
    counts.set(b, (counts.get(b) ?? 0) + 1);
  }
  const parts: string[] = [];
  const order = ["read", "search", "write", "shell", "web", "other"] as const;
  const labels: Record<(typeof order)[number], string> = {
    read: "file reads",
    search: "searches",
    write: "edits",
    shell: "commands",
    web: "web",
    other: "other tools",
  };
  for (const k of order) {
    const c = counts.get(k);
    if (c) parts.push(`${c}× ${labels[k]}`);
  }
  return parts.length ? `Tools: ${parts.join(", ")}` : null;
}
