/** Contexte rapport Deep Research injecté dans le prochain message chat (côté API). */

export type ChatResearchContext = {
  topic: string;
  reportMarkdown: string;
  category?: string;
};

const MAX_REPORT_CHARS = 48_000;

export function buildMessageWithResearchContext(
  userMessage: string,
  ctx: ChatResearchContext,
  locale: "fr" | "en",
): string {
  const en = locale === "en";
  let report = ctx.reportMarkdown.trim();
  const truncated = report.length > MAX_REPORT_CHARS;
  if (truncated) {
    report = `${report.slice(0, MAX_REPORT_CHARS)}\n\n${en ? "[... report truncated for context length ...]" : "[... rapport tronqué pour la taille du contexte ...]"}`;
  }
  const categoryLine = ctx.category ? (en ? `Category: ${ctx.category}\n` : `Catégorie : ${ctx.category}\n`) : "";

  if (en) {
    return `[Deep Research report — discussion context]
The user is discussing the research report below. Use it as the primary source; cite sections when helpful. Do not say you lack access to the report.

Topic: ${ctx.topic}
${categoryLine}
--- Report ---
${report}
--- End report ---

User question: ${userMessage}`;
  }

  return `[Rapport Deep Research — contexte de discussion]
L'utilisateur discute du rapport ci-dessous. Utilisez-le comme source principale ; citez les sections si utile. Ne dites pas que vous n'avez pas accès au rapport.

Sujet : ${ctx.topic}
${categoryLine}
--- Rapport ---
${report}
--- Fin du rapport ---

Question : ${userMessage}`;
}
