import { Fragment, useCallback, useEffect, useMemo, useState } from "react";
import { InfoTip } from "../components/Tooltip";
import { useNotifyOnMessage } from "../notifications/useNotifyOnMessage";
import { useI18n } from "../useI18n";
import {
  DEFAULT_COOKBOOK_FILTERS,
  filterCookbookItems,
  uniqueProviders,
  type CookbookFilters,
  type CookbookItem,
  type LocalRuntimeInfo,
} from "../cookbookFilters";
import {
  boolGlyph,
  detailOf,
  fitHeatClass,
  formatContext,
  formatModelSize,
  formatPricePerMillion,
  formatSizeGb,
  providerColor,
  sortByFitDesc,
} from "../cookbookMatrix";
import { CookbookRecipesView, type CookbookRecipeHandlers } from "./CookbookRecipesView";

type Props = {
  fetchEndpoint: (path: string, init?: RequestInit) => Promise<{ ok: boolean; status: number; text: string }>;
  locale: "fr" | "en";
  subView?: "models" | "recipes";
  onSubViewChange?: (view: "models" | "recipes") => void;
  recipeHandlers?: CookbookRecipeHandlers;
};

type LocalRuntimes = {
  ollama?: LocalRuntimeInfo;
  rbitnet?: LocalRuntimeInfo;
};

type AddRouteOptions = {
  category: string;
  role: "primary" | "fallback";
  pullOllama?: boolean;
  pullRbitnet?: boolean;
  routeProvider?: string;
  routeModel?: string;
};

const MODEL_TYPE_OPTIONS = ["all", "local", "cloud", "embedded", "gguf", "code", "vision", "instruct", "embedding", "other"] as const;
const SOURCE_OPTIONS = ["all", "configured_route", "provider_catalog", "suggestion", "huggingface"] as const;
const LOCAL_INSTALL_OPTIONS = ["all", "local", "installed", "missing"] as const;

function RuntimeStatus({ runtimes, en }: { runtimes: LocalRuntimes | null; en: boolean }) {
  if (!runtimes) return null;
  return (
    <div className="cookbook-runtime-status" role="status">
      {(["ollama", "rbitnet"] as const).map((key) => {
        const rt = runtimes[key];
        const running = Boolean(rt?.running);
        const label = key === "ollama" ? "Ollama" : "Rbitnet";
        const count = rt?.models?.length ?? 0;
        return (
          <span
            key={key}
            className={`cookbook-runtime-pill ${running ? "cookbook-runtime-on" : "cookbook-runtime-off"}`}
            title={rt?.base_url ?? (running ? label : en ? `${label} not reachable` : `${label} injoignable`)}
          >
            <span className="cookbook-runtime-dot" aria-hidden />
            {label}
            {running ? ` · ${count}` : en ? " · off" : " · arrêté"}
          </span>
        );
      })}
    </div>
  );
}

function LocalInstallBadges({ item, en }: { item: CookbookItem; en: boolean }) {
  const li = item.local_install;
  if (!li?.ollama && !li?.rbitnet) return null;
  return (
    <span className="cookbook-local-badges">
      {li.ollama ? (
        <span className="cookbook-local-icon cookbook-local-ollama" title={en ? "Installed on Ollama" : "Installé sur Ollama"}>
          O
        </span>
      ) : null}
      {li.rbitnet ? (
        <span className="cookbook-local-icon cookbook-local-rbitnet" title={en ? "On Rbitnet" : "Sur Rbitnet"}>
          R
        </span>
      ) : null}
    </span>
  );
}

function CapabilityCell({ value, title }: { value?: boolean; title: string }) {
  return (
    <td className={`cookbook-matrix-cap ${value ? "cookbook-cap-yes" : "cookbook-cap-no"}`} title={title}>
      {boolGlyph(value)}
    </td>
  );
}

function ModelLinksInline({ item }: { item: CookbookItem }) {
  const links = item.links?.length ? item.links : item.url ? [{ label: "↗", url: item.url }] : [];
  if (links.length === 0) return null;
  return (
    <span className="cookbook-matrix-links">
      {links.map((link) => (
        <a key={link.url} href={link.url} target="_blank" rel="noreferrer" title={link.label} className="cookbook-info-link">
          {link.label}
        </a>
      ))}
    </span>
  );
}

function AddRouteForm({
  item,
  taskCategories,
  registeredProviders,
  localRuntimes,
  en,
  onAdd,
  busy,
}: {
  item: CookbookItem;
  taskCategories: string[];
  registeredProviders: string[];
  localRuntimes: LocalRuntimes | null;
  en: boolean;
  onAdd: (opts: AddRouteOptions) => void;
  busy: boolean;
}) {
  const [category, setCategory] = useState(taskCategories[0] ?? "conversation");
  const [role, setRole] = useState<"primary" | "fallback">("primary");
  const [pullOllama, setPullOllama] = useState(true);
  const [pullRbitnet, setPullRbitnet] = useState(true);
  const [routeVia, setRouteVia] = useState<"ollama" | "bitnet" | "direct">(
    item.provider === "huggingface"
      ? registeredProviders.includes("ollama")
        ? "ollama"
        : registeredProviders.includes("bitnet")
          ? "bitnet"
          : "direct"
      : "direct",
  );

  const ollamaUp = Boolean(localRuntimes?.ollama?.running);
  const rbitnetUp = Boolean(localRuntimes?.rbitnet?.running);
  const ollamaPullName = item.pull_hints?.ollama ?? item.model;
  const rbitnetPullName = item.pull_hints?.rbitnet ?? item.model;
  const isHf = item.provider === "huggingface";
  const canRouteOllama = registeredProviders.includes("ollama");
  const canRouteBitnet = registeredProviders.includes("bitnet");
  const canRouteDirect = registeredProviders.includes(item.provider);

  const showOllamaPull =
    ollamaUp && !item.local_install?.ollama && (item.provider === "ollama" || isHf);
  const showRbitnetPull =
    rbitnetUp && !item.local_install?.rbitnet && (item.provider === "bitnet" || isHf);

  if (!canRouteDirect && !isHf) {
    return (
      <p className="muted cookbook-add-hint">
        {en
          ? "Register this provider in llm_router.yaml before routing."
          : "Enregistrez ce provider dans llm_router.yaml avant de router."}
      </p>
    );
  }

  if (isHf && !canRouteOllama && !canRouteBitnet) {
    return (
      <p className="muted cookbook-add-hint">
        {en
          ? "Add ollama or bitnet provider in llm_router.yaml to route Hugging Face models locally."
          : "Ajoutez ollama ou bitnet dans llm_router.yaml pour router les modèles Hugging Face."}
      </p>
    );
  }

  const resolveRoute = (): { provider: string; model: string } => {
    if (isHf) {
      if (routeVia === "ollama" && canRouteOllama) {
        return { provider: "ollama", model: ollamaPullName };
      }
      if (routeVia === "bitnet" && canRouteBitnet) {
        return { provider: "bitnet", model: rbitnetPullName };
      }
    }
    return { provider: item.provider, model: item.model };
  };

  return (
    <div className="cookbook-add-route">
      {isHf && (canRouteOllama || canRouteBitnet) ? (
        <label className="cookbook-add-label">
          {en ? "Route via" : "Router via"}
          <select value={routeVia} onChange={(e) => setRouteVia(e.target.value as "ollama" | "bitnet" | "direct")} className="cookbook-add-select">
            {canRouteOllama ? <option value="ollama">Ollama</option> : null}
            {canRouteBitnet ? <option value="bitnet">Rbitnet / bitnet</option> : null}
          </select>
        </label>
      ) : null}
      <label className="cookbook-add-label">
        {en ? "Task type" : "Type de tâche"}
        <select value={category} onChange={(e) => setCategory(e.target.value)} className="cookbook-add-select">
          {taskCategories.map((c) => (
            <option key={c} value={c}>
              {c}
            </option>
          ))}
        </select>
      </label>
      <label className="cookbook-add-label">
        {en ? "Role" : "Rôle"}
        <select value={role} onChange={(e) => setRole(e.target.value as "primary" | "fallback")} className="cookbook-add-select">
          <option value="primary">{en ? "Primary" : "Primaire"}</option>
          <option value="fallback">{en ? "Fallback" : "Secondaire"}</option>
        </select>
      </label>
      {showOllamaPull && (isHf ? routeVia === "ollama" : true) ? (
        <label className="cookbook-add-check">
          <input type="checkbox" checked={pullOllama} onChange={(e) => setPullOllama(e.target.checked)} />
          {en ? `Pull to Ollama: ${ollamaPullName}` : `Télécharger Ollama : ${ollamaPullName}`}
        </label>
      ) : null}
      {showRbitnetPull && (isHf ? routeVia === "bitnet" : true) ? (
        <label className="cookbook-add-check">
          <input type="checkbox" checked={pullRbitnet} onChange={(e) => setPullRbitnet(e.target.checked)} />
          {en ? `Install on Rbitnet: ${rbitnetPullName}` : `Installer Rbitnet : ${rbitnetPullName}`}
        </label>
      ) : null}
      <button
        type="button"
        className="cookbook-add-btn"
        disabled={busy || !category}
        onClick={() => {
          const route = resolveRoute();
          onAdd({
            category,
            role,
            pullOllama: showOllamaPull && pullOllama && (isHf ? routeVia === "ollama" : item.provider === "ollama"),
            pullRbitnet: showRbitnetPull && pullRbitnet && (isHf ? routeVia === "bitnet" : item.provider === "bitnet"),
            routeProvider: route.provider,
            routeModel: route.model,
          });
        }}
      >
        {busy ? (en ? "Saving…" : "Enregistrement…") : en ? "Add to llm_router" : "Ajouter à llm_router"}
      </button>
    </div>
  );
}

export function CookbookPanel({
  fetchEndpoint,
  locale,
  subView = "models",
  onSubViewChange,
  recipeHandlers,
}: Props) {
  const { t } = useI18n();
  const en = locale === "en";
  const [hardware, setHardware] = useState<Record<string, unknown> | null>(null);
  const [localRuntimes, setLocalRuntimes] = useState<LocalRuntimes | null>(null);
  const [allItems, setAllItems] = useState<CookbookItem[]>([]);
  const [taskCategories, setTaskCategories] = useState<string[]>([]);
  const [registeredProviders, setRegisteredProviders] = useState<string[]>([]);
  const [filters, setFilters] = useState<CookbookFilters>(DEFAULT_COOKBOOK_FILTERS);
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [routeBusy, setRouteBusy] = useState(false);
  const [routeMsg, setRouteMsg] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);

  useNotifyOnMessage(err, "error", t("tabs.cookbook"));

  const loadData = useCallback(async () => {
    const res = await fetchEndpoint("/api/cookbook/recommendations");
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const j = JSON.parse(res.text) as {
      hardware?: Record<string, unknown>;
      local_runtimes?: LocalRuntimes;
      recommendations?: CookbookItem[];
      suggestions?: CookbookItem[];
      huggingface_local?: CookbookItem[];
      task_categories?: string[];
      registered_providers?: string[];
    };
    setHardware(j.hardware ?? null);
    setLocalRuntimes(j.local_runtimes ?? null);
    setTaskCategories(j.task_categories ?? []);
    setRegisteredProviders(j.registered_providers ?? []);
    const merged: CookbookItem[] = [
      ...(j.recommendations ?? []),
      ...(j.suggestions ?? []),
      ...(j.huggingface_local ?? []),
    ];
    setAllItems(sortByFitDesc(merged));
  }, [fetchEndpoint]);

  useEffect(() => {
    void (async () => {
      try {
        await loadData();
      } catch (e) {
        setErr(e instanceof Error ? e.message : String(e));
      }
    })();
  }, [loadData]);

  const providers = useMemo(() => uniqueProviders(allItems), [allItems]);
  const filtered = useMemo(() => sortByFitDesc(filterCookbookItems(allItems, filters)), [allItems, filters]);

  const pullLocalModel = async (runtime: "ollama" | "rbitnet", model: string) => {
    const res = await fetchEndpoint("/api/cookbook/local/pull", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ runtime, model }),
    });
    const j = JSON.parse(res.text) as { ok?: boolean; error?: string; message?: string };
    if (!res.ok || !j.ok) throw new Error(j.error ?? `HTTP ${res.status}`);
    return j.message ?? "";
  };

  const addRouteDirect = async (opts: {
    category: string;
    provider: string;
    model: string;
    role: "primary" | "fallback";
  }) => {
    const res = await fetchEndpoint("/api/router/route", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(opts),
    });
    const j = JSON.parse(res.text) as { ok?: boolean; error?: string; message?: string };
    if (!res.ok || !j.ok) throw new Error(j.error ?? `HTTP ${res.status}`);
    await loadData();
    return j.message ?? (en ? "Route saved." : "Route enregistrée.");
  };

  const installSkillDirect = async (url: string) => {
    const res = await fetchEndpoint("/api/skills/install", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ url }),
    });
    const j = JSON.parse(res.text) as { installed?: boolean; message?: string; error?: string; detail?: string };
    if (!res.ok || !j.installed) throw new Error(j.detail ?? j.error ?? `HTTP ${res.status}`);
    return j.message ?? (en ? "Skill installed." : "Skill installé.");
  };

  const defaultRecipeHandlers: CookbookRecipeHandlers = {
    onTryInChat: () => {},
    onOpenCompare: () => {},
    onOpenModelsTab: () => onSubViewChange?.("models"),
    onPullModel: pullLocalModel,
    onAddRoute: addRouteDirect,
    onInstallSkill: installSkillDirect,
  };

  const handlers = recipeHandlers ?? defaultRecipeHandlers;

  const addToRouter = async (item: CookbookItem, opts: AddRouteOptions) => {
    setRouteBusy(true);
    setRouteMsg(null);
    try {
      const provider = opts.routeProvider ?? item.provider;
      const model = opts.routeModel ?? item.model;
      if (opts.pullOllama) {
        const name = item.pull_hints?.ollama ?? model;
        setRouteMsg(en ? `Pulling ${name} on Ollama…` : `Téléchargement ${name} sur Ollama…`);
        const msg = await pullLocalModel("ollama", name);
        setRouteMsg(msg);
      }
      if (opts.pullRbitnet) {
        const name = item.pull_hints?.rbitnet ?? model;
        setRouteMsg(en ? `Installing ${name} on Rbitnet…` : `Installation ${name} sur Rbitnet…`);
        const msg = await pullLocalModel("rbitnet", name);
        setRouteMsg(msg);
      }
      const res = await fetchEndpoint("/api/router/route", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ category: opts.category, provider, model, role: opts.role }),
      });
      const j = JSON.parse(res.text) as { ok?: boolean; error?: string; message?: string };
      if (!res.ok || !j.ok) throw new Error(j.error ?? `HTTP ${res.status}`);
      setRouteMsg(j.message ?? (en ? "Route saved." : "Route enregistrée."));
      await loadData();
      setExpandedId(null);
    } catch (e) {
      setRouteMsg(e instanceof Error ? e.message : String(e));
    } finally {
      setRouteBusy(false);
    }
  };

  const sourceLabel = (s: string) => {
    if (s === "all") return en ? "All sources" : "Toutes sources";
    if (s === "configured_route") return en ? "Routed" : "Routé";
    if (s === "provider_catalog") return en ? "Catalog" : "Catalogue";
    if (s === "suggestion") return en ? "Suggestions" : "Suggestions";
    if (s === "huggingface") return "Hugging Face";
    return s;
  };

  const typeLabel = (s: string) => (s === "all" ? (en ? "All types" : "Tous types") : s);

  const localInstallLabel = (s: string) => {
    if (s === "all") return en ? "All" : "Tous";
    if (s === "local") return en ? "Local installable" : "Installable localement";
    if (s === "installed") return en ? "Already installed" : "Déjà installé";
    if (s === "missing") return en ? "Not installed yet" : "Pas encore installé";
    return s;
  };

  return (
    <section className="workspace-panel cookbook-panel">
      <nav className="settings-tabs cookbook-panel-tabs" role="tablist" aria-label={t("cookbook.subnav")}>
        <button
          type="button"
          role="tab"
          aria-selected={subView === "models"}
          className={subView === "models" ? "active" : ""}
          onClick={() => onSubViewChange?.("models")}
        >
          {t("cookbook.tab_models")}
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={subView === "recipes"}
          className={subView === "recipes" ? "active" : ""}
          onClick={() => onSubViewChange?.("recipes")}
        >
          {t("cookbook.tab_recipes")}
        </button>
      </nav>

      {subView === "recipes" ? (
        <CookbookRecipesView fetchEndpoint={fetchEndpoint} locale={locale} handlers={handlers} />
      ) : (
        <>
      <h3 className="cookbook-panel-heading">
        {t("cookbook.matrix_title")}
        <InfoTip
          label={t("cookbook.matrix_title")}
          content={
            en
              ? "Model matrix — sorted by hardware fit. Local Ollama/Rbitnet status shown when reachable."
              : "Matrice de modèles — tri par compatibilité. Statut Ollama/Rbitnet si joignables."
          }
        />
      </h3>
      {routeMsg ? <p className="cookbook-route-msg muted">{routeMsg}</p> : null}
      {hardware ? (
        <dl className="cookbook-hardware-dl">
          <dt>{en ? "RAM (GB)" : "RAM (Go)"}</dt>
          <dd>{String(hardware.total_ram_gb ?? "?")}</dd>
          <dt>{en ? "Platform" : "Plateforme"}</dt>
          <dd>
            {String(hardware.platform ?? "?")} / {String(hardware.arch ?? "?")}
          </dd>
          <dt>GPU</dt>
          <dd>{String(hardware.gpu_hint ?? "unknown")}</dd>
        </dl>
      ) : null}
      <RuntimeStatus runtimes={localRuntimes} en={en} />

      <div className="cookbook-filters" role="search">
        <label className="cookbook-filter-field">
          {t("cookbook.filter_provider")}
          <select value={filters.provider} onChange={(e) => setFilters((f) => ({ ...f, provider: e.target.value }))}>
            <option value="all">{en ? "All" : "Tous"}</option>
            {providers.map((p) => (
              <option key={p} value={p}>
                {p}
              </option>
            ))}
          </select>
        </label>
        <label className="cookbook-filter-field">
          {t("cookbook.filter_min_fit")}
          <input
            type="number"
            min={0}
            max={100}
            step={5}
            value={filters.minFitPct}
            onChange={(e) => setFilters((f) => ({ ...f, minFitPct: Number(e.target.value) || 0 }))}
          />
        </label>
        <label className="cookbook-filter-field">
          {t("cookbook.filter_source")}
          <select value={filters.source} onChange={(e) => setFilters((f) => ({ ...f, source: e.target.value }))}>
            {SOURCE_OPTIONS.map((s) => (
              <option key={s} value={s}>
                {sourceLabel(s)}
              </option>
            ))}
          </select>
        </label>
        <label className="cookbook-filter-field">
          {t("cookbook.filter_type")}
          <select value={filters.modelType} onChange={(e) => setFilters((f) => ({ ...f, modelType: e.target.value }))}>
            {MODEL_TYPE_OPTIONS.map((s) => (
              <option key={s} value={s}>
                {typeLabel(s)}
              </option>
            ))}
          </select>
        </label>
        <label className="cookbook-filter-field">
          {t("cookbook.filter_local")}
          <select
            value={filters.localInstall}
            onChange={(e) => setFilters((f) => ({ ...f, localInstall: e.target.value }))}
          >
            {LOCAL_INSTALL_OPTIONS.map((s) => (
              <option key={s} value={s}>
                {localInstallLabel(s)}
              </option>
            ))}
          </select>
        </label>
        <label className="cookbook-filter-field cookbook-filter-search">
          {t("cookbook.filter_search")}
          <input
            type="search"
            value={filters.search}
            placeholder={en ? "Model name…" : "Nom du modèle…"}
            onChange={(e) => setFilters((f) => ({ ...f, search: e.target.value }))}
          />
        </label>
      </div>

      <p className="muted cookbook-result-count">
        {en ? `${filtered.length} / ${allItems.length} models` : `${filtered.length} / ${allItems.length} modèles`}
      </p>
      <div className="cookbook-provider-legend" aria-hidden>
        {providers.map((p) => (
          <span key={p} className="cookbook-legend-item">
            <span className="cookbook-legend-swatch" style={{ background: providerColor(p) }} />
            {p}
          </span>
        ))}
      </div>

      <div className="cookbook-scroll" role="region" aria-label={t("cookbook.matrix_title")}>
        {filtered.length === 0 ? (
          <p className="muted cookbook-empty">{en ? "No models match these filters." : "Aucun modèle ne correspond à ces filtres."}</p>
        ) : (
          <div className="cookbook-matrix-wrap">
            <table className="cookbook-matrix">
              <thead>
                <tr>
                  <th title={t("cookbook.col_fit_hint")}>{t("cookbook.col_fit")}</th>
                  <th>{t("cookbook.col_model")}</th>
                  <th title={en ? "Local install" : "Install. locale"}>{t("cookbook.col_local")}</th>
                  <th>{t("cookbook.col_quant")}</th>
                  <th title={t("cookbook.col_size_hint")}>{t("cookbook.col_size")}</th>
                  <th title={t("cookbook.col_ram_hint")}>{t("cookbook.col_ram")}</th>
                  <th title={t("cookbook.col_vision_hint")}>{t("cookbook.col_vision")}</th>
                  <th title={t("cookbook.col_audio_hint")}>{t("cookbook.col_audio")}</th>
                  <th title={t("cookbook.col_video_hint")}>{t("cookbook.col_video")}</th>
                  <th title={t("cookbook.col_agentic_hint")}>{t("cookbook.col_agentic")}</th>
                  <th>{t("cookbook.col_price_in")}</th>
                  <th>{t("cookbook.col_price_out")}</th>
                  <th>{t("cookbook.col_context")}</th>
                  <th>{t("cookbook.col_routes")}</th>
                  <th>{t("cookbook.col_actions")}</th>
                </tr>
              </thead>
              <tbody>
                {filtered.map((r) => {
                  const d = detailOf(r);
                  const fitTip = r.fit_explanation ?? d.fit_explanation ?? t("cookbook.fit_tooltip_default");
                  return (
                    <Fragment key={r.id}>
                      <tr className={`cookbook-matrix-row ${fitHeatClass(r.fit_score)}`}>
                        <td className="cookbook-matrix-fit">
                          <span className="cookbook-fit-badge cookbook-fit-pct" title={fitTip}>
                            {typeof r.fit_score === "number" ? `${Math.round(r.fit_score * 100)}%` : "—"}
                          </span>
                        </td>
                        <td className="cookbook-matrix-model">
                          <span className="cookbook-provider-name" style={{ color: providerColor(r.provider) }}>
                            {r.provider}
                          </span>
                          <span className="cookbook-model-sep"> / </span>
                          <span className="cookbook-model-name">{r.model}</span>
                          <ModelLinksInline item={r} />
                        </td>
                        <td className="cookbook-matrix-local">
                          <LocalInstallBadges item={r} en={en} />
                        </td>
                        <td className="cookbook-matrix-quant">{d.quantization ?? "—"}</td>
                        <td className="cookbook-matrix-size" title={t("cookbook.col_size_hint")}>
                          {formatModelSize(d.params_label, d.size_gb)}
                        </td>
                        <td
                          className="cookbook-matrix-ram"
                          title={
                            d.ram_gb != null
                              ? t("cookbook.col_ram_hint")
                              : en
                                ? "Cloud-hosted — no local RAM estimate"
                                : "Cloud — pas d'estimation RAM locale"
                          }
                        >
                          {formatSizeGb(d.ram_gb)}
                        </td>
                        <CapabilityCell value={d.vision} title={t("cookbook.col_vision_hint")} />
                        <CapabilityCell value={d.audio} title={t("cookbook.col_audio_hint")} />
                        <CapabilityCell value={d.video} title={t("cookbook.col_video_hint")} />
                        <CapabilityCell value={d.agentic} title={t("cookbook.col_agentic_hint")} />
                        <td className="cookbook-matrix-price">{formatPricePerMillion(d.price_input_per_million)}/M</td>
                        <td className="cookbook-matrix-price">{formatPricePerMillion(d.price_output_per_million)}/M</td>
                        <td className="cookbook-matrix-ctx">{formatContext(d.context_length)}</td>
                        <td className="cookbook-matrix-routes">
                          {r.task_types?.length ? r.task_types.join(", ") : "—"}
                        </td>
                        <td className="cookbook-matrix-actions">
                          <button
                            type="button"
                            className="cookbook-route-toggle"
                            onClick={() => setExpandedId((id) => (id === r.id ? null : r.id))}
                          >
                            {expandedId === r.id ? "✕" : "+"}
                          </button>
                        </td>
                      </tr>
                      {expandedId === r.id ? (
                        <tr className="cookbook-matrix-expand-row">
                          <td colSpan={15}>
                            <p className="muted cookbook-matrix-notes">{r.notes}</p>
                            <AddRouteForm
                              item={r}
                              taskCategories={taskCategories}
                              registeredProviders={registeredProviders}
                              localRuntimes={localRuntimes}
                              en={en}
                              busy={routeBusy}
                              onAdd={(opts) => void addToRouter(r, opts)}
                            />
                          </td>
                        </tr>
                      ) : null}
                    </Fragment>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </div>
        </>
      )}
    </section>
  );
}
