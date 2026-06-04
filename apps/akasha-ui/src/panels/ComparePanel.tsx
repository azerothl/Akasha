import { Suspense, lazy, useCallback, useEffect, useMemo, useState } from "react";

const LazyMarkdownContent = lazy(() => import("../MarkdownContent").then((m) => ({ default: m.default })));

type ModelEntry = { provider: string; model: string; label?: string };

type RouterModels = Record<string, string[]>;

type RouterModelsResponse = {
  providers?: RouterModels;
};

function parseRouterModels(raw: unknown): RouterModels {
  if (!raw || typeof raw !== "object") return {};
  const obj = raw as RouterModelsResponse & RouterModels;
  const source = obj.providers && typeof obj.providers === "object" ? obj.providers : obj;
  const out: RouterModels = {};
  for (const [provider, models] of Object.entries(source)) {
    if (provider === "providers") continue;
    if (!Array.isArray(models)) continue;
    const list = models.filter((m): m is string => typeof m === "string" && m.trim().length > 0);
    if (list.length > 0) out[provider] = list;
  }
  return out;
}

type CompareResult = {
  label: string;
  text?: string;
  error?: string;
  ok: boolean;
  provider?: string | null;
  model?: string | null;
  latency_ms?: number;
};

type SelectedRow = {
  id: string;
  provider: string;
  model: string;
};

const MAX_MODELS = 6;

type Props = {
  fetchEndpoint: (path: string, init?: RequestInit) => Promise<{ ok: boolean; status: number; text: string }>;
  locale: "fr" | "en";
  defaultModels?: ModelEntry[];
};

function nextRowId(): string {
  return `row-${Date.now()}-${Math.random().toString(36).slice(2, 7)}`;
}

function flattenRouterModels(providers: RouterModels): { provider: string; model: string }[] {
  const out: { provider: string; model: string }[] = [];
  for (const provider of Object.keys(providers).sort((a, b) => a.localeCompare(b))) {
    for (const model of providers[provider] ?? []) {
      if (model.trim()) out.push({ provider, model });
    }
  }
  return out;
}

function pickInitialRows(
  routerModels: RouterModels,
  defaultModels: ModelEntry[] | undefined,
): SelectedRow[] {
  const all = flattenRouterModels(routerModels);
  if (all.length === 0) return [];

  const defaults = defaultModels ?? [
    { provider: "akasha_embedded", model: "default" },
    { provider: "akasha_core", model: "core" },
  ];

  const validDefaults = defaults.filter(
    (d) => routerModels[d.provider]?.includes(d.model),
  );
  const seed = validDefaults.length > 0 ? validDefaults : all;
  const count = Math.min(2, seed.length, all.length);

  return seed.slice(0, count).map((entry) => ({
    id: nextRowId(),
    provider: entry.provider,
    model: entry.model,
  }));
}

function pickUnusedPair(
  all: { provider: string; model: string }[],
  used: Set<string>,
): { provider: string; model: string } | null {
  for (const pair of all) {
    const key = `${pair.provider}/${pair.model}`;
    if (!used.has(key)) return pair;
  }
  return all[0] ?? null;
}

export function ComparePanel({ fetchEndpoint, locale, defaultModels }: Props) {
  const en = locale === "en";
  const [prompt, setPrompt] = useState("");
  const [blind, setBlind] = useState(true);
  const [routerModels, setRouterModels] = useState<RouterModels | null>(null);
  const [routerLoadError, setRouterLoadError] = useState<string | null>(null);
  const [selectedRows, setSelectedRows] = useState<SelectedRow[]>([]);
  const [loading, setLoading] = useState(false);
  const [results, setResults] = useState<CompareResult[]>([]);
  const [synthesis, setSynthesis] = useState("");

  const providerList = useMemo(() => {
    if (!routerModels) return [];
    return Object.keys(routerModels).sort((a, b) => a.localeCompare(b));
  }, [routerModels]);

  const allPairs = useMemo(() => flattenRouterModels(routerModels ?? {}), [routerModels]);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const res = await fetchEndpoint("/api/router/models");
        if (cancelled) return;
        if (!res.ok) {
          setRouterLoadError(`HTTP ${res.status}`);
          return;
        }
        const parsed = parseRouterModels(JSON.parse(res.text) as unknown);
        setRouterModels(parsed);
        setRouterLoadError(null);
      } catch (e) {
        if (!cancelled) {
          setRouterLoadError(e instanceof Error ? e.message : String(e));
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [fetchEndpoint]);

  useEffect(() => {
    if (!routerModels || selectedRows.length > 0) return;
    setSelectedRows(pickInitialRows(routerModels, defaultModels));
  }, [routerModels, defaultModels, selectedRows.length]);

  const updateRow = useCallback(
    (id: string, patch: Partial<Pick<SelectedRow, "provider" | "model">>) => {
      setSelectedRows((rows) =>
        rows.map((row) => {
          if (row.id !== id) return row;
          const provider = patch.provider ?? row.provider;
          let model = patch.model ?? row.model;
          if (patch.provider && routerModels) {
            const models = routerModels[provider] ?? [];
            if (!models.includes(model)) {
              model = models[0] ?? "";
            }
          }
          return { ...row, provider, model };
        }),
      );
    },
    [routerModels],
  );

  const addRow = useCallback(() => {
    if (selectedRows.length >= MAX_MODELS) return;
    const used = new Set(selectedRows.map((r) => `${r.provider}/${r.model}`));
    const next = pickUnusedPair(allPairs, used);
    if (!next) return;
    setSelectedRows((rows) => [...rows, { id: nextRowId(), ...next }]);
  }, [allPairs, selectedRows]);

  const removeRow = useCallback((id: string) => {
    setSelectedRows((rows) => (rows.length <= 1 ? rows : rows.filter((r) => r.id !== id)));
  }, []);

  const modelsForRun = useMemo(
    () =>
      selectedRows
        .map((r) => ({ provider: r.provider.trim(), model: r.model.trim() }))
        .filter((m) => m.provider && m.model),
    [selectedRows],
  );

  const canRun = prompt.trim().length > 0 && modelsForRun.length > 0 && !loading && routerModels != null;

  const run = async () => {
    if (!canRun) return;
    setLoading(true);
    setResults([]);
    setSynthesis("");
    try {
      const res = await fetchEndpoint("/api/compare", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ prompt: prompt.trim(), blind, models: modelsForRun }),
      });
      if (!res.ok) {
        setSynthesis(`${en ? "Error" : "Erreur"} HTTP ${res.status}: ${res.text.slice(0, 200)}`);
        return;
      }
      const j = JSON.parse(res.text) as { results?: CompareResult[]; synthesis?: string };
      setResults(j.results ?? []);
      setSynthesis(j.synthesis ?? "");
    } catch (e) {
      const raw = e instanceof Error ? e.message : String(e);
      const lower = raw.toLowerCase();
      const timedOut = lower.includes("timed out") || lower.includes("timeout") || lower.includes("time out");
      setSynthesis(
        timedOut
          ? en
            ? "Request timed out — comparing multiple models with the embedded LLM can take several minutes. Try fewer models or configure a faster provider."
            : "Délai dépassé — la comparaison de plusieurs modèles avec le LLM embarqué peut prendre plusieurs minutes. Essayez moins de modèles ou un provider plus rapide."
          : raw,
      );
    } finally {
      setLoading(false);
    }
  };

  return (
    <section className="workspace-panel compare-panel">
      <div className="compare-form">
        <p className="panel-hero-text muted">
          {en
            ? "Send one prompt to several models side by side. Models are loaded from llm_router.yaml."
            : "Envoyez un prompt à plusieurs modèles côte à côte. Les modèles proviennent de llm_router.yaml."}
        </p>

        <label className="settings-field compare-prompt-field">
          <span>{en ? "Prompt" : "Prompt"}</span>
          <textarea
            className="compare-prompt-input"
            value={prompt}
            onChange={(e) => setPrompt(e.target.value)}
            disabled={loading}
            placeholder={en ? "Your comparison prompt…" : "Votre prompt de comparaison…"}
          />
        </label>

        <fieldset className="compare-models-field" disabled={loading || !routerModels}>
          <legend>{en ? "Models to compare" : "Modèles à comparer"}</legend>
          {routerLoadError ? (
            <p className="compare-models-hint compare-models-error" role="alert">
              {en ? "Could not load router models:" : "Impossible de charger les modèles du routeur :"}{" "}
              {routerLoadError}
            </p>
          ) : null}
          {!routerModels && !routerLoadError ? (
            <p className="compare-models-hint muted">{en ? "Loading models…" : "Chargement des modèles…"}</p>
          ) : null}
          {routerModels && allPairs.length === 0 ? (
            <p className="compare-models-hint muted">
              {en
                ? "No models in llm_router.yaml. Configure providers first."
                : "Aucun modèle dans llm_router.yaml. Configurez d'abord les providers."}
            </p>
          ) : null}
          {selectedRows.map((row, index) => {
            const modelsForProvider = routerModels?.[row.provider] ?? [];
            return (
              <div key={row.id} className="compare-model-row">
                <span className="compare-model-index" aria-hidden>
                  {index + 1}
                </span>
                <select
                  className="compare-model-select"
                  value={row.provider}
                  onChange={(e) => updateRow(row.id, { provider: e.target.value })}
                  aria-label={en ? `Provider ${index + 1}` : `Provider ${index + 1}`}
                >
                  {providerList.map((p) => (
                    <option key={p} value={p}>
                      {p}
                    </option>
                  ))}
                </select>
                <select
                  className="compare-model-select"
                  value={row.model}
                  onChange={(e) => updateRow(row.id, { model: e.target.value })}
                  aria-label={en ? `Model ${index + 1}` : `Modèle ${index + 1}`}
                >
                  {modelsForProvider.map((m) => (
                    <option key={m} value={m}>
                      {m}
                    </option>
                  ))}
                </select>
                <button
                  type="button"
                  className="compare-model-remove"
                  onClick={() => removeRow(row.id)}
                  disabled={selectedRows.length <= 1 || loading}
                  aria-label={en ? `Remove model ${index + 1}` : `Retirer le modèle ${index + 1}`}
                  title={en ? "Remove" : "Retirer"}
                >
                  ×
                </button>
              </div>
            );
          })}
          <div className="compare-models-toolbar">
            <button
              type="button"
              className="btn-secondary compare-add-model"
              onClick={addRow}
              disabled={loading || selectedRows.length >= MAX_MODELS || allPairs.length === 0}
            >
              {en ? "Add model" : "Ajouter un modèle"}
            </button>
            <span className="compare-models-hint muted">
              {en
                ? `${selectedRows.length}/${MAX_MODELS} · blind mode hides provider names`
                : `${selectedRows.length}/${MAX_MODELS} · le mode aveugle masque les noms des providers`}
            </span>
          </div>
        </fieldset>

        <div className="compare-form-actions">
          <label className="settings-field compare-blind-field">
            <input type="checkbox" checked={blind} onChange={(e) => setBlind(e.target.checked)} disabled={loading} />
            {en ? "Blind test" : "Test aveugle"}
          </label>
          <button type="button" className="btn-primary" onClick={() => void run()} disabled={!canRun}>
            {loading ? (en ? "Running…" : "En cours…") : en ? "Compare" : "Comparer"}
          </button>
        </div>
      </div>

      <div className="compare-results-scroll">
        {results.length > 0 ? (
          <div className="compare-grid">
            {results.map((r, i) => (
              <article key={i} className={`compare-card ${r.ok ? "" : "compare-card-error"}`}>
                <h4>{r.label}</h4>
                {!blind && r.provider ? (
                  <p className="muted compare-card-meta">
                    {r.provider}/{r.model}
                    {r.latency_ms != null ? ` · ${r.latency_ms} ms` : ""}
                  </p>
                ) : null}
                <div className="compare-card-body markdown-rendered">
                  {r.ok && r.text ? (
                    <Suspense fallback={<span>…</span>}>
                      <LazyMarkdownContent>{r.text}</LazyMarkdownContent>
                    </Suspense>
                  ) : (
                    <p className="muted">{r.error ?? (en ? "Failed" : "Échec")}</p>
                  )}
                </div>
              </article>
            ))}
          </div>
        ) : null}
        {synthesis ? (
          <div className="compare-synthesis">
            <h4>{en ? "Synthesis" : "Synthèse"}</h4>
            <Suspense fallback={<span>…</span>}>
              <LazyMarkdownContent>{synthesis}</LazyMarkdownContent>
            </Suspense>
          </div>
        ) : null}
      </div>
    </section>
  );
}
