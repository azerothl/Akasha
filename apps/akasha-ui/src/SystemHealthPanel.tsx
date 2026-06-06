import { useEffect, useMemo, useState } from "react";
import { useNotifyOnMessage } from "./notifications/useNotifyOnMessage";
import { PermissionsQueuePanel } from "./PermissionsQueuePanel";

type LocaleId = "fr" | "en";

type EndpointResult = {
  ok: boolean;
  status: number;
  text: string;
};

type Props = {
  sessionId: string | null;
  fetchEndpoint: (path: string, init?: RequestInit) => Promise<EndpointResult>;
  requestEndpoint?: (method: string, path: string, body?: string) => Promise<EndpointResult>;
  expert: boolean;
  locale: LocaleId;
  labels: {
    title: string;
    resumeHeading: string;
    toolsHeading: string;
    noSession: string;
    docsMatrix: string;
    docsWebhooks: string;
    docsMcp: string;
    recallHeading: string;
    mcpHeading: string;
    lifecycleHeading: string;
    terminalHeading: string;
    opsHeading: string;
    loadError: string;
    detailsToggle: string;
    summaryUnavailable: string;
  };
};

function tryParseJson(text: string): unknown | null {
  const t = text.trim();
  if (!t || t.startsWith("(")) return null;
  try {
    return JSON.parse(t) as unknown;
  } catch {
    return null;
  }
}

function summarizeTools(jsonStr: string, summaryUnavailable: string, locale: LocaleId): string[] {
  const j = tryParseJson(jsonStr) as { tools?: Array<{ name?: string; allowed?: boolean; runnable?: boolean }> } | null;
  const tools = j?.tools;
  if (!Array.isArray(tools) || tools.length === 0) {
    return [summaryUnavailable];
  }
  const allowed = tools.filter((r) => r.allowed !== false).length;
  const runnable = tools.filter((r) => r.runnable === true).length;
  const names = tools
    .filter((r) => r.runnable === true || r.allowed !== false)
    .map((r) => r.name ?? "?")
    .filter(Boolean)
    .slice(0, 6);
  const preview = names.length ? names.join(", ") : "—";
  if (locale === "en") {
    return [
      `${tools.length} tool(s) listed · ${allowed} allowed by policy · ${runnable} ready to use.`,
      `Examples: ${preview}`,
    ];
  }
  return [
    `${tools.length} outil(s) référencés · ${allowed} autorisé(s) par la politique · ${runnable} prêt(s) à l’emploi.`,
    `Exemples : ${preview}`,
  ];
}

function summarizeRecall(jsonStr: string, summaryUnavailable: string, locale: LocaleId): string[] {
  const j = tryParseJson(jsonStr) as Record<string, unknown> | null;
  if (!j || typeof j !== "object") return [summaryUnavailable];
  const keys = Object.keys(j);
  if (keys.length === 0) return [summaryUnavailable];
  const parts: string[] = [];
  for (const k of ["recall_count", "recalls", "hits", "hit_rate", "miss_rate", "entries"]) {
    if (k in j && j[k] != null) {
      parts.push(`${k} : ${String(j[k])}`);
    }
  }
  if (parts.length === 0) {
    return locale === "en"
      ? [`${keys.length} metric(s) available (see technical details).`]
      : [`${keys.length} indicateur(s) disponibles (voir détails techniques).`];
  }
  return [parts.slice(0, 4).join(" · ")];
}

function summarizeMcp(statusText: string, runtimeText: string, summaryUnavailable: string, locale: LocaleId): string[] {
  const st = tryParseJson(statusText);
  const rt = tryParseJson(runtimeText);
  let configured = 0;
  let online = 0;
  if (st && typeof st === "object") {
    const o = st as Record<string, unknown>;
    const servers = o.servers ?? o.mcp_servers ?? o.config;
    if (Array.isArray(servers)) configured = servers.length;
    else if (servers && typeof servers === "object") configured = Object.keys(servers as object).length;
  }
  if (rt && typeof rt === "object") {
    const o = rt as Record<string, unknown>;
    const run = o.running ?? o.servers ?? o.processes;
    if (Array.isArray(run)) online = run.filter(Boolean).length;
    else if (typeof o.connected === "boolean" && o.connected) online = 1;
  }
  if (!configured && !online && !statusText.trim() && !runtimeText.trim()) {
    return [summaryUnavailable];
  }
  if (locale === "en") {
    return [`MCP servers configured: ${configured || "—"} · active / reachable: ${online || "—"}.`];
  }
  return [`Serveurs MCP configurés : ${configured || "—"} · actifs / joignables : ${online || "—"}.`];
}

function summarizeTerminal(jsonStr: string, summaryUnavailable: string, locale: LocaleId): string[] {
  const j = tryParseJson(jsonStr) as Record<string, unknown> | null;
  if (!j) return [summaryUnavailable];
  const pty = j.interactive_pty ?? j.pty ?? j.has_pty ?? j["pty_supported"];
  const shell = j.shell ?? j.default_shell;
  const ptyLabel =
    typeof pty === "boolean"
      ? pty
        ? locale === "en"
          ? "Integrated terminal available"
          : "Terminal intégré disponible"
        : locale === "en"
          ? "Integrated terminal unavailable"
          : "Terminal intégré indisponible"
      : locale === "en"
        ? `PTY: ${String(pty ?? "—")}`
        : `PTY : ${String(pty ?? "—")}`;
  const shellLabel = shell
    ? locale === "en"
      ? `Default shell: ${String(shell)}.`
      : `Shell par défaut : ${String(shell)}.`
    : "";
  return [ptyLabel + (shellLabel ? ` ${shellLabel}` : "")];
}

function summarizeLifecycle(jsonStr: string, summaryUnavailable: string, locale: LocaleId): string[] {
  const j = tryParseJson(jsonStr);
  let n = 0;
  if (Array.isArray(j)) n = j.length;
  else if (j && typeof j === "object") {
    const o = j as Record<string, unknown>;
    const hooks = o.hooks ?? o.registered;
    if (Array.isArray(hooks)) n = hooks.length;
    else if (hooks && typeof hooks === "object") n = Object.keys(hooks as object).length;
  }
  if (!n && !jsonStr.trim()) return [summaryUnavailable];
  if (!n) {
    return locale === "en" ? ["Lifecycle hooks registered (see technical details)."] : ["Hooks enregistrés (voir détails techniques)."];
  }
  return locale === "en"
    ? [`${n} lifecycle hook(s) or automation(s) registered.`]
    : [`${n} automation(s) ou hook(s) lifecycle enregistré(s).`];
}

function summarizeOps(text: string, summaryUnavailable: string): string[] {
  const lines = text.split("\n").filter(Boolean);
  if (lines.length === 0) return [summaryUnavailable];
  const head = lines[0];
  if (head.includes("schedules:") || head.includes("task_runs:")) {
    return [head.replace(/\s*\|\s*/g, " · ")];
  }
  return [lines[0].slice(0, 200) + (lines[0].length > 200 ? "…" : "")];
}

function summarizeResume(jsonStr: string, noSession: string, locale: LocaleId): string[] {
  if (jsonStr.includes(noSession) || jsonStr.trim() === noSession) {
    return [noSession];
  }
  const j = tryParseJson(jsonStr);
  if (j && typeof j === "object") {
    const o = j as Record<string, unknown>;
    const keys = Object.keys(o).length;
    return keys
      ? locale === "en"
        ? [`Session summary available (${keys} field(s)).`]
        : [`Résumé de session disponible (${keys} champ(s)).`]
      : locale === "en"
        ? ["Session summary is empty."]
        : ["Résumé de session vide."];
  }
  if (jsonStr.trim().length > 20) {
    return locale === "en"
      ? ["A session summary is available for this chat."]
      : ["Un résumé de session est disponible pour cette discussion."];
  }
  return [noSession];
}

function HealthCard({
  title,
  lines,
  raw,
  expert,
  detailsLabel,
}: {
  title: string;
  lines: string[];
  raw: string;
  expert: boolean;
  detailsLabel: string;
}) {
  return (
    <section className="health-card">
      <h4 className="health-card-title">{title}</h4>
      <div className="health-card-summary">
        {lines.map((line, i) => (
          <p key={i} className="health-card-line muted">
            {line}
          </p>
        ))}
      </div>
      {expert ? (
        <details className="health-card-details">
          <summary className="health-card-details-summary">{detailsLabel}</summary>
          <pre className="operator-insights-pre health-card-pre" tabIndex={0}>
            {raw || "…"}
          </pre>
        </details>
      ) : null}
    </section>
  );
}

/**
 * User-friendly system health (tools, memory, MCP, terminal, lifecycle, schedules).
 * Raw JSON is shown under "Détails techniques" in expert mode only.
 */
export function SystemHealthPanel({ sessionId, fetchEndpoint, requestEndpoint, expert, locale, labels }: Props) {
  const [opsSummary, setOpsSummary] = useState<string>("");
  const [resumeJson, setResumeJson] = useState<string>("");
  const [toolsJson, setToolsJson] = useState<string>("");
  const [recallJson, setRecallJson] = useState<string>("");
  const [mcpStatusJson, setMcpStatusJson] = useState<string>("");
  const [mcpRuntimeJson, setMcpRuntimeJson] = useState<string>("");
  const [terminalJson, setTerminalJson] = useState<string>("");
  const [lifecycleJson, setLifecycleJson] = useState<string>("");
  const [permissionsJson, setPermissionsJson] = useState<string>("");
  const [doctorJson, setDoctorJson] = useState<string>("");
  const [browserHint, setBrowserHint] = useState<string | null>(null);
  const [err, setErr] = useState<string>("");

  useNotifyOnMessage(err || null, "error", labels.title);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      setErr("");
      try {
        const tr = await fetchEndpoint("/api/tools/effective");
        if (!cancelled) {
          setToolsJson(tr.ok ? tr.text : `${labels.loadError}: tools/effective HTTP ${tr.status}`);
        }
        const rm = await fetchEndpoint("/api/memory/recall-metrics");
        if (!cancelled) {
          setRecallJson(rm.ok ? rm.text : `${labels.loadError}: recall-metrics HTTP ${rm.status}`);
        }
        const ms = await fetchEndpoint("/api/mcp/status");
        const mr = await fetchEndpoint("/api/mcp/runtime");
        if (!cancelled) {
          setMcpStatusJson(ms.ok ? ms.text : `${labels.loadError}: mcp/status HTTP ${ms.status}`);
          setMcpRuntimeJson(mr.ok ? mr.text : `${labels.loadError}: mcp/runtime HTTP ${mr.status}`);
        }
        const tc = await fetchEndpoint("/api/terminal/capabilities");
        if (!cancelled) {
          setTerminalJson(tc.ok ? tc.text : `${labels.loadError}: terminal/capabilities HTTP ${tc.status}`);
        }
        const lh = await fetchEndpoint("/api/lifecycle/hooks");
        if (!cancelled) {
          setLifecycleJson(lh.ok ? lh.text : `${labels.loadError}: lifecycle/hooks HTTP ${lh.status}`);
        }
        const pq = await fetchEndpoint("/api/permissions/queue");
        if (!cancelled) {
          setPermissionsJson(pq.ok ? pq.text : `${labels.loadError}: permissions/queue HTTP ${pq.status}`);
        }
        const dr = await fetchEndpoint("/api/doctor");
        if (!cancelled) {
          setDoctorJson(dr.ok ? dr.text : `${labels.loadError}: doctor HTTP ${dr.status}`);
        }
        const sc = await fetchEndpoint("/api/schedules");
        const runs = await fetchEndpoint("/api/task_runs");
        const pw = await fetchEndpoint("/api/process/watch/recent?limit=20");
        if (!cancelled) {
          const lines = [
            `schedules: ${sc.status}`,
            `task_runs: ${runs.status}`,
            `process_watch: ${pw.status}`,
          ];
          setOpsSummary(
            `${lines.join(" | ")}\n\n--- /api/schedules ---\n${sc.text}\n\n--- /api/task_runs ---\n${runs.text}\n\n--- /api/process/watch/recent ---\n${pw.text}`,
          );
        }
        if (sessionId?.trim()) {
          const q = new URLSearchParams({ session_id: sessionId.trim() });
          const rr = await fetchEndpoint(`/api/session/resume-brief?${q.toString()}`);
          if (!cancelled) {
            setResumeJson(rr.ok ? rr.text : `${labels.loadError}: resume-brief HTTP ${rr.status}`);
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
  }, [sessionId, fetchEndpoint, labels.loadError, labels.noSession]);

  useEffect(() => {
    const tools = tryParseJson(toolsJson) as { browser_enabled?: boolean } | null;
    const resume = tryParseJson(resumeJson) as { tools_policy_brief?: { browser_enabled?: boolean } } | null;
    const doctor = tryParseJson(doctorJson) as {
      playwright?: {
        runner_found?: boolean;
        node_modules_playwright?: boolean;
        auto_install_disabled?: boolean;
      };
    } | null;
    const browserEnabled =
      tools?.browser_enabled === true || resume?.tools_policy_brief?.browser_enabled === true;
    if (!browserEnabled) {
      setBrowserHint(null);
      return;
    }
    const pw = doctor?.playwright;
    if (pw?.node_modules_playwright === true) {
      setBrowserHint(null);
      return;
    }
    if (pw?.runner_found === true && pw?.auto_install_disabled !== true) {
      setBrowserHint(null);
      return;
    }
    setBrowserHint(
      locale === "en"
        ? "Browser tool is enabled but Playwright/Chromium is not ready. Run akasha doctor, then npm install + npx playwright install chromium in playwright-runner (or set AKASHA_PLAYWRIGHT_AUTO_INSTALL)."
        : "L’outil navigateur est activé mais Playwright/Chromium n’est pas prêt. Lancez akasha doctor, puis npm install + npx playwright install chromium dans playwright-runner (ou AKASHA_PLAYWRIGHT_AUTO_INSTALL).",
    );
  }, [toolsJson, resumeJson, doctorJson, locale]);

  const toolsLines = useMemo(
    () => summarizeTools(toolsJson, labels.summaryUnavailable, locale),
    [toolsJson, labels.summaryUnavailable, locale],
  );
  const recallLines = useMemo(
    () => summarizeRecall(recallJson, labels.summaryUnavailable, locale),
    [recallJson, labels.summaryUnavailable, locale],
  );
  const mcpLines = useMemo(
    () => summarizeMcp(mcpStatusJson, mcpRuntimeJson, labels.summaryUnavailable, locale),
    [mcpStatusJson, mcpRuntimeJson, labels.summaryUnavailable, locale],
  );
  const mcpRaw = useMemo(() => {
    const a = mcpStatusJson || "";
    const b = mcpRuntimeJson || "";
    return `${a}\n\n--- /api/mcp/runtime ---\n${b}`;
  }, [mcpStatusJson, mcpRuntimeJson]);
  const terminalLines = useMemo(
    () => summarizeTerminal(terminalJson, labels.summaryUnavailable, locale),
    [terminalJson, labels.summaryUnavailable, locale],
  );
  const lifecycleLines = useMemo(
    () => summarizeLifecycle(lifecycleJson, labels.summaryUnavailable, locale),
    [lifecycleJson, labels.summaryUnavailable, locale],
  );
  const opsLines = useMemo(() => summarizeOps(opsSummary, labels.summaryUnavailable), [opsSummary, labels.summaryUnavailable]);
  const resumeLines = useMemo(() => summarizeResume(resumeJson, labels.noSession, locale), [resumeJson, labels.noSession, locale]);

  return (
    <div className="operator-insights-panel system-health-panel">
      <h3 className="settings-subtitle">{labels.title}</h3>
      <p className="settings-doc muted">
        <a href="https://github.com/azerothl/Akasha/blob/main/spec/dev/roadmap/reference-products-parity-matrix.md" target="_blank" rel="noopener noreferrer">
          {labels.docsMatrix}
        </a>
        {" · "}
        <a href="https://github.com/azerothl/Akasha/blob/main/spec/dev/integrations/automation-webhooks.md" target="_blank" rel="noopener noreferrer">
          {labels.docsWebhooks}
        </a>
        {" · "}
        <a href="https://github.com/azerothl/Akasha/blob/main/spec/dev/integrations/mcp-runtime.md" target="_blank" rel="noopener noreferrer">
          {labels.docsMcp}
        </a>
      </p>

      {browserHint ? (
        <p className="settings-doc banner banner-warning" role="status">
          {browserHint}
        </p>
      ) : null}

      <HealthCard
        title={labels.opsHeading}
        lines={opsLines}
        raw={opsSummary}
        expert={expert}
        detailsLabel={labels.detailsToggle}
      />
      <HealthCard
        title={labels.resumeHeading}
        lines={resumeLines}
        raw={resumeJson}
        expert={expert}
        detailsLabel={labels.detailsToggle}
      />
      <HealthCard
        title={labels.toolsHeading}
        lines={toolsLines}
        raw={toolsJson}
        expert={expert}
        detailsLabel={labels.detailsToggle}
      />
      <HealthCard
        title={labels.recallHeading}
        lines={recallLines}
        raw={recallJson}
        expert={expert}
        detailsLabel={labels.detailsToggle}
      />
      <HealthCard
        title={labels.mcpHeading}
        lines={mcpLines}
        raw={mcpRaw}
        expert={expert}
        detailsLabel={labels.detailsToggle}
      />
      <HealthCard
        title={labels.terminalHeading}
        lines={terminalLines}
        raw={terminalJson}
        expert={expert}
        detailsLabel={labels.detailsToggle}
      />
      <HealthCard
        title={labels.lifecycleHeading}
        lines={lifecycleLines}
        raw={lifecycleJson}
        expert={expert}
        detailsLabel={labels.detailsToggle}
      />
      <section className="health-card permissions-queue-card">
        <h4 className="health-card-title">{locale === "fr" ? "File permissions" : "Permission queue"}</h4>
        {requestEndpoint ? (
          <PermissionsQueuePanel
            locale={locale}
            fetchEndpoint={(path, init) =>
              requestEndpoint(init?.method ?? "GET", path, typeof init?.body === "string" ? init.body : undefined)
            }
            decisionSource="ui_tauri"
          />
        ) : (
          <HealthCard
            title=""
            lines={
              permissionsJson
                ? [`${locale === "fr" ? "Voir détails pour approuver/refuser" : "See details to approve/deny"}`]
                : [labels.summaryUnavailable]
            }
            raw={permissionsJson}
            expert={expert}
            detailsLabel={labels.detailsToggle}
          />
        )}
        {expert && permissionsJson ? (
          <details className="health-card-details">
            <summary>{labels.detailsToggle}</summary>
            <pre className="operator-insights-pre health-card-pre" tabIndex={0}>
              {permissionsJson}
            </pre>
          </details>
        ) : null}
      </section>
    </div>
  );
}
