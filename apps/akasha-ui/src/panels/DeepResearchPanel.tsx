import { Suspense, lazy, useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  ResearchStarViz,
  type ResearchBranchViz,
  type ResearchPhase,
  type ResearchStepState,
} from "../components/ResearchStarViz";
import {
  downloadResearchReportHtml,
  exportResearchReportPdf,
  openResearchReportWindow,
  type ResearchReportDocument,
  type ResearchReportMeta,
} from "../researchReportExport";
import {
  loadResearchReportTheme,
  RESEARCH_REPORT_THEME_OPTIONS,
  saveResearchReportTheme,
  type ResearchReportTheme,
} from "../researchReportThemes";
import { useI18n } from "../useI18n";

const LazyMarkdownContent = lazy(() => import("../MarkdownContent").then((m) => ({ default: m.default })));

const STORAGE_KEY = "akasha_deep_research_v2";
const MAX_HISTORY = 40;
const POLL_MS = 1500;

export type ReportMeta = ResearchReportMeta;

export type ResearchRun = {
  id: string;
  serverRunId?: string;
  topic: string;
  maxRounds: number;
  createdAt: string;
  updatedAt: string;
  status: "running" | "complete" | "error" | "cancelled";
  branches: ResearchBranchViz[];
  evolvingReport?: string;
  reportMarkdown?: string;
  reportMeta?: ReportMeta;
  searchDegraded?: boolean;
  error?: string;
};

type ServerRun = {
  id: string;
  topic: string;
  phase: string;
  round: number;
  max_rounds: number;
  branches: {
    id: string;
    label: string;
    status: string;
    sub_agents: { id: string; role: string; label: string; status: string }[];
  }[];
  evolving_report: string;
  report_markdown?: string | null;
  report_meta?: ReportMeta | null;
  search_degraded: boolean;
  error?: string | null;
  finished_at?: string | null;
};

type Props = {
  fetchEndpoint: (path: string, init?: RequestInit) => Promise<{ ok: boolean; status: number; text: string }>;
  locale: "fr" | "en";
  onDiscussReport?: (doc: ResearchReportDocument) => void;
};

function loadHistory(): ResearchRun[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw) as ResearchRun[];
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

function saveHistory(runs: ResearchRun[]) {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(runs.slice(0, MAX_HISTORY)));
}

function newId(): string {
  return `res_${Date.now()}_${Math.random().toString(36).slice(2, 9)}`;
}

function mapStepStatus(s: string): ResearchStepState {
  if (s === "running" || s === "done" || s === "error" || s === "pending") return s;
  return "pending";
}

function mapServerPhase(phase: string): ResearchPhase {
  const p = phase.toLowerCase();
  if (p === "planning") return "planning";
  if (p === "searching") return "searching";
  if (p === "reading") return "reading";
  if (p === "analyzing") return "analyzing";
  if (p === "writing") return "writing";
  if (p === "done") return "done";
  if (p === "error" || p === "cancelled") return "idle";
  return "steps";
}

function serverToRun(server: ServerRun, existing?: ResearchRun): ResearchRun {
  const status =
    server.phase === "done"
      ? "complete"
      : server.phase === "error"
        ? "error"
        : server.phase === "cancelled"
          ? "cancelled"
          : "running";

  return {
    id: existing?.id ?? server.id,
    serverRunId: server.id,
    topic: server.topic,
    maxRounds: server.max_rounds,
    createdAt: existing?.createdAt ?? new Date().toISOString(),
    updatedAt: new Date().toISOString(),
    status,
    branches: server.branches.map((b) => ({
      label: b.label,
      status: mapStepStatus(b.status),
      subAgents: b.sub_agents.map((a) => ({
        id: a.id,
        role: a.role,
        label: a.label,
        status: mapStepStatus(a.status),
      })),
    })),
    evolvingReport: server.evolving_report || undefined,
    reportMarkdown: server.report_markdown ?? undefined,
    reportMeta: server.report_meta ?? undefined,
    searchDegraded: server.search_degraded,
    error: server.error ?? undefined,
  };
}

function formatDaemonError(message: string, locale: "fr" | "en"): string {
  const en = locale === "en";
  const lower = message.toLowerCase();
  if (lower.includes("timed out") || lower.includes("timeout") || lower.includes("time out")) {
    return en
      ? "Request timed out — use an external LLM in llm_router.yaml for faster research."
      : "Délai dépassé — configurez un LLM externe dans llm_router.yaml pour accélérer la recherche.";
  }
  return message;
}

function formatWhen(iso: string, locale: "fr" | "en"): string {
  try {
    return new Date(iso).toLocaleString(locale === "en" ? "en-GB" : "fr-FR", {
      dateStyle: "short",
      timeStyle: "short",
    });
  } catch {
    return iso;
  }
}

function phaseLabelKey(phase: ResearchPhase): string | null {
  switch (phase) {
    case "planning":
      return "research.phase.planning";
    case "searching":
      return "research.phase.searching";
    case "reading":
      return "research.phase.reading";
    case "analyzing":
      return "research.phase.analyzing";
    case "writing":
    case "synthesizing":
      return "research.phase.writing";
    default:
      return null;
  }
}

export function DeepResearchPanel({ fetchEndpoint, locale, onDiscussReport }: Props) {
  const { t } = useI18n();
  const en = locale === "en";
  const [topic, setTopic] = useState("");
  const [maxRounds, setMaxRounds] = useState(10);
  const [history, setHistory] = useState<ResearchRun[]>(() => loadHistory());
  const [selectedId, setSelectedId] = useState<string | null>(() => loadHistory()[0]?.id ?? null);
  const [activeRun, setActiveRun] = useState<ResearchRun | null>(null);
  const [phase, setPhase] = useState<ResearchPhase>("idle");
  const [loading, setLoading] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [apiReady, setApiReady] = useState<boolean | null>(null);
  const [reportTheme, setReportTheme] = useState<ResearchReportTheme>(() => loadResearchReportTheme());
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const serverRunIdRef = useRef<string | null>(null);
  const autoOpenedReportIdsRef = useRef<Set<string>>(new Set());
  /** When true, do not auto-select the latest history run (user clicked "New research"). */
  const draftModeRef = useRef(false);

  const daemonOutdatedMsg = en
    ? "Research API unavailable — rebuild and restart the daemon (IterResearch routes)."
    : "API recherche indisponible — recompilez et redémarrez le daemon (routes IterResearch).";

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const res = await fetchEndpoint("/api/research/deep/start", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ topic: "" }),
        });
        if (cancelled) return;
        setApiReady(res.status === 400);
      } catch {
        if (!cancelled) setApiReady(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [fetchEndpoint]);

  useEffect(() => {
    return () => {
      if (pollRef.current) clearInterval(pollRef.current);
    };
  }, []);

  const persist = useCallback((runs: ResearchRun[]) => {
    setHistory(runs);
    saveHistory(runs);
  }, []);

  const reportExportBlocked = useCallback(() => {
    window.alert(t("research.report.popup_blocked"));
  }, [t]);

  const maybeAutoOpenReport = useCallback(
    (run: ResearchRun) => {
      const md = run.reportMarkdown?.trim();
      if (!md || run.status !== "complete") return;
      const key = run.serverRunId ?? run.id;
      if (autoOpenedReportIdsRef.current.has(key)) return;
      autoOpenedReportIdsRef.current.add(key);
      const doc: ResearchReportDocument = {
        topic: run.topic,
        dateLabel: formatWhen(run.updatedAt || run.createdAt, locale),
        reportMarkdown: md,
        reportMeta: run.reportMeta,
        locale,
        displayTheme: reportTheme,
      };
      void openResearchReportWindow(doc).then((ok) => {
        if (!ok) reportExportBlocked();
      });
    },
    [locale, reportExportBlocked, reportTheme],
  );

  const upsertRun = useCallback(
    (run: ResearchRun) => {
      setHistory((prev) => {
        const next = [run, ...prev.filter((h) => h.id !== run.id)];
        saveHistory(next);
        return next;
      });
      setSelectedId(run.id);
      setActiveRun(run);
      maybeAutoOpenReport(run);
    },
    [maybeAutoOpenReport],
  );

  const selected = useMemo(() => {
    if (!selectedId) return null;
    if (activeRun?.id === selectedId) return activeRun;
    return history.find((h) => h.id === selectedId) ?? activeRun;
  }, [activeRun, history, selectedId]);

  const displayPhase: ResearchPhase = useMemo(() => {
    if (loading) return phase;
    if (selected?.status === "complete") return "done";
    if (selected?.status === "running") return phase;
    return "idle";
  }, [loading, phase, selected]);

  const phaseLabel = useMemo(() => {
    const key = phaseLabelKey(displayPhase);
    return key ? t(key) : null;
  }, [displayPhase, t]);

  useEffect(() => {
    if (draftModeRef.current) return;
    if (!selectedId && history.length > 0) setSelectedId(history[0].id);
  }, [history, selectedId]);

  const pollRun = useCallback(
    async (serverRunId: string, clientRun: ResearchRun) => {
      const res = await fetchEndpoint(`/api/research/deep/runs/${encodeURIComponent(serverRunId)}`);
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}`);
      }
      const server = JSON.parse(res.text) as ServerRun;
      const mapped = serverToRun(server, clientRun);
      upsertRun(mapped);
      setPhase(mapServerPhase(server.phase));
      return server;
    },
    [fetchEndpoint, upsertRun],
  );

  const stopPolling = () => {
    if (pollRef.current) {
      clearInterval(pollRef.current);
      pollRef.current = null;
    }
  };

  const watchResearchRun = useCallback(
    (serverRunId: string, initialRun: ResearchRun): Promise<void> => {
      serverRunIdRef.current = serverRunId;
      let current = initialRun;
      stopPolling();
      return new Promise<void>((resolve, reject) => {
        const tick = async () => {
          try {
            const server = await pollRun(serverRunId, current);
            current = serverToRun(server, current);
            if (server.phase === "done" || server.phase === "error" || server.phase === "cancelled") {
              stopPolling();
              if (server.phase === "error") {
                reject(new Error(server.error ?? (en ? "Research failed" : "Recherche échouée")));
              } else {
                if (server.phase === "done") setPhase("done");
                resolve();
              }
            }
          } catch (e) {
            stopPolling();
            reject(e);
          }
        };
        void tick();
        pollRef.current = setInterval(() => void tick(), POLL_MS);
      });
    },
    [pollRun, en],
  );

  /** After a full page reload, resume polling if a run was still in progress. */
  const resumeCheckedRef = useRef(false);
  useEffect(() => {
    if (resumeCheckedRef.current || pollRef.current) return;
    resumeCheckedRef.current = true;
    const running = loadHistory().find((h) => h.status === "running" && h.serverRunId);
    if (!running?.serverRunId) return;

    draftModeRef.current = false;
    setSelectedId(running.id);
    setActiveRun(running);
    setTopic(running.topic);
    setMaxRounds(running.maxRounds ?? 10);
    setLoading(true);
    setErr(null);
    setPhase("planning");

    void watchResearchRun(running.serverRunId, running)
      .catch((e) => {
        const message = formatDaemonError(e instanceof Error ? e.message : String(e), locale);
        setErr(message);
        upsertRun({
          ...running,
          updatedAt: new Date().toISOString(),
          status: "error",
          error: message,
        });
        setPhase("idle");
      })
      .finally(() => {
        setLoading(false);
        stopPolling();
      });
  }, [locale, upsertRun, watchResearchRun]);

  const cancelRun = async () => {
    const sid = serverRunIdRef.current;
    if (!sid) return;
    await fetchEndpoint(`/api/research/deep/runs/${encodeURIComponent(sid)}/cancel`, { method: "POST" });
    stopPolling();
    setLoading(false);
    setPhase("idle");
  };

  const run = async () => {
    const trimmed = topic.trim();
    if (!trimmed) return;

    draftModeRef.current = false;
    const runId = newId();
    const now = new Date().toISOString();
    let current: ResearchRun = {
      id: runId,
      topic: trimmed,
      maxRounds,
      createdAt: now,
      updatedAt: now,
      status: "running",
      branches: [],
    };
    upsertRun(current);
    setLoading(true);
    setErr(null);
    setPhase("planning");
    serverRunIdRef.current = null;
    stopPolling();

    try {
      const startRes = await fetchEndpoint("/api/research/deep/start", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ topic: trimmed, max_rounds: maxRounds, max_time_secs: 1200 }),
      });
      if (startRes.status === 404) {
        setApiReady(false);
        throw new Error(daemonOutdatedMsg);
      }
      if (!startRes.ok) {
        throw new Error(`HTTP ${startRes.status}`);
      }
      const { run_id: serverRunId } = JSON.parse(startRes.text) as { run_id: string };
      current = { ...current, serverRunId };
      upsertRun(current);

      await watchResearchRun(serverRunId, current);
    } catch (e) {
      const message = formatDaemonError(e instanceof Error ? e.message : String(e), locale);
      setErr(message);
      current = {
        ...current,
        updatedAt: new Date().toISOString(),
        status: "error",
        error: message,
      };
      upsertRun(current);
      setPhase("idle");
    } finally {
      setLoading(false);
      stopPolling();
    }
  };

  const deleteRun = (id: string) => {
    const next = history.filter((h) => h.id !== id);
    persist(next);
    if (selectedId === id) {
      if (activeRun?.id === id) setActiveRun(null);
      if (draftModeRef.current || next.length === 0) {
        setSelectedId(null);
      } else {
        setSelectedId(next[0].id);
      }
    }
  };

  const report = selected?.reportMarkdown ?? "";
  const evolving = selected?.evolvingReport ?? "";

  const reportDoc: ResearchReportDocument | null = useMemo(() => {
    if (!report.trim() || !selected) return null;
    const dateIso = selected.updatedAt || selected.createdAt;
    return {
      topic: selected.topic,
      dateLabel: formatWhen(dateIso, locale),
      reportMarkdown: report,
      reportMeta: selected.reportMeta,
      locale,
      displayTheme: reportTheme,
    };
  }, [report, selected, locale, reportTheme]);

  const startNewResearch = () => {
    draftModeRef.current = true;
    stopPolling();
    serverRunIdRef.current = null;
    setLoading(false);
    setSelectedId(null);
    setActiveRun(null);
    setTopic("");
    setMaxRounds(10);
    setErr(null);
    setPhase("idle");
  };

  const selectHistoryRun = (h: ResearchRun) => {
    draftModeRef.current = false;
    setSelectedId(h.id);
    setActiveRun(null);
    setTopic(h.topic);
    setMaxRounds(h.maxRounds ?? 10);
    setErr(null);
    setPhase(h.status === "complete" ? "done" : h.status === "running" ? "steps" : "idle");
  };

  const onOpenReportWindow = useCallback(() => {
    if (!reportDoc) return;
    void openResearchReportWindow(reportDoc).then((ok) => {
      if (!ok) reportExportBlocked();
    });
  }, [reportDoc, reportExportBlocked]);

  const onExportHtml = useCallback(() => {
    if (!reportDoc) return;
    void downloadResearchReportHtml(reportDoc);
  }, [reportDoc]);

  const onExportPdf = useCallback(() => {
    if (!reportDoc) return;
    void exportResearchReportPdf(reportDoc, {
      hint: t("research.report.print_hint"),
      buttonLabel: t("research.report.print_button"),
    }).then((ok) => {
      if (!ok) reportExportBlocked();
    });
  }, [reportDoc, t, reportExportBlocked]);

  const inDraft = selectedId === null;
  const branches = inDraft ? [] : (selected?.branches ?? []);
  const reportReady = !inDraft && Boolean(report.trim() && !loading);

  return (
    <section className="workspace-panel research-panel">
      <div className="research-layout">
        <aside className="research-history" aria-label={en ? "Research history" : "Historique des recherches"}>
          <div className="research-history-head">
            <h3>{en ? "Past research" : "Recherches passées"}</h3>
            <span className="muted">{history.length}</span>
          </div>
          {history.length === 0 ? (
            <p className="muted research-history-empty">
              {en ? "Completed runs appear here." : "Les recherches terminées apparaissent ici."}
            </p>
          ) : (
            <ul className="research-history-list">
              {history.map((h) => (
                <li key={h.id}>
                  <button
                    type="button"
                    className={`research-history-item ${!inDraft && selectedId === h.id ? "active" : ""} research-history-item-${h.status}`}
                    onClick={() => selectHistoryRun(h)}
                  >
                    <span className="research-history-title">
                      {h.reportMeta?.report_title?.trim() || h.topic}
                    </span>
                    <span className="research-history-meta">
                      {formatWhen(h.updatedAt, locale)}
                      {h.reportMeta
                        ? ` · ${h.reportMeta.rounds}/${h.maxRounds} ${en ? "rounds" : "tours"} · ${h.reportMeta.word_count} ${en ? "words" : "mots"}`
                        : ` · max ${h.maxRounds} ${en ? "rounds" : "tours"}`}
                      {h.status === "running" ? (en ? " · running" : " · en cours") : null}
                      {h.status === "error" ? (en ? " · error" : " · erreur") : null}
                    </span>
                  </button>
                  <button
                    type="button"
                    className="research-history-delete"
                    aria-label={en ? "Delete" : "Supprimer"}
                    onClick={() => deleteRun(h.id)}
                  >
                    ×
                  </button>
                </li>
              ))}
            </ul>
          )}
        </aside>

        <div className="research-main">
          <header className="research-main-head">
            <div className="research-main-head-actions">
              <button
                type="button"
                className="panel-hero-action research-new-btn-main"
                onClick={startNewResearch}
                disabled={loading}
              >
                {t("research.new_research")}
              </button>
              {reportReady ? (
                <div
                  className="research-report-toolbar"
                  role="toolbar"
                  aria-label={t("research.report.actions_label")}
                >
                  <button
                    type="button"
                    className="panel-hero-action panel-hero-action-secondary"
                    onClick={onOpenReportWindow}
                  >
                    {t("research.report.reopen")}
                  </button>
                  <button
                    type="button"
                    className="panel-hero-action panel-hero-action-secondary"
                    onClick={onExportHtml}
                  >
                    {t("research.report.export_html")}
                  </button>
                  <button
                    type="button"
                    className="panel-hero-action panel-hero-action-secondary"
                    onClick={onExportPdf}
                  >
                    {t("research.report.export_pdf")}
                  </button>
                  {onDiscussReport && reportDoc ? (
                    <button
                      type="button"
                      className="panel-hero-action panel-hero-action-secondary"
                      onClick={() => onDiscussReport(reportDoc)}
                    >
                      {en ? "Discuss in chat" : "Discuter dans le chat"}
                    </button>
                  ) : null}
                </div>
              ) : null}
            </div>
            <div className="research-main-head-meta">
              <label className="research-theme-field">
                <span>{t("research.report.theme_label")}</span>
                <select
                  value={reportTheme}
                  onChange={(e) => {
                    const next = e.target.value as ResearchReportTheme;
                    setReportTheme(next);
                    saveResearchReportTheme(next);
                  }}
                  aria-label={t("research.report.theme_label")}
                >
                  {RESEARCH_REPORT_THEME_OPTIONS.map((opt) => (
                    <option key={opt.id} value={opt.id}>
                      {en ? opt.labelEn : opt.labelFr}
                    </option>
                  ))}
                </select>
              </label>
              {reportReady ? (
                <p className="research-report-ready-hint muted">{t("research.report.ready")}</p>
              ) : null}
            </div>
          </header>
          {apiReady === false ? (
            <div className="research-api-banner" role="alert">
              {daemonOutdatedMsg}
            </div>
          ) : null}
          {!inDraft && selected?.searchDegraded ? (
            <div className="research-api-banner research-degraded-banner" role="status">
              {t("research.search_degraded")}
            </div>
          ) : null}
          {loading ? (
            <p className="research-slow-hint muted">
              {t("research.slow_hint")}
              {!inDraft ? (
                <span className="research-background-hint"> {t("research.background_hint")}</span>
              ) : null}
            </p>
          ) : null}
          <p className="panel-hero-text muted">{t("research.hero")}</p>

          <ResearchStarViz
            branches={branches}
            phase={inDraft && !loading ? "idle" : displayPhase}
            phaseLabel={inDraft && !loading ? undefined : (phaseLabel ?? undefined)}
            topic={loading ? topic.trim() : inDraft ? undefined : selected?.topic}
          />

          {branches.length > 0 ? (
            <ol className="research-step-list">
              {branches.map((b, i) => (
                <li key={i} className={`research-step-item research-step-${b.status}`}>
                  <span className="research-step-badge">{i + 1}</span>
                  <span className="research-step-label">{b.label}</span>
                  {b.subAgents && b.subAgents.length > 0 ? (
                    <ul className="research-subagent-list">
                      {b.subAgents.map((a) => (
                        <li key={a.id} className={`research-subagent research-subagent-${a.status}`}>
                          <span className={`research-subagent-role research-subagent-role-${a.role}`}>
                            {t(`research.role.${a.role}`)}
                          </span>
                          <span className="research-subagent-label">{a.label}</span>
                        </li>
                      ))}
                    </ul>
                  ) : null}
                </li>
              ))}
            </ol>
          ) : null}

          {evolving && loading ? (
            <details className="research-evolving">
              <summary>{t("research.evolving_summary")}</summary>
              <div className="research-evolving-body markdown-rendered">
                <Suspense fallback={<span>…</span>}>
                  <LazyMarkdownContent>{evolving}</LazyMarkdownContent>
                </Suspense>
              </div>
            </details>
          ) : null}

          <label className="settings-field">
            <span>{t("research.topic_label")}</span>
            <textarea value={topic} onChange={(e) => setTopic(e.target.value)} rows={3} disabled={loading} />
          </label>
          <label className="settings-field research-max-rounds-field">
            <span className="research-max-rounds-label">
              {t("research.max_rounds_label")}
              <button
                type="button"
                className="research-field-tip"
                title={t("research.max_rounds_help")}
                aria-label={t("research.max_rounds_help")}
              >
                ?
              </button>
            </span>
            <input
              type="number"
              min={1}
              max={16}
              value={maxRounds}
              onChange={(e) => setMaxRounds(Math.min(16, Math.max(1, Number(e.target.value) || 10)))}
              disabled={loading}
              aria-describedby="research-max-rounds-help"
            />
            <span id="research-max-rounds-help" className="sr-only">
              {t("research.max_rounds_help")}
            </span>
            {selected?.reportMeta && !loading ? (
              <span className="research-max-rounds-used muted">
                {en
                  ? `Last run: ${selected.reportMeta.rounds} of ${selected.maxRounds} rounds used`
                  : `Dernière recherche : ${selected.reportMeta.rounds} / ${selected.maxRounds} tours utilisés`}
              </span>
            ) : null}
          </label>
          <div className="research-actions">
            <button
              type="button"
              className="btn-primary"
              onClick={() => void run()}
              disabled={loading || !topic.trim() || apiReady === false}
            >
              {loading
                ? phaseLabel ?? (en ? "Researching…" : "Recherche…")
                : en
                  ? "Run research"
                  : "Lancer la recherche"}
            </button>
            {loading ? (
              <button type="button" className="panel-hero-action panel-hero-action-secondary" onClick={() => void cancelRun()}>
                {en ? "Cancel" : "Annuler"}
              </button>
            ) : null}
          </div>
          {err ? <p className="settings-plugin-reputation-feedback settings-plugin-reputation-feedback-err">{err}</p> : null}
          {!inDraft && selected?.error && !loading ? (
            <p className="settings-plugin-reputation-feedback settings-plugin-reputation-feedback-err">{selected.error}</p>
          ) : null}
        </div>
      </div>
    </section>
  );
}
