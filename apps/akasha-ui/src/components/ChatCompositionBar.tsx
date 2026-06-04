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
    <div className="chat-composition-bar" role="toolbar" aria-label={en ? "Chat options" : "Options de chat"}>
      <button
        type="button"
        className={`chat-composition-toggle ${agentMode ? "active" : ""}`}
        onClick={() => onAgentModeChange(!agentMode)}
        disabled={disabled}
        title={en ? "Agent mode (tools enabled)" : "Mode agent (outils activés)"}
      >
        {agentMode ? (en ? "Agent" : "Agent") : en ? "Chat" : "Chat"}
      </button>
      <button
        type="button"
        className={`chat-composition-toggle ${webSearchEnabled ? "active" : ""}`}
        onClick={() => onWebSearchChange(!webSearchEnabled)}
        disabled={disabled}
        title={en ? "Prefer web search in replies" : "Privilégier la recherche web"}
      >
        {en ? "Web" : "Web"}
      </button>
      <button
        type="button"
        className={`chat-composition-toggle ${incognito ? "active" : ""}`}
        onClick={() => onIncognitoChange(!incognito)}
        disabled={disabled}
        title={en ? "Incognito — skip memory promotion for this session" : "Incognito — sans promotion mémoire pour cette session"}
      >
        {en ? "Incognito" : "Incognito"}
      </button>
    </div>
  );
}
