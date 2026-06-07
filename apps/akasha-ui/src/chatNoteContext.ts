/** Contexte note injecté dans le prochain message chat (côté API). */

export type ChatNoteIntent = "discuss" | "improve" | "validate" | "add_references";

export type ChatNoteContext = {
  noteId: string;
  title: string;
  markdown: string;
  intent?: ChatNoteIntent;
};

const MAX_NOTE_CHARS = 48_000;

function intentInstructions(intent: ChatNoteIntent | undefined, en: boolean): string {
  switch (intent) {
    case "improve":
      return en
        ? "The user wants you to improve and correct this note (spelling, clarity, structure). Propose an improved version; use notes_write only if the user confirms applying changes."
        : "L'utilisateur souhaite améliorer et corriger cette note (orthographe, clarté, structure). Proposez une version améliorée ; utilisez notes_write seulement si l'utilisateur confirme l'application.";
    case "validate":
      return en
        ? "Validate this note: check internal consistency, flag unverifiable claims, list gaps, and suggest what to verify."
        : "Validez cette note : cohérence interne, signalez les affirmations non vérifiables, listez les lacunes et ce qu'il faudrait vérifier.";
    case "add_references":
      return en
        ? "Add references and sources to this note using web_search when helpful. Suggest citations/links to append or integrate."
        : "Ajoutez des références et sources à cette note (web_search si utile). Proposez des citations/liens à intégrer.";
    case "discuss":
    default:
      return en
        ? "The user is discussing this note. Use it as the primary source; cite sections when helpful. Do not say you lack access to the note."
        : "L'utilisateur discute de cette note. Utilisez-la comme source principale ; citez les sections si utile. Ne dites pas que vous n'avez pas accès à la note.";
  }
}

export function buildMessageWithNoteContext(
  userMessage: string,
  ctx: ChatNoteContext,
  locale: "fr" | "en",
): string {
  const en = locale === "en";
  let note = ctx.markdown.trim();
  const truncated = note.length > MAX_NOTE_CHARS;
  if (truncated) {
    note = `${note.slice(0, MAX_NOTE_CHARS)}\n\n${en ? "[... note truncated for context length ...]" : "[... note tronquée pour la taille du contexte ...]"}`;
  }
  const instructions = intentInstructions(ctx.intent, en);

  if (en) {
    return `[User note — discussion context]
${instructions}

Note id: ${ctx.noteId}
Title: ${ctx.title}
--- Note ---
${note}
--- End note ---

User message: ${userMessage}`;
  }

  return `[Note utilisateur — contexte de discussion]
${instructions}

Id note : ${ctx.noteId}
Titre : ${ctx.title}
--- Note ---
${note}
--- Fin note ---

Message : ${userMessage}`;
}
