import { useCallback, useMemo, useState } from "react";
import type { ThemeId } from "../themeTypes";

export const THEME_OVERRIDE_STORAGE_KEY = "akasha_theme_css_overrides";

/** CSS variables users may override per theme. */
export const EDITABLE_THEME_VARS: Array<{ key: string; labelKey: string }> = [
  { key: "--akasha-bg", labelKey: "theme_editor.var_bg" },
  { key: "--akasha-bg-surface", labelKey: "theme_editor.var_surface" },
  { key: "--akasha-text", labelKey: "theme_editor.var_text" },
  { key: "--akasha-text-muted", labelKey: "theme_editor.var_text_muted" },
  { key: "--akasha-accent", labelKey: "theme_editor.var_accent" },
  { key: "--akasha-border", labelKey: "theme_editor.var_border" },
  { key: "--akasha-success", labelKey: "theme_editor.var_success" },
  { key: "--akasha-error", labelKey: "theme_editor.var_error" },
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

type Props = {
  theme: ThemeId;
  t: (key: string) => string;
  onChange?: (overrides: ThemeOverrides) => void;
};

export function ThemeEditorPanel({ theme, t, onChange }: Props) {
  const [overrides, setOverrides] = useState<ThemeOverrides>(() => loadThemeOverrides());
  const current = useMemo(() => overrides[theme] ?? {}, [overrides, theme]);

  const setVar = useCallback(
    (cssVar: string, value: string) => {
      setOverrides((prev) => {
        const next: ThemeOverrides = { ...prev };
        const themeMap = { ...(next[theme] ?? {}) };
        if (value.trim()) {
          themeMap[cssVar] = value;
        } else {
          delete themeMap[cssVar];
        }
        if (Object.keys(themeMap).length === 0) {
          delete next[theme];
        } else {
          next[theme] = themeMap;
        }
        saveThemeOverrides(next);
        applyThemeOverrides(theme, next);
        onChange?.(next);
        return next;
      });
    },
    [theme, onChange],
  );

  const resetTheme = useCallback(() => {
    setOverrides((prev) => {
      const next = { ...prev };
      delete next[theme];
      saveThemeOverrides(next);
      applyThemeOverrides(theme, next);
      onChange?.(next);
      return next;
    });
  }, [theme, onChange]);

  return (
    <div className="theme-editor-panel">
      <p className="settings-doc muted">{t("theme_editor.desc")}</p>
      <dl className="settings-list theme-editor-vars">
        {EDITABLE_THEME_VARS.map(({ key, labelKey }) => (
          <div key={key} className="theme-editor-row">
            <dt>{t(labelKey)}</dt>
            <dd>
              <input
                type="text"
                className="settings-input theme-editor-input"
                value={current[key] ?? ""}
                placeholder={key}
                onChange={(e) => setVar(key, e.target.value)}
                aria-label={`${t(labelKey)} (${key})`}
              />
            </dd>
          </div>
        ))}
      </dl>
      <button type="button" className="btn-secondary" onClick={resetTheme}>
        {t("theme_editor.reset")}
      </button>
    </div>
  );
}
