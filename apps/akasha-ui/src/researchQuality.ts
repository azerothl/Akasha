import type { ResearchReportMeta } from "./researchReportExport";

/** Includes old history entries produced before no-evidence runs became errors. */
export function researchQuality(markdown: string | undefined, meta?: ResearchReportMeta): "empty" | "limited" | "ready" {
  const text = markdown?.trim() ?? "";
  if (!text || /^No information could be gathered(?:\.| for this question\.)/.test(text)) return "empty";
  if (meta && !meta.search_degraded && (!Array.isArray(meta.sources) || meta.sources.length === 0)) return "empty";
  if (meta?.search_degraded || meta?.sources_sufficient === false || meta?.quality_warnings?.length) return "limited";
  return "ready";
}
