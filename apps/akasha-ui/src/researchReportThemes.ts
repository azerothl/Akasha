/** Standalone research report display themes (export / preview window). */

export type ResearchReportTheme = "dark" | "light" | "sepia" | "ocean";

export const RESEARCH_REPORT_THEME_STORAGE_KEY = "akasha_research_report_theme";

const THEME_VARS: Record<ResearchReportTheme, string> = {
  dark: `
  --rr-bg: #0f1117;
  --rr-surface: #181b24;
  --rr-surface-2: #1f2430;
  --rr-text: #e8eaef;
  --rr-muted: #9ca3b8;
  --rr-accent: #a78bfa;
  --rr-accent-2: #7c3aed;
  --rr-border: #2d3344;
  --rr-quote-bg: #221e3a;
  --rr-cover-grad-1: #2d2650;
  --rr-cover-grad-2: #181b24;
  --rr-body-grad: #2a2248;
  --rr-stat-bg: rgba(124, 58, 237, 0.2);
  --rr-stat-text: #ddd6fe;
  --rr-link: #c4b5fd;
  --rr-code-bg: rgba(124, 58, 237, 0.15);
  --rr-code-text: #ddd6fe;
  --rr-h1: #f5f3ff;
  --rr-h2: #e9e5ff;
  --rr-h3: #c4b5fd;
`,
  light: `
  --rr-bg: #f4f4f8;
  --rr-surface: #ffffff;
  --rr-surface-2: #f0f1f6;
  --rr-text: #1a1d26;
  --rr-muted: #5c6370;
  --rr-accent: #5b21b6;
  --rr-accent-2: #6d28d9;
  --rr-border: #dde1ea;
  --rr-quote-bg: #f5f3ff;
  --rr-cover-grad-1: #ede9fe;
  --rr-cover-grad-2: #ffffff;
  --rr-body-grad: #e0e7ff;
  --rr-stat-bg: rgba(109, 40, 217, 0.1);
  --rr-stat-text: #4c1d95;
  --rr-link: #5b21b6;
  --rr-code-bg: #f3f0ff;
  --rr-code-text: #4c1d95;
  --rr-h1: #1e1b4b;
  --rr-h2: #312e81;
  --rr-h3: #4338ca;
`,
  sepia: `
  --rr-bg: #f7f1e3;
  --rr-surface: #fffdf8;
  --rr-surface-2: #f3ead8;
  --rr-text: #3d3428;
  --rr-muted: #6b5d4d;
  --rr-accent: #9a3412;
  --rr-accent-2: #c2410c;
  --rr-border: #e7dcc8;
  --rr-quote-bg: #f5ebe0;
  --rr-cover-grad-1: #fde8d0;
  --rr-cover-grad-2: #fffdf8;
  --rr-body-grad: #e8d4b8;
  --rr-stat-bg: rgba(194, 65, 12, 0.12);
  --rr-stat-text: #7c2d12;
  --rr-link: #9a3412;
  --rr-code-bg: #f5ebe0;
  --rr-code-text: #7c2d12;
  --rr-h1: #431407;
  --rr-h2: #7c2d12;
  --rr-h3: #9a3412;
`,
  ocean: `
  --rr-bg: #0a1628;
  --rr-surface: #0f2137;
  --rr-surface-2: #152a45;
  --rr-text: #e2eaf4;
  --rr-muted: #8ba3bc;
  --rr-accent: #38bdf8;
  --rr-accent-2: #0284c7;
  --rr-border: #1e3a5f;
  --rr-quote-bg: #0c2340;
  --rr-cover-grad-1: #0c4a6e;
  --rr-cover-grad-2: #0f2137;
  --rr-body-grad: #164e63;
  --rr-stat-bg: rgba(2, 132, 199, 0.25);
  --rr-stat-text: #bae6fd;
  --rr-link: #7dd3fc;
  --rr-code-bg: rgba(56, 189, 248, 0.12);
  --rr-code-text: #bae6fd;
  --rr-h1: #f0f9ff;
  --rr-h2: #e0f2fe;
  --rr-h3: #7dd3fc;
`,
};

export function loadResearchReportTheme(): ResearchReportTheme {
  try {
    const raw = localStorage.getItem(RESEARCH_REPORT_THEME_STORAGE_KEY);
    if (raw === "light" || raw === "sepia" || raw === "ocean" || raw === "dark") return raw;
  } catch {
    /* ignore */
  }
  return "dark";
}

export function saveResearchReportTheme(theme: ResearchReportTheme): void {
  try {
    localStorage.setItem(RESEARCH_REPORT_THEME_STORAGE_KEY, theme);
  } catch {
    /* ignore */
  }
}

export function buildReportThemeCss(theme: ResearchReportTheme): string {
  const vars = THEME_VARS[theme] ?? THEME_VARS.dark;
  return `
:root { ${vars}
}
body {
  background: radial-gradient(ellipse 120% 80% at 50% -20%, var(--rr-body-grad) 0%, var(--rr-bg) 55%);
}
.research-report-cover {
  background: linear-gradient(145deg, var(--rr-cover-grad-1) 0%, var(--rr-surface) 45%, var(--rr-cover-grad-2) 100%);
}
.research-report-cover h1 { color: var(--rr-h1); }
.research-report-stats-grid span {
  background: var(--rr-stat-bg);
  color: var(--rr-stat-text);
  border-color: var(--rr-border);
}
.markdown-rendered h1 { color: var(--rr-h1); }
.markdown-rendered h2 { color: var(--rr-h2); }
.markdown-rendered h3 { color: var(--rr-h3); }
.markdown-rendered a { color: var(--rr-link); }
.markdown-rendered code { background: var(--rr-code-bg); color: var(--rr-code-text); }
.markdown-rendered blockquote { color: var(--rr-muted); }
`;
}

export const RESEARCH_REPORT_THEME_OPTIONS: { id: ResearchReportTheme; labelFr: string; labelEn: string }[] = [
  { id: "dark", labelFr: "Sombre", labelEn: "Dark" },
  { id: "light", labelFr: "Clair", labelEn: "Light" },
  { id: "sepia", labelFr: "Sépia", labelEn: "Sepia" },
  { id: "ocean", labelFr: "Océan", labelEn: "Ocean" },
];
