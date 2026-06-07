import { Tooltip } from "./Tooltip";

type Props = {
  locale: "fr" | "en";
  agentMode: boolean;
  onAgentModeChange: (v: boolean) => void;
  webSearchEnabled: boolean;
  onWebSearchChange: (v: boolean) => void;
  incognito: boolean;
  onIncognitoChange: (v: boolean) => void;
  disabled?: boolean;
};

export function ChatCompositionBar({
  locale,
  agentMode,
  onAgentModeChange,
  webSearchEnabled,
  onWebSearchChange,
  incognito,
  onIncognitoChange,
  disabled,
}: Props) {
  const en = locale === "en";
  return (
    <div className="btn-group chat-composition-bar" role="toolbar" aria-label={en ? "Chat options" : "Options de chat"}>
      <Tooltip content={en ? "Agent mode (tools enabled)" : "Mode agent (outils activés)"}>
        <button
          type="button"
          className={`btn-group-item ${agentMode ? "active" : ""}`}
          onClick={() => onAgentModeChange(!agentMode)}
          disabled={disabled}
        >
          {agentMode ? (en ? "Agent" : "Agent") : en ? "Chat" : "Chat"}
        </button>
      </Tooltip>
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
    </div>
  );
}
