import { describe, expect, it, vi } from "vitest";

vi.mock("./MarkdownContent", () => ({
  renderMarkdownStaticHtml: (md: string) => `<div class="mock-md">${md}</div>`,
}));

import { buildResearchReportHtml, type ResearchReportDocument } from "./researchReportExport";

describe("buildResearchReportHtml", () => {
  it("includes cover stats and sources when reportMeta present", async () => {
    const doc: ResearchReportDocument = {
      topic: "Rust vs Go",
      dateLabel: "2026-06-03",
      reportMarkdown:
        "## Executive summary\n\nBoth languages are great.\n\n## Résumé exécutif\n\nDuplicate.\n\n## Details\n\nMore text.",
      locale: "en",
      reportMeta: {
        report_title: "Rust and Go for Backend Systems",
        user_question: "Rust vs Go?",
        category: "comparison",
        word_count: 1200,
        sources: [{ title: "Example", url: "https://example.com/page" }],
        providers_used: ["brave"],
        rounds: 3,
        duration_secs: 180,
        search_degraded: false,
        search_queries: ["site:en.wikipedia.org Rust Go", "Rust vs Go benchmark"],
        pages_fetched: 12,
        search_hits_seen: 40,
        images_retrieved: 2,
        sources_skipped: [
          { url: "https://spam.test/x", title: "Spam", reason: "fetch_or_low_quality" },
        ],
      },
      displayTheme: "light",
    };
    const html = await buildResearchReportHtml(doc);
    expect(html).toContain("research-report-cover");
    expect(html).toContain("Rust and Go for Backend Systems");
    expect(html).toContain("Original question");
    expect(html).toContain("Rust vs Go?");
    expect(html).toContain("Search queries executed");
    expect(html).toContain("comparison");
    expect(html).toContain("1200 words");
    expect(html).toContain("research-sources-grid");
    expect(html).toContain("https://example.com/page");
    expect(html).toContain("Executive summary");
    expect(html).not.toContain("Résumé exécutif");
    expect(html).toContain('lang="en"');
  });

  it("shows coverage warning when sources_sufficient is false", async () => {
    const doc: ResearchReportDocument = {
      topic: "Thin topic",
      dateLabel: "2026-06-04",
      reportMarkdown: "## Executive summary\n\nBrief.",
      locale: "en",
      reportMeta: {
        report_title: "Thin Evidence Study",
        category: "general",
        word_count: 400,
        sources: [{ title: "A", url: "https://a.test/1" }],
        providers_used: ["brave"],
        rounds: 2,
        duration_secs: 120,
        search_degraded: false,
        pages_fetched: 3,
        sources_sufficient: false,
        quality_warnings: ["Only 3 pages were successfully read."],
      },
    };
    const html = await buildResearchReportHtml(doc);
    expect(html).toContain("Coverage warning");
    expect(html).toContain("thin sources");
    expect(html).toContain("Quality notes");
  });
});
