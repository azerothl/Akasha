import { Tooltip } from "./Tooltip";

export type ComposerMode = "agent" | "architect" | "code" | "ask";

type Props = {
  locale: "fr" | "en";
  agentMode: boolean;
  onAgentModeChange: (v: boolean) => void;
  composerMode: ComposerMode;
  onComposerModeChange: (v: ComposerMode) => void;
  webSearchEnabled: boolean;
  onWebSearchChange: (v: boolean) => void;
  incognito: boolean;
  onIncognitoChange: (v: boolean) => void;
  disabled?: boolean;
};

const MODES: { id: ComposerMode; en: string; fr: string; tipEn: string; tipFr: string }[] = [
  {
    id: "ask",
    en: "Ask",
    fr: "Ask",
    tipEn: "Q&A only — read tools, no file writes",
    tipFr: "Q&R seulement — lecture seule, pas d’écriture",
  },
  {
    id: "architect",
    en: "Architect",
    fr: "Architecte",
    tipEn: "Plan & design — no full implementation",
    tipFr: "Plan & conception — pas d’implémentation complète",
  },
  {
    id: "code",
    en: "Code",
    fr: "Code",
    tipEn: "Implement — files, shell, tests",
    tipFr: "Implémenter — fichiers, shell, tests",
  },
  {
    id: "agent",
    en: "Agent",
    fr: "Agent",
    tipEn: "Full agent with tools (default)",
    tipFr: "Agent complet avec outils (défaut)",
  },
];

export function ChatCompositionBar({
  locale,
  agentMode,
  onAgentModeChange,
  composerMode,
  onComposerModeChange,
  webSearchEnabled,
  onWebSearchChange,
  incognito,
  onIncognitoChange,
  disabled,
}: Props) {
  const en = locale === "en";
  return (
    <div className="btn-group chat-composition-bar" role="toolbar" aria-label={en ? "Chat options" : "Options de chat"}>
      <div className="btn-group chat-composer-modes" role="group" aria-label={en ? "Agent mode" : "Mode agent"}>
        {MODES.map((m) => (
          <Tooltip key={m.id} content={en ? m.tipEn : m.tipFr}>
            <button
              type="button"
              className={`btn-group-item ${composerMode === m.id ? "active" : ""}`}
              onClick={() => {
                onComposerModeChange(m.id);
                if (m.id === "ask") onAgentModeChange(false);
                else onAgentModeChange(true);
              }}
              disabled={disabled}
              aria-pressed={composerMode === m.id}
            >
              {en ? m.en : m.fr}
            </button>
          </Tooltip>
        ))}
      </div>
      <Tooltip content={en ? "Prefer web search in replies" : "Privilégier la recherche web"}>
        <button
          type="button"
          className={`btn-group-item ${webSearchEnabled ? "active" : ""}`}
          onClick={() => onWebSearchChange(!webSearchEnabled)}
          disabled={disabled}
        >
          {en ? "Web" : "Web"}
        </button>
      </Tooltip>
      <Tooltip content={en ? "Incognito — skip memory promotion for this session" : "Incognito — sans promotion mémoire pour cette session"}>
        <button
          type="button"
          className={`btn-group-item ${incognito ? "active" : ""}`}
          onClick={() => onIncognitoChange(!incognito)}
          disabled={disabled}
        >
          {en ? "Incognito" : "Incognito"}
        </button>
      </Tooltip>
      {/* Keep legacy agentMode toggle for callers that still sync boolean state */}
      <span className="visually-hidden" aria-hidden={!agentMode}>
        {agentMode ? "agent-on" : "agent-off"}
      </span>
    </div>
  );
}
