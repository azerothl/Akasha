/** Normalize Deep Research markdown before HTML export (citations, duplicate headings, links). */

export type ResearchSourceRef = { title: string; url: string };

const EXEC_HEADING_RE = /^#{1,3}\s*(executive\s+summary|résumé\s+exécutif)\s*$/i;

function normalizeUrl(raw: string): string | null {
  const u = raw.trim();
  if (!u || u === "#" || u.startsWith("#")) return null;
  if (/^https?:\/\//i.test(u)) return u;
  if (/^[a-z][a-z0-9+.-]*:/i.test(u) && !/^https?:/i.test(u)) return null;
  if (u.startsWith("//")) return `https:${u}`;
  if (/^[\w.-]+\.[a-z]{2,}(\/|$)/i.test(u)) return `https://${u.replace(/^\/+/, "")}`;
  return null;
}

/** Remove duplicate executive-summary sections (EN + FR) and keep one locale-appropriate heading. */
export function dedupeExecutiveSummarySections(markdown: string, locale: "fr" | "en"): string {
  const lines = markdown.split("\n");
  const out: string[] = [];
  let sawExec = false;
  let skippingDup = false;

  for (const line of lines) {
    const trimmed = line.trim();
    if (EXEC_HEADING_RE.test(trimmed)) {
      if (sawExec) {
        skippingDup = true;
        continue;
      }
      sawExec = true;
      skippingDup = false;
      out.push(locale === "en" ? "## Executive summary" : "## Résumé exécutif");
      continue;
    }
    if (skippingDup) {
      if (/^#{1,3}\s+\S/.test(trimmed)) skippingDup = false;
      else continue;
    }
    out.push(line);
  }
  return out.join("\n");
}

/** Turn [1], [2]… into markdown links when sources are known. */
export function linkNumericCitations(markdown: string, sources: ResearchSourceRef[]): string {
  if (!sources.length) return markdown;
  let out = markdown;
  for (let i = 0; i < sources.length; i++) {
    const n = i + 1;
    const url = normalizeUrl(sources[i].url);
    if (!url) continue;
    const label = (sources[i].title || `Source ${n}`).replace(/[\[\]]/g, "");
    const re = new RegExp(`\\[${n}\\](?!\\()`, "g");
    out = out.replace(re, `[${label}](${url})`);
  }
  return out;
}

/** Fix markdown links missing a scheme or pointing at # anchors. */
export function fixMarkdownLinks(markdown: string, sources: ResearchSourceRef[]): string {
  const urlByHost = new Map<string, string>();
  for (const s of sources) {
    const url = normalizeUrl(s.url);
    if (!url) continue;
    try {
      urlByHost.set(new URL(url).hostname.replace(/^www\./, ""), url);
    } catch {
      /* ignore */
    }
  }

  return markdown.replace(/\[([^\]]+)\]\(([^)]+)\)/g, (_m, text: string, href: string) => {
    const fixed =
      normalizeUrl(href) ??
      (() => {
        const host = href.replace(/^www\./, "").split("/")[0];
        return urlByHost.get(host) ?? null;
      })();
    if (!fixed) return `[${text}](${href})`;
    return `[${text}](${fixed})`;
  });
}

export function preprocessResearchReportMarkdown(
  markdown: string,
  sources: ResearchSourceRef[],
  locale: "fr" | "en",
): string {
  let md = markdown.trim();
  md = dedupeExecutiveSummarySections(md, locale);
  md = linkNumericCitations(md, sources);
  md = fixMarkdownLinks(md, sources);
  return md;
}
