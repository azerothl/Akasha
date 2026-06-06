import type { CookbookItem } from "./cookbookFilters";

export const PROVIDER_COLORS: Record<string, string> = {
  openrouter: "#818cf8",
  ollama: "#64748b",
  openai: "#34d399",
  huggingface: "#fb923c",
  bitnet: "#a78bfa",
  akasha_embedded: "#38bdf8",
  akasha_core: "#22d3ee",
  azure_openai: "#0078d4",
  llama_server: "#94a3b8",
  local_openai: "#94a3b8",
};

export function providerColor(provider: string): string {
  return PROVIDER_COLORS[provider] ?? "#cbd5e1";
}

export function fitHeatClass(score?: number): string {
  const pct = (score ?? 0) * 100;
  if (pct >= 85) return "cookbook-fit-excellent";
  if (pct >= 70) return "cookbook-fit-good";
  if (pct >= 55) return "cookbook-fit-fair";
  return "cookbook-fit-low";
}

export function sortByFitDesc(items: CookbookItem[]): CookbookItem[] {
  return [...items].sort((a, b) => (b.fit_score ?? 0) - (a.fit_score ?? 0));
}

export function formatPricePerMillion(v?: number | null): string {
  if (v == null || Number.isNaN(v)) return "—";
  if (v === 0) return "$0";
  if (v < 0.01) return `$${v.toFixed(4)}`;
  if (v < 1) return `$${v.toFixed(3)}`;
  return `$${v.toFixed(2)}`;
}

export function formatContext(n?: number | null): string {
  if (n == null) return "—";
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1000) return `${Math.round(n / 1000)}k`;
  return String(n);
}

export function formatSizeGb(gb?: number | null): string {
  if (gb == null || Number.isNaN(gb)) return "—";
  if (gb < 1) return `${Math.round(gb * 1024)} MB`;
  return `${gb.toFixed(1)} GB`;
}

export function formatModelSize(paramsLabel?: string | null, sizeGb?: number | null): string {
  const parts: string[] = [];
  if (paramsLabel) parts.push(paramsLabel);
  if (sizeGb != null && !Number.isNaN(sizeGb)) parts.push(formatSizeGb(sizeGb));
  return parts.length ? parts.join(" · ") : "—";
}

export function boolGlyph(v?: boolean): string {
  return v ? "✓" : "·";
}

export function detailOf(item: CookbookItem) {
  return item.details ?? {};
}
