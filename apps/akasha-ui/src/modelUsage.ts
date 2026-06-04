export type ModelPriceRates = {
  inputPerMillion?: number | null;
  outputPerMillion?: number | null;
};

export type ModelUsageStats = {
  model?: string | null;
  promptTokens?: number | null;
  completionTokens?: number | null;
  costUsd?: number | null;
  latencyMs?: number | null;
  priceRates?: ModelPriceRates | null;
};

export type ModelUsageDisplay = {
  model?: string;
  promptTokens?: number;
  completionTokens?: number;
  latencyMs?: number;
  costUsd?: number;
  costInputUsd?: number;
  costOutputUsd?: number;
  costEstimated: boolean;
};

function num(v: unknown): number | undefined {
  return typeof v === "number" && Number.isFinite(v) ? v : undefined;
}

export function parseUsageFromTaskStatus(status: {
  last_turn_tokens_in?: unknown;
  last_turn_tokens_out?: unknown;
  last_turn_cost_usd?: unknown;
  last_turn_latency_ms?: unknown;
  last_turn_model_used?: unknown;
}): ModelUsageStats | null {
  const promptTokens = num(status.last_turn_tokens_in);
  const completionTokens = num(status.last_turn_tokens_out);
  const costUsd = num(status.last_turn_cost_usd);
  const latencyMs = num(status.last_turn_latency_ms);
  const model =
    typeof status.last_turn_model_used === "string" && status.last_turn_model_used.trim()
      ? status.last_turn_model_used.trim()
      : undefined;
  if (
    promptTokens == null &&
    completionTokens == null &&
    costUsd == null &&
    latencyMs == null &&
    !model
  ) {
    return null;
  }
  return { model, promptTokens, completionTokens, costUsd, latencyMs };
}

export function parseUsageFromEventPayload(payload: unknown): ModelUsageStats | null {
  if (!payload || typeof payload !== "object") return null;
  const p = payload as Record<string, unknown>;
  const promptTokens = num(p.prompt_tokens);
  const completionTokens = num(p.completion_tokens);
  const costUsd = num(p.cost_usd);
  const latencyMs = num(p.latency_ms);
  const model =
    typeof p.model_used === "string" && p.model_used.trim() ? p.model_used.trim() : undefined;
  if (
    promptTokens == null &&
    completionTokens == null &&
    costUsd == null &&
    latencyMs == null &&
    !model
  ) {
    return null;
  }
  return { model, promptTokens, completionTokens, costUsd, latencyMs };
}

export function estimateCostFromRates(
  promptTokens: number,
  completionTokens: number,
  rates: ModelPriceRates,
): { inputUsd: number; outputUsd: number; totalUsd: number } | null {
  const inRate = rates.inputPerMillion;
  const outRate = rates.outputPerMillion;
  if (inRate == null && outRate == null) return null;
  const inputUsd = inRate != null ? (promptTokens * inRate) / 1_000_000 : 0;
  const outputUsd = outRate != null ? (completionTokens * outRate) / 1_000_000 : 0;
  return { inputUsd, outputUsd, totalUsd: inputUsd + outputUsd };
}

export function buildModelUsageDisplay(stats: ModelUsageStats): ModelUsageDisplay | null {
  const promptTokens = stats.promptTokens ?? undefined;
  const completionTokens = stats.completionTokens ?? undefined;
  const latencyMs = stats.latencyMs ?? undefined;
  const model = stats.model ?? undefined;
  const hasTokens = (promptTokens ?? 0) > 0 || (completionTokens ?? 0) > 0;
  const hasLatency = (latencyMs ?? 0) > 0;
  const hasCost = stats.costUsd != null && Math.abs(stats.costUsd) > 0;
  if (!hasTokens && !hasLatency && !hasCost && !model) return null;

  const out: ModelUsageDisplay = {
    model,
    promptTokens,
    completionTokens,
    latencyMs,
    costEstimated: false,
  };

  if (hasCost && stats.costUsd != null) {
    out.costUsd = stats.costUsd;
  }

  if (
    stats.priceRates &&
    promptTokens != null &&
    completionTokens != null &&
    (stats.priceRates.inputPerMillion != null || stats.priceRates.outputPerMillion != null)
  ) {
    const est = estimateCostFromRates(promptTokens, completionTokens, stats.priceRates);
    if (est) {
      if (!hasCost) {
        out.costUsd = est.totalUsd;
        out.costEstimated = true;
      }
      out.costInputUsd = est.inputUsd;
      out.costOutputUsd = est.outputUsd;
    }
  }

  return out;
}

export function formatUsd(value: number, locale: "fr" | "en"): string {
  if (value >= 0.01) return `$${value.toFixed(4)}`;
  if (value >= 0.0001) return `$${value.toFixed(5)}`;
  return locale === "en" ? `$${value.toExponential(2)}` : `$${value.toExponential(2)}`;
}

export function formatLatency(ms: number, locale: "fr" | "en"): string {
  if (ms >= 60_000) {
    const s = (ms / 1000).toFixed(1);
    return locale === "en" ? `${s}s` : `${s} s`;
  }
  if (ms >= 1000) {
    const s = (ms / 1000).toFixed(2);
    return locale === "en" ? `${s}s` : `${s} s`;
  }
  return locale === "en" ? `${Math.round(ms)} ms` : `${Math.round(ms)} ms`;
}

export type CookbookPricingEntry = {
  provider: string;
  model: string;
  price_input_per_million?: number | null;
  price_output_per_million?: number | null;
};

export function buildCookbookPricingLookup(
  items: CookbookPricingEntry[],
): Map<string, ModelPriceRates> {
  const map = new Map<string, ModelPriceRates>();
  for (const item of items) {
    const rates: ModelPriceRates = {
      inputPerMillion: item.price_input_per_million ?? null,
      outputPerMillion: item.price_output_per_million ?? null,
    };
    if (rates.inputPerMillion == null && rates.outputPerMillion == null) continue;
    const keys = [
      `${item.provider}/${item.model}`,
      item.model,
      item.model.split("/").pop() ?? item.model,
    ];
    for (const key of keys) {
      if (key && !map.has(key)) map.set(key, rates);
    }
  }
  return map;
}

export function lookupPriceRates(
  lookup: Map<string, ModelPriceRates>,
  model?: string | null,
  provider?: string | null,
): ModelPriceRates | null {
  if (!model && !provider) return null;
  const candidates = [
    provider && model ? `${provider}/${model}` : null,
    model ?? null,
    model?.split("/").pop() ?? null,
  ].filter(Boolean) as string[];
  for (const key of candidates) {
    const hit = lookup.get(key);
    if (hit) return hit;
  }
  return null;
}
