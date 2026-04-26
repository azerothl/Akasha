import { useEffect, useState } from "react";

type Props = {
  /** Active chat session id (resume-brief is scoped). */
  sessionId: string | null;
  /** Same helper as chat: E2E proxy vs direct daemon port. */
  daemonUrl: (path: string) => string;
  /** Only load in expert mode to avoid noise for simple users. */
  expert: boolean;
  labels: {
    title: string;
    resumeHeading: string;
    toolsHeading: string;
    noSession: string;
    docsMatrix: string;
    docsWebhooks: string;
    docsMcp: string;
    loadError: string;
  };
};

/**
 * Operator-facing snapshot: session resume-brief + effective tools (Hermes parity surfaces).
 * Links point at GitHub-hosted markdown in the Akasha repo (readable without a local checkout).
 */
export function OperatorHermesInsights({ sessionId, daemonUrl, expert, labels }: Props) {
  const [resumeJson, setResumeJson] = useState<string>("");
  const [toolsJson, setToolsJson] = useState<string>("");
  const [err, setErr] = useState<string>("");

  useEffect(() => {
    if (!expert) return;
    let cancelled = false;
    (async () => {
      setErr("");
      try {
        const tr = await fetch(daemonUrl("/api/tools/effective"));
        const tt = await tr.text();
        if (!cancelled) {
          setToolsJson(tr.ok ? tt : `${labels.loadError}: tools/effective HTTP ${tr.status}`);
        }
        if (sessionId?.trim()) {
          const q = new URLSearchParams({ session_id: sessionId.trim() });
          const rr = await fetch(daemonUrl(`/api/session/resume-brief?${q.toString()}`));
          const rt = await rr.text();
          if (!cancelled) {
            setResumeJson(rr.ok ? rt : `${labels.loadError}: resume-brief HTTP ${rr.status}`);
          }
        } else if (!cancelled) {
          setResumeJson(labels.noSession);
        }
      } catch (e) {
        if (!cancelled) setErr(e instanceof Error ? e.message : String(e));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [expert, sessionId, daemonUrl, labels.loadError, labels.noSession]);

  if (!expert) return null;

  return (
    <div className="operator-hermes-insights">
      <h3 className="settings-subtitle">{labels.title}</h3>
      <p className="settings-doc muted">
        <a href="https://github.com/azerothl/Akasha/blob/main/docs/hermes-akasha-parity-matrix.md" target="_blank" rel="noopener">
          {labels.docsMatrix}
        </a>
        {" · "}
        <a href="https://github.com/azerothl/Akasha/blob/main/docs/automation-webhooks.md" target="_blank" rel="noopener">
          {labels.docsWebhooks}
        </a>
        {" · "}
        <a href="https://github.com/azerothl/Akasha/blob/main/docs/mcp-runtime.md" target="_blank" rel="noopener">
          {labels.docsMcp}
        </a>
      </p>
      {err ? <p className="settings-plugin-reputation-feedback settings-plugin-reputation-feedback-err">{err}</p> : null}
      <h4 className="settings-plugin-status-title">{labels.resumeHeading}</h4>
      <pre className="operator-hermes-pre" tabIndex={0}>
        {resumeJson || "…"}
      </pre>
      <h4 className="settings-plugin-status-title" style={{ marginTop: "1rem" }}>
        {labels.toolsHeading}
      </h4>
      <pre className="operator-hermes-pre" tabIndex={0}>
        {toolsJson || "…"}
      </pre>
    </div>
  );
}
