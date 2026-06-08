export type RecipeCategory =
  | "prompts"
  | "rag"
  | "agents"
  | "fine_tuning"
  | "evaluation"
  | "ml"
  | "deployment";

export type RecipeSummary = {
  id: string;
  category: RecipeCategory;
  tags: string[];
  difficulty: "beginner" | "intermediate" | "advanced";
  last_reviewed: string;
  title: string;
  summary: string;
};

export type RecipeLocale = {
  title: string;
  summary: string;
  steps_md: string;
};

export type RecipeVariable = {
  key: string;
  label_en: string;
  label_fr: string;
  default?: string;
  placeholder_en?: string;
  placeholder_fr?: string;
};

export type RecipeAction = {
  type:
    | "try_in_chat"
    | "open_compare"
    | "pull_model"
    | "add_route"
    | "install_skill"
    | "open_external"
    | "open_models_tab";
  label_en: string;
  label_fr: string;
  payload?: Record<string, unknown>;
};

export type CookbookRecipe = {
  id: string;
  category: RecipeCategory;
  tags: string[];
  difficulty: "beginner" | "intermediate" | "advanced";
  last_reviewed: string;
  locales: Record<string, RecipeLocale>;
  prerequisites?: {
    task_types?: string[];
    models_hint?: string[];
    skills?: string[];
    min_ram_gb?: number;
    gpu_hint?: string;
  };
  prompt_template?: string;
  variables?: RecipeVariable[];
  config_snippet?: { language: string; content: string };
  commands?: Array<{ label_en: string; label_fr: string; command: string }>;
  actions?: RecipeAction[];
  related_recipe_ids?: string[];
  external_links?: Array<{ label_en: string; label_fr: string; url: string }>;
};

export type RecipeRecommendedModel = {
  id?: string;
  provider: string;
  model: string;
  fit_score?: number;
  recipe_fit_score?: number;
  label?: string;
  notes?: string;
};

export const RECIPE_CATEGORIES: RecipeCategory[] = [
  "prompts",
  "rag",
  "agents",
  "fine_tuning",
  "evaluation",
  "ml",
  "deployment",
];

export function recipeLocale(recipe: CookbookRecipe, locale: "fr" | "en"): RecipeLocale {
  return recipe.locales[locale] ?? recipe.locales.en;
}

export function actionLabel(action: RecipeAction, locale: "fr" | "en"): string {
  return locale === "fr" ? action.label_fr : action.label_en;
}

export function categoryLabel(category: RecipeCategory, locale: "fr" | "en"): string {
  const en: Record<RecipeCategory, string> = {
    prompts: "Prompts",
    rag: "RAG",
    agents: "Agents",
    fine_tuning: "Fine-tuning",
    evaluation: "Evaluation",
    ml: "ML & data",
    deployment: "Deployment",
  };
  const fr: Record<RecipeCategory, string> = {
    prompts: "Prompts",
    rag: "RAG",
    agents: "Agents",
    fine_tuning: "Fine-tuning",
    evaluation: "Évaluation",
    ml: "ML & données",
    deployment: "Déploiement",
  };
  return locale === "fr" ? fr[category] : en[category];
}

export function difficultyLabel(d: CookbookRecipe["difficulty"], locale: "fr" | "en"): string {
  const map: Record<CookbookRecipe["difficulty"], [string, string]> = {
    beginner: ["Beginner", "Débutant"],
    intermediate: ["Intermediate", "Intermédiaire"],
    advanced: ["Advanced", "Avancé"],
  };
  return locale === "fr" ? map[d][1] : map[d][0];
}

export function defaultVariableValues(recipe: CookbookRecipe): Record<string, string> {
  const out: Record<string, string> = {};
  for (const v of recipe.variables ?? []) {
    out[v.key] = v.default ?? "";
  }
  return out;
}

export function substitutePromptTemplate(template: string, values: Record<string, string>): string {
  return template.replace(/\{(\w+)\}/g, (_, key: string) => values[key] ?? `{${key}}`);
}

export function buildRecipePrompt(recipe: CookbookRecipe, values: Record<string, string>): string | null {
  if (!recipe.prompt_template) return null;
  return substitutePromptTemplate(recipe.prompt_template, values);
}

export function filterRecipeSummaries(
  items: RecipeSummary[],
  category: string,
  search: string,
): RecipeSummary[] {
  const q = search.trim().toLowerCase();
  return items.filter((r) => {
    if (category !== "all" && r.category !== category) return false;
    if (!q) return true;
    const hay = `${r.title} ${r.summary} ${r.tags.join(" ")} ${r.id}`.toLowerCase();
    return hay.includes(q);
  });
}

export type ComparePrefill = {
  prompt: string;
  blind?: boolean;
  models?: Array<{ provider: string; model: string }>;
};

export type CookbookRecipeActionContext = {
  recipe: CookbookRecipe;
  variableValues: Record<string, string>;
  prompt: string | null;
};
