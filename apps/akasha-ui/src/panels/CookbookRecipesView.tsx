import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import MarkdownContent from "../MarkdownContent";
import { InfoTip } from "../components/Tooltip";
import { useNotifyOnMessage } from "../notifications/useNotifyOnMessage";
import { useI18n } from "../useI18n";
import {
  actionLabel,
  buildRecipePrompt,
  categoryLabel,
  defaultVariableValues,
  difficultyLabel,
  filterRecipeSummaries,
  recipeLocale,
  RECIPE_CATEGORIES,
  type ComparePrefill,
  type CookbookRecipe,
  type CookbookRecipeActionContext,
  type RecipeRecommendedModel,
  type RecipeSummary,
} from "../cookbookRecipes";
import { fitHeatClass, providerColor } from "../cookbookMatrix";

type FetchEndpoint = (
  path: string,
  init?: RequestInit,
) => Promise<{ ok: boolean; status: number; text: string }>;

export type CookbookRecipeHandlers = {
  onTryInChat: (ctx: CookbookRecipeActionContext) => void;
  onOpenCompare: (prefill: ComparePrefill) => void;
  onOpenModelsTab: () => void;
  onPullModel: (runtime: "ollama" | "rbitnet", model: string) => Promise<string>;
  onAddRoute: (opts: {
    category: string;
    provider: string;
    model: string;
    role: "primary" | "fallback";
  }) => Promise<string>;
  onInstallSkill: (url: string) => Promise<string>;
};

type Props = {
  fetchEndpoint: FetchEndpoint;
  locale: "fr" | "en";
  handlers: CookbookRecipeHandlers;
};

function CopyButton({ text, en }: { text: string; en: boolean }) {
  const [copied, setCopied] = useState(false);
  return (
    <button
      type="button"
      className="cookbook-recipe-copy-btn"
      onClick={() => {
        void navigator.clipboard.writeText(text).then(() => {
          setCopied(true);
          window.setTimeout(() => setCopied(false), 1500);
        });
      }}
    >
      {copied ? (en ? "Copied" : "Copié") : en ? "Copy" : "Copier"}
    </button>
  );
}

export function CookbookRecipesView({ fetchEndpoint, locale, handlers }: Props) {
  const { t } = useI18n();
  const en = locale === "en";
  const [summaries, setSummaries] = useState<RecipeSummary[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [recipe, setRecipe] = useState<CookbookRecipe | null>(null);
  const [recommended, setRecommended] = useState<RecipeRecommendedModel[]>([]);
  const [variableValues, setVariableValues] = useState<Record<string, string>>({});
  const [categoryFilter, setCategoryFilter] = useState("all");
  const [search, setSearch] = useState("");
  const [err, setErr] = useState<string | null>(null);
  const [actionMsg, setActionMsg] = useState<string | null>(null);
  const [actionBusy, setActionBusy] = useState(false);
  const [detailLoading, setDetailLoading] = useState(false);
  const [contextLoading, setContextLoading] = useState(false);
  const recipeCacheRef = useRef(new Map<string, CookbookRecipe>());
  const loadSeqRef = useRef(0);

  useNotifyOnMessage(err, "error", t("tabs.cookbook"));

  const loadList = useCallback(async () => {
    const res = await fetchEndpoint(`/api/cookbook/recipes?locale=${locale}`);
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const j = JSON.parse(res.text) as { recipes?: RecipeSummary[] };
    setSummaries(j.recipes ?? []);
  }, [fetchEndpoint, locale]);

  useEffect(() => {
    void (async () => {
      try {
        await loadList();
      } catch (e) {
        setErr(e instanceof Error ? e.message : String(e));
      }
    })();
  }, [loadList]);

  const loadDetail = useCallback(
    async (id: string) => {
      const seq = ++loadSeqRef.current;
      setSelectedId(id);
      setActionMsg(null);
      setRecommended([]);
      setContextLoading(true);

      const cached = recipeCacheRef.current.get(id);
      if (cached) {
        setRecipe(cached);
        setVariableValues(defaultVariableValues(cached));
        setDetailLoading(false);
      } else {
        setDetailLoading(true);
        setRecipe(null);
      }

      try {
        const detailRes = await fetchEndpoint(`/api/cookbook/recipes/${encodeURIComponent(id)}?locale=${locale}`);
        if (seq !== loadSeqRef.current) return;
        if (!detailRes.ok) throw new Error(`HTTP ${detailRes.status}`);
        const detail = JSON.parse(detailRes.text) as { recipe?: CookbookRecipe };
        const full = detail.recipe ?? null;
        if (full) {
          recipeCacheRef.current.set(id, full);
          setRecipe(full);
          setVariableValues(defaultVariableValues(full));
        }
      } catch (e) {
        if (seq === loadSeqRef.current) {
          setErr(e instanceof Error ? e.message : String(e));
        }
      } finally {
        if (seq === loadSeqRef.current) {
          setDetailLoading(false);
        }
      }

      try {
        const ctxRes = await fetchEndpoint(
          `/api/cookbook/recipes/${encodeURIComponent(id)}/context?locale=${locale}`,
        );
        if (seq !== loadSeqRef.current) return;
        if (ctxRes.ok) {
          const ctx = JSON.parse(ctxRes.text) as { recommended_models?: RecipeRecommendedModel[] };
          setRecommended(ctx.recommended_models ?? []);
        }
      } catch {
        /* suggested models are optional */
      } finally {
        if (seq === loadSeqRef.current) {
          setContextLoading(false);
        }
      }
    },
    [fetchEndpoint, locale],
  );

  const prefetchDetail = useCallback(
    (id: string) => {
      if (recipeCacheRef.current.has(id)) return;
      void fetchEndpoint(`/api/cookbook/recipes/${encodeURIComponent(id)}?locale=${locale}`)
        .then((res) => {
          if (!res.ok) return;
          const detail = JSON.parse(res.text) as { recipe?: CookbookRecipe };
          if (detail.recipe) recipeCacheRef.current.set(id, detail.recipe);
        })
        .catch(() => {
          /* ignore prefetch errors */
        });
    },
    [fetchEndpoint, locale],
  );

  const filtered = useMemo(
    () => filterRecipeSummaries(summaries, categoryFilter, search),
    [summaries, categoryFilter, search],
  );

  const loc = recipe ? recipeLocale(recipe, locale) : null;
  const builtPrompt = recipe ? buildRecipePrompt(recipe, variableValues) : null;

  const actionContext: CookbookRecipeActionContext | null = recipe
    ? { recipe, variableValues, prompt: builtPrompt }
    : null;

  const runAction = async (actionType: string, payload: Record<string, unknown> = {}) => {
    if (!actionContext) return;
    setActionBusy(true);
    setActionMsg(null);
    try {
      if (actionType === "try_in_chat") {
        const override = payload.override_prompt as string | undefined;
        let prompt = override
          ? override.replace(/\{(\w+)\}/g, (_, k: string) => variableValues[k] ?? `{${k}}`)
          : builtPrompt;
        if (!prompt) {
          prompt = en
            ? `Follow cookbook recipe "${loc?.title}" (${recipe!.id}).`
            : `Suivre la recette cookbook « ${loc?.title} » (${recipe!.id}).`;
        }
        handlers.onTryInChat({ ...actionContext, prompt });
        setActionMsg(en ? "Opened in Chat." : "Ouvert dans le Chat.");
        return;
      }
      if (actionType === "open_compare") {
        handlers.onOpenCompare({
          prompt: builtPrompt ?? "",
          blind: payload.blind !== false,
        });
        setActionMsg(en ? "Opened Compare." : "Comparer ouvert.");
        return;
      }
      if (actionType === "open_models_tab") {
        handlers.onOpenModelsTab();
        return;
      }
      if (actionType === "open_external") {
        const url = payload.url as string | undefined;
        if (url) window.open(url, "_blank", "noopener,noreferrer");
        return;
      }
      if (actionType === "pull_model") {
        const runtime = (payload.runtime as string) === "rbitnet" ? "rbitnet" : "ollama";
        const model = String(payload.model ?? "").trim();
        if (!model) throw new Error(en ? "Missing model" : "Modèle manquant");
        const msg = await handlers.onPullModel(runtime, model);
        setActionMsg(msg);
        return;
      }
      if (actionType === "add_route") {
        const category = String(payload.category ?? "conversation");
        const provider = String(payload.provider ?? "");
        const model = String(payload.model ?? "");
        const role = payload.role === "fallback" ? "fallback" : "primary";
        if (!provider || !model) throw new Error(en ? "Missing route" : "Route incomplète");
        const msg = await handlers.onAddRoute({ category, provider, model, role });
        setActionMsg(msg);
        return;
      }
      if (actionType === "install_skill") {
        const url = String(payload.url ?? "").trim();
        if (!url) throw new Error(en ? "Missing skill URL" : "URL skill manquante");
        const msg = await handlers.onInstallSkill(url);
        setActionMsg(msg);
        return;
      }
    } catch (e) {
      setActionMsg(e instanceof Error ? e.message : String(e));
    } finally {
      setActionBusy(false);
    }
  };

  return (
    <div className="cookbook-recipes-view">
      <div className="cookbook-recipes-layout">
        <aside className="cookbook-recipes-list-pane" aria-label={t("cookbook.recipes_list")}>
          <div className="cookbook-recipes-filters">
            <label className="cookbook-filter-field">
              {t("cookbook.recipes_category")}
              <select value={categoryFilter} onChange={(e) => setCategoryFilter(e.target.value)}>
                <option value="all">{en ? "All" : "Toutes"}</option>
                {RECIPE_CATEGORIES.map((c) => (
                  <option key={c} value={c}>
                    {categoryLabel(c, locale)}
                  </option>
                ))}
              </select>
            </label>
            <label className="cookbook-filter-field cookbook-filter-search">
              {t("cookbook.filter_search")}
              <input type="search" value={search} onChange={(e) => setSearch(e.target.value)} placeholder={en ? "Recipe…" : "Recette…"} />
            </label>
          </div>
          <p className="muted cookbook-result-count">
            {en ? `${filtered.length} recipes` : `${filtered.length} recettes`}
          </p>
          <ul className="cookbook-recipes-list">
            {filtered.map((r) => (
              <li key={r.id}>
                <button
                  type="button"
                  className={`cookbook-recipe-card ${selectedId === r.id ? "cookbook-recipe-card-active" : ""}`}
                  onClick={() => void loadDetail(r.id)}
                  onMouseEnter={() => prefetchDetail(r.id)}
                  onFocus={() => prefetchDetail(r.id)}
                >
                  <span className="cookbook-recipe-card-cat">{categoryLabel(r.category, locale)}</span>
                  <span className="cookbook-recipe-card-title">{r.title}</span>
                  <span className="cookbook-recipe-card-summary muted">{r.summary}</span>
                  <span className="cookbook-recipe-card-meta">{difficultyLabel(r.difficulty, locale)}</span>
                </button>
              </li>
            ))}
          </ul>
        </aside>

        <div className="cookbook-recipes-detail-pane">
          {detailLoading && !recipe ? (
            <p className="muted cookbook-recipes-empty">
              {en ? "Loading recipe…" : "Chargement de la recette…"}
            </p>
          ) : !recipe || !loc ? (
            <p className="muted cookbook-recipes-empty">
              {en ? "Select a recipe to view steps and actions." : "Sélectionnez une recette pour voir les étapes et actions."}
            </p>
          ) : (
            <>
              <header className="cookbook-recipe-detail-header">
                <h4>{loc.title}</h4>
                <p className="muted">{loc.summary}</p>
                <div className="cookbook-recipe-badges">
                  <span className="cookbook-recipe-badge">{categoryLabel(recipe.category, locale)}</span>
                  <span className="cookbook-recipe-badge">{difficultyLabel(recipe.difficulty, locale)}</span>
                  {recipe.prerequisites?.min_ram_gb ? (
                    <span className="cookbook-recipe-badge">RAM ≥ {recipe.prerequisites.min_ram_gb} GB</span>
                  ) : null}
                </div>
              </header>

              {actionMsg ? <p className="cookbook-route-msg muted">{actionMsg}</p> : null}

              {contextLoading ? (
                <p className="muted cookbook-recipe-models-loading">
                  {en ? "Loading suggested models…" : "Chargement des modèles suggérés…"}
                </p>
              ) : recommended.length > 0 ? (
                <section className="cookbook-recipe-models-banner">
                  <h5>
                    {t("cookbook.recipes_suggested_models")}
                    <InfoTip
                      label={t("cookbook.recipes_suggested_models")}
                      content={
                        en
                          ? "Models ranked for this recipe based on hardware fit and prerequisites."
                          : "Modèles classés pour cette recette selon le matériel et les prérequis."
                      }
                    />
                  </h5>
                  <ul className="cookbook-recipe-model-chips">
                    {recommended.slice(0, 6).map((m) => {
                      const score = m.recipe_fit_score ?? m.fit_score;
                      return (
                        <li key={`${m.provider}/${m.model}`} className={fitHeatClass(score)}>
                          <span style={{ color: providerColor(m.provider) }}>{m.provider}</span>
                          <span> / {m.model}</span>
                          {typeof score === "number" ? (
                            <span className="cookbook-recipe-model-fit">{Math.round(score * 100)}%</span>
                          ) : null}
                        </li>
                      );
                    })}
                  </ul>
                </section>
              ) : null}

              {(recipe.variables?.length ?? 0) > 0 ? (
                <section className="cookbook-recipe-variables">
                  <h5>{t("cookbook.recipes_variables")}</h5>
                  {recipe.variables!.map((v) => (
                    <label key={v.key} className="cookbook-recipe-var-field">
                      {locale === "fr" ? v.label_fr : v.label_en}
                      <input
                        type="text"
                        value={variableValues[v.key] ?? ""}
                        placeholder={locale === "fr" ? v.placeholder_fr : v.placeholder_en}
                        onChange={(e) => setVariableValues((prev) => ({ ...prev, [v.key]: e.target.value }))}
                      />
                    </label>
                  ))}
                </section>
              ) : null}

              {builtPrompt ? (
                <section className="cookbook-recipe-prompt-block">
                  <div className="cookbook-recipe-block-head">
                    <h5>{t("cookbook.recipes_prompt")}</h5>
                    <CopyButton text={builtPrompt} en={en} />
                  </div>
                  <pre className="cookbook-recipe-pre">{builtPrompt}</pre>
                </section>
              ) : null}

              {recipe.config_snippet ? (
                <section className="cookbook-recipe-prompt-block">
                  <div className="cookbook-recipe-block-head">
                    <h5>{t("cookbook.recipes_config")}</h5>
                    <CopyButton text={recipe.config_snippet.content} en={en} />
                  </div>
                  <pre className="cookbook-recipe-pre">{recipe.config_snippet.content}</pre>
                </section>
              ) : null}

              {(recipe.commands?.length ?? 0) > 0 ? (
                <section className="cookbook-recipe-commands">
                  <h5>{t("cookbook.recipes_commands")}</h5>
                  {recipe.commands!.map((cmd) => (
                    <div key={cmd.command} className="cookbook-recipe-cmd-row">
                      <span className="muted">{en ? cmd.label_en : cmd.label_fr}</span>
                      <pre className="cookbook-recipe-pre">{cmd.command}</pre>
                      <CopyButton text={cmd.command} en={en} />
                    </div>
                  ))}
                </section>
              ) : null}

              <section className="cookbook-recipe-steps">
                <h5>{t("cookbook.recipes_steps")}</h5>
                <MarkdownContent>{loc.steps_md}</MarkdownContent>
              </section>

              {(recipe.actions?.length ?? 0) > 0 ? (
                <div className="cookbook-recipe-actions">
                  {recipe.actions!.map((action) => (
                    <button
                      key={`${action.type}-${action.label_en}`}
                      type="button"
                      className="cookbook-recipe-action-btn"
                      disabled={actionBusy}
                      onClick={() => void runAction(action.type, action.payload ?? {})}
                    >
                      {actionLabel(action, locale)}
                    </button>
                  ))}
                </div>
              ) : null}

              {(recipe.external_links?.length ?? 0) > 0 ? (
                <p className="cookbook-recipe-external">
                  {recipe.external_links!.map((link) => (
                    <a key={link.url} href={link.url} target="_blank" rel="noreferrer" className="cookbook-info-link">
                      {en ? link.label_en : link.label_fr}
                    </a>
                  ))}
                </p>
              ) : null}
            </>
          )}
        </div>
      </div>
    </div>
  );
}
