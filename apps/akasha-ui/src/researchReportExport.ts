import {
  buildReportThemeCss,
  type ResearchReportTheme,
} from "./researchReportThemes";

export type SkippedSourceMeta = {
  url: string;
  title: string;
  reason: string;
};

export type ResearchReportMeta = {
  report_title?: string;
  category: string;
  word_count: number;
  sources: { title: string; url: string; og_image?: string }[];
  providers_used: string[];
  rounds: number;
  duration_secs: number;
  search_degraded: boolean;
  user_question?: string;
  search_queries?: string[];
  sub_questions?: string[];
  pages_fetched?: number;
  search_hits_seen?: number;
  images_retrieved?: number;
  sources_skipped?: SkippedSourceMeta[];
  unique_domains?: number;
  sources_sufficient?: boolean;
  quality_warnings?: string[];
};

export type ResearchReportDocument = {
  /** Original user topic (methodology); cover uses report_title when set. */
  topic: string;
  dateLabel: string;
  reportMarkdown: string;
  reportMeta?: ResearchReportMeta;
  locale?: "fr" | "en";
  displayTheme?: ResearchReportTheme;
};

/** Cover / window title — reformulated headline when available. */
export function reportCoverTitle(doc: ResearchReportDocument): string {
  const t = doc.reportMeta?.report_title?.trim();
  return t && t.length > 0 ? t : doc.topic;
}

export const RESEARCH_PREVIEW_STORAGE_PREFIX = "akasha_research_preview_";

const E2E_WEB = import.meta.env.VITE_E2E === "true" || import.meta.env.MODE === "e2e";

export function parseResearchPreviewHash(hash: string): { key: string | null; autoPrint: boolean } {
  const raw = hash.replace(/^#\/?/, "");
  const [path, query = ""] = raw.split("?");
  if (path !== "research-preview") return { key: null, autoPrint: false };
  const params = new URLSearchParams(query);
  const key = params.get("key")?.trim() || null;
  const autoPrint = params.get("print") === "1";
  return { key, autoPrint };
}

export function storeResearchPreviewHtml(html: string): string {
  const key = `${Date.now()}_${Math.random().toString(36).slice(2, 9)}`;
  localStorage.setItem(RESEARCH_PREVIEW_STORAGE_PREFIX + key, html);
  return key;
}

export function consumeResearchPreviewHtml(key: string): string | null {
  const storageKey = RESEARCH_PREVIEW_STORAGE_PREFIX + key;
  const html = localStorage.getItem(storageKey);
  if (html) localStorage.removeItem(storageKey);
  return html;
}

export function readResearchPreviewHtml(key: string): string | null {
  return localStorage.getItem(RESEARCH_PREVIEW_STORAGE_PREFIX + key);
}

export function clearResearchPreviewHtml(key: string): void {
  localStorage.removeItem(RESEARCH_PREVIEW_STORAGE_PREFIX + key);
}

function buildResearchPreviewUrl(key: string, autoPrint = false): string {
  const params = new URLSearchParams({ key });
  if (autoPrint) params.set("print", "1");
  const base = `${window.location.origin}${window.location.pathname}`;
  return `${base}#/research-preview?${params.toString()}`;
}

async function isTauriDesktop(): Promise<boolean> {
  if (E2E_WEB) return false;
  try {
    const { isTauri } = await import("@tauri-apps/api/core");
    return isTauri();
  } catch {
    return false;
  }
}

async function openHtmlInTauriWindow(html: string, title: string, autoPrint = false): Promise<boolean> {
  try {
    const { WebviewWindow } = await import("@tauri-apps/api/webviewWindow");
    const key = storeResearchPreviewHtml(html);
    const label = `research-${Date.now()}`;
    const previewUrl = buildResearchPreviewUrl(key, autoPrint);
    const win = new WebviewWindow(label, {
      url: previewUrl,
      title: title.slice(0, 120) || "Research report",
      width: 960,
      height: 720,
      center: true,
      resizable: true,
    });
    return await new Promise<boolean>((resolve) => {
      let settled = false;
      const finish = (ok: boolean) => {
        if (settled) return;
        settled = true;
        resolve(ok);
      };
      win.once("tauri://created", () => finish(true));
      win.once("tauri://error", () => finish(false));
      window.setTimeout(() => finish(false), 8000);
    });
  } catch {
    return false;
  }
}

const STANDALONE_MARKDOWN_CSS_BASE = `
* { box-sizing: border-box; }
body {
  font-family: "Segoe UI", system-ui, -apple-system, Roboto, "Helvetica Neue", sans-serif;
  font-size: 1.05rem;
  line-height: 1.72;
  color: var(--rr-text);
  margin: 0;
  padding: 2.5rem 1.5rem 4rem;
}
.research-report-page {
  max-width: 46rem;
  margin: 0 auto;
}
.research-report-print-bar {
  max-width: 46rem;
  margin: 0 auto 1.5rem;
  padding: 0.85rem 1.1rem;
  background: var(--rr-surface);
  border: 1px solid var(--rr-border);
  border-radius: 10px;
  font-size: 0.875rem;
  color: var(--rr-muted);
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 0.75rem;
}
.research-report-print-bar button {
  font: inherit;
  padding: 0.4rem 0.9rem;
  border-radius: 8px;
  border: 1px solid var(--rr-accent);
  background: var(--rr-accent-2);
  color: #fff;
  cursor: pointer;
}
.research-report-cover {
  margin-bottom: 2.75rem;
  padding: 2.25rem 2rem 2rem;
  border-radius: 16px;
  border: 1px solid var(--rr-border);
  box-shadow: 0 12px 40px rgba(0,0,0,0.2);
}
.research-report-cover h1 {
  margin: 0 0 0.65rem;
  font-size: clamp(1.5rem, 4vw, 2rem);
  font-weight: 700;
  line-height: 1.28;
  letter-spacing: -0.02em;
}
.research-report-cover .meta {
  margin: 0;
  font-size: 0.95rem;
  color: var(--rr-muted);
}
.research-report-stats-grid {
  display: flex;
  flex-wrap: wrap;
  gap: 0.5rem 0.75rem;
  margin: 1.25rem 0 0;
  font-size: 0.8rem;
}
.research-report-stats-grid span {
  padding: 0.3rem 0.75rem;
  border-radius: 999px;
  border: 1px solid var(--rr-border);
}
.research-report-methodology {
  margin: 0 0 2rem;
  padding: 1.15rem 1.35rem;
  background: var(--rr-surface);
  border-radius: 12px;
  border: 1px solid var(--rr-border);
  font-size: 0.9rem;
  color: var(--rr-text);
  line-height: 1.55;
}
.research-report-methodology h2 {
  margin: 0 0 0.85rem;
  font-size: 1.05rem;
  color: var(--rr-h2);
}
.research-report-methodology h3 {
  margin: 1rem 0 0.4rem;
  font-size: 0.92rem;
  color: var(--rr-h3);
}
.research-report-methodology ul {
  margin: 0.35rem 0 0.75rem;
  padding-left: 1.35rem;
}
.research-report-methodology li { margin: 0.25rem 0; }
.research-report-methodology .user-question {
  margin: 0.5rem 0 0;
  padding: 0.65rem 0.85rem;
  background: var(--rr-surface-2);
  border-radius: 8px;
  border-left: 3px solid var(--rr-accent);
  font-style: italic;
  color: var(--rr-muted);
}
.research-report-methodology .method-stats {
  display: flex;
  flex-wrap: wrap;
  gap: 0.5rem 1rem;
  margin: 0.75rem 0;
  font-size: 0.85rem;
  color: var(--rr-muted);
}
.research-report-methodology .skipped-list {
  font-size: 0.82rem;
  color: var(--rr-muted);
  max-height: 12rem;
  overflow-y: auto;
}
.research-report-methodology .skipped-list li { margin: 0.2rem 0; }
.research-report-illustrations {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(7.5rem, 1fr));
  gap: 0.65rem;
  margin-top: 1.5rem;
}
.research-report-illustrations img {
  width: 100%;
  aspect-ratio: 16/10;
  object-fit: cover;
  border-radius: 10px;
  border: 1px solid var(--rr-border);
  background: var(--rr-surface-2);
}
.research-report-body {
  background: var(--rr-surface);
  border: 1px solid var(--rr-border);
  border-radius: 14px;
  padding: 2rem 1.75rem 2.5rem;
  box-shadow: 0 8px 32px rgba(0,0,0,0.25);
}
.markdown-rendered { white-space: normal; }
.markdown-rendered > h1:first-child,
.markdown-rendered > h2:first-child { margin-top: 0; }
.markdown-rendered h1 {
  margin: 2rem 0 1rem;
  font-size: 1.65rem;
  font-weight: 700;
  letter-spacing: -0.02em;
}
.markdown-rendered h2 {
  margin: 2.25rem 0 0.85rem;
  padding-bottom: 0.35rem;
  font-size: 1.28rem;
  font-weight: 650;
  border-bottom: 1px solid var(--rr-border);
}
.markdown-rendered h3 {
  margin: 1.5rem 0 0.65rem;
  font-size: 1.08rem;
  font-weight: 600;
}
.markdown-rendered p {
  margin: 0 0 1.15rem;
  color: var(--rr-text);
}
.markdown-rendered p:last-child { margin-bottom: 0; }
.markdown-rendered ul, .markdown-rendered ol {
  margin: 0 0 1.25rem;
  padding-left: 1.65rem;
}
.markdown-rendered li {
  margin: 0.45rem 0;
}
.markdown-rendered li::marker { color: var(--rr-accent); }
.markdown-rendered pre {
  margin: 1.25rem 0;
  padding: 1rem 1.1rem;
  background: #0c0e14;
  border-radius: 10px;
  border: 1px solid var(--rr-border);
  overflow-x: auto;
  font-size: 0.88rem;
}
.markdown-rendered code {
  background: rgba(124, 58, 237, 0.15);
  padding: 0.12rem 0.4rem;
  border-radius: 4px;
  font-size: 0.88em;
  font-family: ui-monospace, "Cascadia Code", "JetBrains Mono", monospace;
  color: #ddd6fe;
}
.markdown-rendered pre code { padding: 0; background: transparent; color: #e8eaef; display: block; }
.markdown-rendered table {
  width: 100%;
  margin: 1.25rem 0 1.5rem;
  border-collapse: collapse;
  font-size: 0.92rem;
}
.markdown-rendered th, .markdown-rendered td {
  padding: 0.55rem 0.75rem;
  border: 1px solid var(--rr-border);
  text-align: left;
}
.markdown-rendered th {
  background: var(--rr-surface-2);
  font-weight: 600;
}
.markdown-rendered tr:nth-child(even) td { background: rgba(127,127,127,0.06); }
.markdown-rendered hr {
  margin: 2rem 0;
  border: none;
  border-top: 1px solid var(--rr-border);
}
.markdown-rendered a {
  text-decoration: underline;
  text-underline-offset: 3px;
}
.markdown-rendered a:hover {
  text-decoration-color: var(--rr-accent);
}
.markdown-rendered img {
  max-width: 100%;
  height: auto;
  margin: 1.25rem 0;
  border-radius: 10px;
  border: 1px solid var(--rr-border);
}
.markdown-rendered blockquote {
  margin: 1.35rem 0;
  padding: 0.85rem 1.15rem 0.85rem 1.25rem;
  border-left: 4px solid var(--rr-accent);
  background: var(--rr-quote-bg);
  border-radius: 0 10px 10px 0;
  font-style: italic;
}
.markdown-rendered blockquote p { margin: 0.35rem 0; }
.research-report-sources {
  margin-top: 3rem;
  padding-top: 2rem;
  border-top: 2px solid var(--rr-border);
}
.research-report-sources h2 {
  font-size: 1.2rem;
  margin: 0 0 1.25rem;
  font-weight: 650;
}
.research-sources-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(15rem, 1fr));
  gap: 1rem;
}
.research-source-card {
  display: flex;
  flex-direction: column;
  overflow: hidden;
  background: var(--rr-surface);
  border: 1px solid var(--rr-border);
  border-radius: 12px;
  text-decoration: none;
  color: inherit;
  transition: border-color 0.15s, transform 0.15s, box-shadow 0.15s;
}
.research-source-card:hover {
  border-color: var(--rr-accent);
  transform: translateY(-2px);
  box-shadow: 0 8px 24px rgba(124, 58, 237, 0.2);
}
.research-source-thumb {
  width: 100%;
  height: 5.5rem;
  object-fit: cover;
  background: var(--rr-surface-2);
  border-bottom: 1px solid var(--rr-border);
}
.research-source-thumb-placeholder {
  height: 3rem;
  background: linear-gradient(90deg, var(--rr-surface-2), #2a2248);
  border-bottom: 1px solid var(--rr-border);
}
.research-source-card .body { padding: 0.85rem 1rem 1rem; }
.research-source-card .title {
  font-weight: 600;
  font-size: 0.92rem;
  color: var(--rr-text);
  margin-bottom: 0.3rem;
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
}
.research-source-card .domain {
  font-size: 0.78rem;
  color: var(--rr-muted);
}
@media print {
  body { background: #fff; color: #111; padding: 0; }
  .research-report-cover, .research-report-body, .research-source-card {
    background: #fff; color: #111; border-color: #ccc; box-shadow: none;
  }
  .research-report-print-bar { display: none !important; }
  .markdown-rendered a { color: #4338ca; }
}
`;

function escapeHtml(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

export function sanitizeResearchReportBasename(topic: string, maxLen = 48): string {
  const line = topic.split("\n")[0]?.trim() ?? "";
  const s = line
    .toLowerCase()
    .normalize("NFD")
    .replace(/[\u0300-\u036f]/g, "")
    .replace(/[^a-z0-9\s-]/gi, "")
    .replace(/\s+/g, "-")
    .replace(/-+/g, "-")
    .replace(/^-|-$/g, "");
  if (!s) return "research-report";
  return s.length > maxLen ? s.slice(0, maxLen - 1) + "-" : s;
}

async function renderReportBodyHtml(markdown: string): Promise<string> {
  const { renderMarkdownStaticHtml } = await import("./MarkdownContent");
  return renderMarkdownStaticHtml(markdown);
}

function safeOgImageUrl(url?: string): string | null {
  if (!url?.trim()) return null;
  try {
    const u = new URL(url.trim());
    if (u.protocol !== "https:") return null;
    return u.href;
  } catch {
    return null;
  }
}

function buildIllustrationsHtml(meta: ResearchReportMeta): string {
  const imgs = meta.sources
    .map((s) => safeOgImageUrl(s.og_image))
    .filter((u): u is string => Boolean(u))
    .slice(0, 4);
  if (!imgs.length) return "";
  const tags = imgs.map((src) => `<img src="${escapeHtml(src)}" alt="" loading="lazy" decoding="async" />`).join("");
  return `<div class="research-report-illustrations" aria-hidden="true">${tags}</div>`;
}

function buildSourcesHtml(meta: ResearchReportMeta, locale: "fr" | "en"): string {
  if (!meta.sources.length) return "";
  const heading = locale === "en" ? "Sources" : "Sources";
  const cards = meta.sources
    .map((s) => {
      const href = (() => {
        try {
          return new URL(s.url).href;
        } catch {
          return s.url;
        }
      })();
      let domain = href;
      try {
        domain = new URL(href).hostname.replace(/^www\./, "");
      } catch {
        /* keep */
      }
      const thumb = safeOgImageUrl(s.og_image);
      const thumbHtml = thumb
        ? `<img class="research-source-thumb" src="${escapeHtml(thumb)}" alt="" loading="lazy" />`
        : `<div class="research-source-thumb-placeholder"></div>`;
      return `<a class="research-source-card" href="${escapeHtml(href)}" target="_blank" rel="noopener noreferrer">
        ${thumbHtml}
        <div class="body">
          <div class="title">${escapeHtml(s.title || domain)}</div>
          <div class="domain">${escapeHtml(domain)}</div>
        </div>
      </a>`;
    })
    .join("\n");
  return `<section class="research-report-sources">
    <h2>${heading}</h2>
    <div class="research-sources-grid">${cards}</div>
  </section>`;
}

function skipReasonLabel(reason: string, en: boolean): string {
  const map: Record<string, [string, string]> = {
    duplicate: ["Already consulted", "Déjà consultée"],
    per_query_limit: ["Per-query URL limit", "Limite d’URLs par requête"],
    round_limit: ["Round URL budget", "Budget URLs du tour"],
    fetch_or_low_quality: ["Fetch failed or low quality", "Échec de lecture ou qualité insuffisante"],
  };
  const pair = map[reason];
  return pair ? (en ? pair[0] : pair[1]) : reason;
}

function buildMethodologyHtml(meta: ResearchReportMeta, locale: "fr" | "en"): string {
  const en = locale === "en";
  const userQ = meta.user_question?.trim() || "";
  const queries = meta.search_queries ?? [];
  const subQ = meta.sub_questions ?? [];
  const skipped = meta.sources_skipped ?? [];
  const pages = meta.pages_fetched ?? meta.sources.length;
  const hits = meta.search_hits_seen ?? 0;
  const images = meta.images_retrieved ?? 0;
  const used = meta.sources.length;
  const domains = meta.unique_domains ?? 0;
  const sufficient = meta.sources_sufficient !== false;
  const warnings = meta.quality_warnings ?? [];

  const queriesList =
    queries.length > 0
      ? `<ul>${queries
          .slice(0, 40)
          .map((q) => `<li>${escapeHtml(q)}</li>`)
          .join("")}${queries.length > 40 ? `<li>… (+${queries.length - 40})</li>` : ""}</ul>`
      : `<p class="muted">${en ? "No search queries recorded." : "Aucune requête enregistrée."}</p>`;

  const subList =
    subQ.length > 0
      ? `<ul>${subQ.map((q) => `<li>${escapeHtml(q)}</li>`).join("")}</ul>`
      : "";

  const skippedList =
    skipped.length > 0
      ? `<ul class="skipped-list">${skipped
          .slice(0, 25)
          .map(
            (s) =>
              `<li><a href="${escapeHtml(s.url)}" target="_blank" rel="noopener">${escapeHtml(s.title || s.url)}</a> — ${escapeHtml(skipReasonLabel(s.reason, en))}</li>`,
          )
          .join("")}${skipped.length > 25 ? `<li>… (+${skipped.length - 25})</li>` : ""}</ul>`
      : `<p class="muted">${en ? "No sources set aside." : "Aucune source mise de côté."}</p>`;

  const usedList =
    used > 0
      ? `<ul>${meta.sources
          .slice(0, 30)
          .map(
            (s) =>
              `<li><a href="${escapeHtml(s.url)}" target="_blank" rel="noopener">${escapeHtml(s.title || s.url)}</a></li>`,
          )
          .join("")}${used > 30 ? `<li>… (+${used - 30})</li>` : ""}</ul>`
      : "";

  return `<section class="research-report-methodology">
    <h2>${en ? "Methodology" : "Méthodologie"}</h2>
    ${
      userQ
        ? `<h3>${en ? "Original question" : "Question posée"}</h3>
    <p class="user-question">${escapeHtml(userQ)}</p>`
        : ""
    }
    <div class="method-stats">
      <span>${en ? "Search providers" : "Fournisseurs"} : ${escapeHtml(meta.providers_used.join(", ") || (meta.search_degraded ? "—" : "brave"))}</span>
      <span>${en ? "Rounds" : "Tours"} : ${meta.rounds}</span>
      <span>${en ? "Duration" : "Durée"} : ${meta.duration_secs}s</span>
      <span>${en ? "Search hits seen" : "Résultats vus"} : ${hits}</span>
      <span>${en ? "Pages read" : "Pages lues"} : ${pages}</span>
      <span>${en ? "Sources used" : "Sources retenues"} : ${used}</span>
      <span>${en ? "Distinct domains" : "Domaines distincts"} : ${domains}</span>
      <span>${en ? "Sources set aside" : "Sources écartées"} : ${skipped.length}</span>
      <span>${en ? "Images retrieved" : "Images récupérées"} : ${images}</span>
    </div>
    ${
      meta.search_degraded
        ? `<p><strong>${en ? "Note" : "Note"} :</strong> ${en ? "Web search was unavailable — report relies on model knowledge." : "Recherche web indisponible — rapport basé sur les connaissances du modèle."}</p>`
        : !sufficient
          ? `<p><strong>${en ? "Coverage warning" : "Avertissement couverture"} :</strong> ${en ? `Only ${pages} pages were read (below the target for a full report). Treat conclusions with extra caution.` : `Seulement ${pages} pages lues (sous le seuil visé). À interpréter avec prudence.`}</p>`
          : ""
    }
    ${
      warnings.length > 0
        ? `<h3>${en ? "Quality notes" : "Notes de qualité"}</h3><ul>${warnings
            .slice(0, 8)
            .map((w) => `<li>${escapeHtml(w)}</li>`)
            .join("")}</ul>`
        : ""
    }
    ${subList ? `<h3>${en ? "Investigation angles (plan)" : "Angles d’investigation (plan)"}</h3>${subList}` : ""}
    <h3>${en ? "Search queries executed" : "Requêtes de recherche exécutées"}</h3>
    ${queriesList}
    <h3>${en ? "Sources used in the report" : "Sources retenues pour le rapport"}</h3>
    ${usedList || `<p class="muted">—</p>`}
    <h3>${en ? "Sources set aside" : "Sources mises de côté"}</h3>
    ${skippedList}
  </section>`;
}

function buildCoverHtml(doc: ResearchReportDocument, locale: "fr" | "en"): string {
  const title = escapeHtml(reportCoverTitle(doc));
  const date = escapeHtml(doc.dateLabel);
  const meta = doc.reportMeta;
  const en = locale === "en";
  const stats = meta
    ? `<div class="research-report-stats-grid">
        <span>${escapeHtml(meta.category)}</span>
        <span>${meta.word_count} ${en ? "words" : "mots"}</span>
        <span>${meta.sources.length} ${en ? "sources" : "sources"}</span>
        <span>${meta.rounds} ${en ? "rounds" : "tours"} · ${meta.duration_secs}s</span>
        ${meta.search_degraded ? `<span>${en ? "degraded (no web)" : "dégradé (sans web)"}</span>` : ""}
        ${meta.sources_sufficient === false ? `<span>${en ? "thin sources" : "sources limitées"}</span>` : ""}
      </div>`
    : "";
  const methodology = meta ? buildMethodologyHtml(meta, locale) : "";
  const illustrations = meta ? buildIllustrationsHtml(meta) : "";

  return `<header class="research-report-cover">
    <h1>${title}</h1>
    <p class="meta">${date}</p>
    ${stats}
    ${illustrations}
  </header>
  ${methodology}`;
}

function buildFullReportCss(theme: ResearchReportTheme): string {
  return `${buildReportThemeCss(theme)}${STANDALONE_MARKDOWN_CSS_BASE}`;
}

export async function buildResearchReportHtml(
  doc: ResearchReportDocument,
  options?: { printBar?: { hint: string; buttonLabel: string } },
): Promise<string> {
  const locale = doc.locale ?? "fr";
  const { preprocessResearchReportMarkdown } = await import("./researchReportMarkdown");
  const sourcesList = doc.reportMeta?.sources ?? [];
  const processedMd = preprocessResearchReportMarkdown(doc.reportMarkdown, sourcesList, locale);
  const bodyHtml = await renderReportBodyHtml(processedMd);
  const theme = doc.displayTheme ?? "dark";
  const title = escapeHtml(reportCoverTitle(doc));
  const cover = buildCoverHtml(doc, locale);
  const sources = doc.reportMeta ? buildSourcesHtml(doc.reportMeta, locale) : "";
  const lang = locale === "en" ? "en" : "fr";
  const css = buildFullReportCss(theme);
  const printBar = options?.printBar
    ? `<div class="research-report-print-bar no-print">
        <span>${escapeHtml(options.printBar.hint)}</span>
        <button type="button" onclick="window.print()">${escapeHtml(options.printBar.buttonLabel)}</button>
      </div>`
    : "";

  return `<!DOCTYPE html>
<html lang="${lang}">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>${title}</title>
  <style>${css}</style>
</head>
<body>
  <div class="research-report-page">
    ${printBar}
    ${cover}
    <main class="research-report-body markdown-rendered">
    ${bodyHtml}
    </main>
    ${sources}
  </div>
</body>
</html>`;
}

function openHtmlInNewWindow(html: string, windowName?: string): Window | null {
  const win = window.open("", windowName ?? "_blank", "noopener,noreferrer");
  if (!win) return null;
  win.document.open();
  win.document.write(html);
  win.document.close();
  return win;
}

export async function openResearchReportWindow(doc: ResearchReportDocument): Promise<boolean> {
  const html = await buildResearchReportHtml(doc);
  if (await isTauriDesktop()) {
    return openHtmlInTauriWindow(html, reportCoverTitle(doc));
  }
  return openHtmlInNewWindow(html) !== null;
}

export async function downloadResearchReportHtml(doc: ResearchReportDocument): Promise<void> {
  const html = await buildResearchReportHtml(doc);
  const base = sanitizeResearchReportBasename(reportCoverTitle(doc));
  const blob = new Blob([html], { type: "text/html;charset=utf-8" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = `${base}.html`;
  a.rel = "noopener";
  a.click();
  URL.revokeObjectURL(url);
}

export async function exportResearchReportPdf(
  doc: ResearchReportDocument,
  labels: { hint: string; buttonLabel: string },
): Promise<boolean> {
  const html = await buildResearchReportHtml(doc, { printBar: labels });
  if (await isTauriDesktop()) {
    return openHtmlInTauriWindow(html, reportCoverTitle(doc), true);
  }
  const win = openHtmlInNewWindow(html, "akasha-research-pdf");
  if (!win) return false;
  const triggerPrint = () => {
    try {
      win.focus();
      win.print();
    } catch {
      /* user may block print */
    }
  };
  if (win.document.readyState === "complete") {
    window.setTimeout(triggerPrint, 400);
  } else {
    win.addEventListener("load", () => window.setTimeout(triggerPrint, 400));
  }
  return true;
}
