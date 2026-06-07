import { Suspense, lazy, type ReactNode } from "react";
import { ModelUsageBadge } from "./ModelUsageBadge";
import { preprocessDataUrlImages } from "../preprocessDataUrlImages";
import { preprocessMessagePaths } from "../preprocessMessagePaths";
import type { ModelUsageStats } from "../modelUsage";
import { redactDisplaySecrets } from "../utils/redactDisplay";

function blurSecrets(text: string): string {
  return text
    .replace(/\bsk-[A-Za-z0-9_-]{12,}\b/g, "sk-••••••••")
    .replace(/\bBearer\s+[A-Za-z0-9._-]{10,}\b/gi, "Bearer ••••••••")
    .replace(/\bAKASHA_[A-Z0-9_]{4,}\b/g, "AKASHA_••••");
}

const LazyMarkdownContent = lazy(() => import("../MarkdownContent").then((m) => ({ default: m.default })));

export type ChatMessage = {
  role: "user" | "assistant" | "system";
  text: string;
  error?: boolean;
  streaming?: boolean;
  taskId?: string;
  mapVisual?: unknown;
  usage?: ModelUsageStats;
};

export type AskUserData = {
  question: string;
  context?: string;
  choices?: string[];
};

type Props = {
  messages: ChatMessage[];
  parseAskUser: (text: string) => AskUserData | null;
  onPathClick?: (path: string) => void;
  userAvatar?: string;
  agentAvatar?: string;
  agentName?: string;
  renderMapVisual?: (m: ChatMessage) => ReactNode;
  renderAskUserChoice?: (choice: string, index: number) => ReactNode;
  onOpenTaskDetail?: (taskId: string) => void;
  taskDetailLabel?: string;
};

export function ChatRenderer({
  messages,
  parseAskUser,
  onPathClick,
  userAvatar,
  agentAvatar,
  agentName = "Akasha",
  renderMapVisual,
  renderAskUserChoice,
  onOpenTaskDetail,
  taskDetailLabel = "View task",
}: Props) {
  return (
    <div className="chat-messages-column">
      {messages.map((m, i) => {
        const askUserData = m.role === "assistant" ? parseAskUser(m.text) : null;
        return (
          <div
            key={i}
            className={`message ${m.role} ${m.error ? "error" : ""} ${askUserData ? "message-ask-user" : ""} ${m.streaming ? "message-streaming" : ""}`}
          >
            <div className="message-head">
              {m.role === "user" && userAvatar ? (
                <img src={userAvatar} alt="" className="message-avatar message-avatar-user" />
              ) : null}
              {m.role === "assistant" && agentAvatar ? (
                <img src={agentAvatar} alt="" className="message-avatar message-avatar-assistant" />
              ) : null}
              <span className="role" aria-hidden>
                {m.role === "user" ? "Vous" : m.role === "system" ? "Système" : agentName}
              </span>
            </div>
            {m.role === "system" ? (
              <div className="text system-text" style={{ whiteSpace: "pre-wrap" }}>
                {m.text}
              </div>
            ) : askUserData ? (
              <div className="message-ask-user-card">
                <div className="message-ask-user-question markdown-rendered">
                  <Suspense fallback={<span>…</span>}>
                    <LazyMarkdownContent>{askUserData.question}</LazyMarkdownContent>
                  </Suspense>
                </div>
                {askUserData.context ? <p className="message-ask-user-context">{askUserData.context}</p> : null}
                {askUserData.choices?.length ? (
                  <div className="message-ask-user-choices">
                    {askUserData.choices.map((choice, j) =>
                      renderAskUserChoice ? (
                        renderAskUserChoice(choice, j)
                      ) : (
                        <span key={j} className="message-ask-user-choice-tag">
                          {choice}
                        </span>
                      ),
                    )}
                  </div>
                ) : null}
              </div>
            ) : (
              <div className="text markdown-rendered">
                <Suspense fallback={<span className="markdown-rendered">…</span>}>
                  <LazyMarkdownContent onPathClick={onPathClick}>
                    {preprocessMessagePaths(
                      preprocessDataUrlImages(
                        m.role === "assistant"
                          ? blurSecrets(redactDisplaySecrets(m.text))
                          : blurSecrets(m.text),
                      ),
                    )}
                  </LazyMarkdownContent>
                </Suspense>
                {m.streaming ? <span className="message-streaming-caret" aria-hidden /> : null}
              </div>
            )}
            {m.role === "assistant" && !m.streaming && (m.usage || (m.taskId && onOpenTaskDetail)) ? (
              <div className="message-footer">
                {m.usage ? <ModelUsageBadge usage={m.usage} compact /> : null}
                {m.taskId && onOpenTaskDetail ? (
                  <button
                    type="button"
                    className="chat-message-task-btn"
                    onClick={() => onOpenTaskDetail(m.taskId!)}
                  >
                    {taskDetailLabel}
                  </button>
                ) : null}
              </div>
            ) : null}
            {renderMapVisual ? renderMapVisual(m) : null}
          </div>
        );
      })}
    </div>
  );
}
