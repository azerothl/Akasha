import { describe, expect, it } from "vitest";
import {
  dedupeExecutiveSummarySections,
  linkNumericCitations,
  preprocessResearchReportMarkdown,
} from "./researchReportMarkdown";

describe("researchReportMarkdown", () => {
  it("keeps one executive summary heading", () => {
    const md = `## Executive summary\n\nEN text.\n\n## Résumé exécutif\n\nFR text.\n\n## Body\n\nMore.`;
    const out = dedupeExecutiveSummarySections(md, "fr");
    expect(out).toContain("## Résumé exécutif");
    expect(out).not.toMatch(/## Executive summary/i);
    expect((out.match(/## Résumé exécutif/gi) ?? []).length).toBe(1);
    expect(out).toContain("## Body");
  });

  it("links numeric citations to sources", () => {
    const md = "Claim supported [1] and also [2].";
    const out = linkNumericCitations(md, [
      { title: "A", url: "https://a.test/1" },
      { title: "B", url: "https://b.test/2" },
    ]);
    expect(out).toContain("[A](https://a.test/1)");
    expect(out).toContain("[B](https://b.test/2)");
  });
});

describe("preprocessResearchReportMarkdown", () => {
  it("dedupes and fixes links together", () => {
    const md = `## Executive summary\n\nx\n\n## Résumé exécutif\n\ny\n\n## Details\n\nSee [1].`;
    const out = preprocessResearchReportMarkdown(
      md,
      [{ title: "Src", url: "https://example.com" }],
      "en",
    );
    expect(out).toContain("[Src](https://example.com)");
    expect(out.split(/executive summary/i).length).toBeLessThanOrEqual(2);
  });
});
