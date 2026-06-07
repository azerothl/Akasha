export type LocalRuntimeInfo = {
  base_url?: string;
  running?: boolean;
  configured?: boolean;
  models?: string[];
};

export type LocalInstall = {
  ollama?: boolean;
  rbitnet?: boolean;
};

export type PullHints = {
  ollama?: string;
  rbitnet?: string;
};

export type ModelDetails = {
  quantization?: string | null;
  vision?: boolean;
  audio?: boolean;
  video?: boolean;
  agentic?: boolean;
  context_length?: number | null;
  price_input_per_million?: number | null;
  price_output_per_million?: number | null;
  fit_explanation?: string;
  params_billions?: number | null;
  params_label?: string | null;
  size_gb?: number | null;
  ram_gb?: number | null;
};

export type CookbookItem = {
  id: string;
  label: string;
  provider: string;
  model: string;
  fit_score?: number;
  fit_explanation?: string;
  source?: string;
  task_types?: string[];
  notes: string;
  url?: string;
  integration?: string;
  model_types?: string[];
  links?: { label: string; url: string }[];
  section?: string;
  details?: ModelDetails;
  local_install?: LocalInstall;
  pull_hints?: PullHints;
};

export type CookbookFilters = {
  provider: string;
  minFitPct: number;
  source: string;
  modelType: string;
  localInstall: string;
  search: string;
};

export const DEFAULT_COOKBOOK_FILTERS: CookbookFilters = {
  provider: "all",
  minFitPct: 0,
  source: "all",
  modelType: "all",
  localInstall: "all",
  search: "",
};

const LOCAL_INSTALL_PROVIDERS = new Set([
  "ollama",
  "bitnet",
  "huggingface",
  "llama_server",
  "local_openai",
]);

export function isLocalInstallCandidate(item: CookbookItem): boolean {
  if (LOCAL_INSTALL_PROVIDERS.has(item.provider)) return true;
  const types = item.model_types ?? [];
  return types.includes("local") || types.includes("gguf");
}

export function isLocallyInstalled(item: CookbookItem): boolean {
  const li = item.local_install;
  return Boolean(li?.ollama || li?.rbitnet);
}

export function filterCookbookItems(items: CookbookItem[], filters: CookbookFilters): CookbookItem[] {
  const q = filters.search.trim().toLowerCase();
  return items.filter((item) => {
    if (filters.provider !== "all" && item.provider !== filters.provider) return false;
    if (filters.minFitPct > 0) {
      const pct = (item.fit_score ?? 0) * 100;
      if (pct < filters.minFitPct) return false;
    }
    if (filters.source !== "all" && item.source !== filters.source) return false;
    if (filters.modelType !== "all") {
      const types = item.model_types ?? [];
      if (!types.includes(filters.modelType)) return false;
    }
    if (filters.localInstall !== "all") {
      const candidate = isLocalInstallCandidate(item);
      const installed = isLocallyInstalled(item);
      if (filters.localInstall === "local" && !candidate) return false;
      if (filters.localInstall === "installed" && !installed) return false;
      if (filters.localInstall === "missing" && (!candidate || installed)) return false;
    }
    if (q) {
      const d = item.details;
      const hay = `${item.label} ${item.provider} ${item.model} ${item.notes} ${(item.model_types ?? []).join(" ")} ${d?.quantization ?? ""} ${d?.params_label ?? ""}`.toLowerCase();
      if (!hay.includes(q)) return false;
    }
    return true;
  });
}

export function uniqueProviders(items: CookbookItem[]): string[] {
  return [...new Set(items.map((i) => i.provider))].sort();
}
