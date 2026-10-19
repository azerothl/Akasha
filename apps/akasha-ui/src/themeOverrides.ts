import type { ThemeId } from "./themeTypes";

export const THEME_OVERRIDE_STORAGE_KEY = "akasha_theme_css_overrides";

/** CSS variables users may override per theme. */
export const EDITABLE_THEME_VARS: Array<{ key: string; labelKey: string; hintKey: string }> = [
  { key: "--akasha-bg", labelKey: "theme_editor.var_bg", hintKey: "theme_editor.var_bg_hint" },
  { key: "--akasha-bg-surface", labelKey: "theme_editor.var_surface", hintKey: "theme_editor.var_surface_hint" },
  { key: "--akasha-text", labelKey: "theme_editor.var_text", hintKey: "theme_editor.var_text_hint" },
  { key: "--akasha-text-muted", labelKey: "theme_editor.var_text_muted", hintKey: "theme_editor.var_text_muted_hint" },
  { key: "--akasha-accent", labelKey: "theme_editor.var_accent", hintKey: "theme_editor.var_accent_hint" },
  { key: "--akasha-border", labelKey: "theme_editor.var_border", hintKey: "theme_editor.var_border_hint" },
  { key: "--akasha-success", labelKey: "theme_editor.var_success", hintKey: "theme_editor.var_success_hint" },
  { key: "--akasha-error", labelKey: "theme_editor.var_error", hintKey: "theme_editor.var_error_hint" },
];

export type ThemeOverrides = Record<ThemeId, Record<string, string>>;

export function loadThemeOverrides(): ThemeOverrides {
  try {
    const raw = localStorage.getItem(THEME_OVERRIDE_STORAGE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as ThemeOverrides;
      if (parsed && typeof parsed === "object") return parsed;
    }
  } catch {
    /* ignore */
  }
  return {} as ThemeOverrides;
}

export function saveThemeOverrides(overrides: ThemeOverrides) {
  try {
    localStorage.setItem(THEME_OVERRIDE_STORAGE_KEY, JSON.stringify(overrides));
  } catch {
    /* ignore */
  }
}

export function applyThemeOverrides(theme: ThemeId, overrides: ThemeOverrides) {
  const root = document.documentElement;
  for (const { key } of EDITABLE_THEME_VARS) {
    root.style.removeProperty(key);
  }
  const themeVars = overrides[theme];
  if (!themeVars) return;
  for (const [key, value] of Object.entries(themeVars)) {
    if (value.trim()) {
      root.style.setProperty(key, value.trim());
    }
  }
}

export function themeOverrideCount(overrides: ThemeOverrides, theme: ThemeId): number {
  return Object.keys(overrides[theme] ?? {}).filter((k) => (overrides[theme]?.[k] ?? "").trim()).length;
}

