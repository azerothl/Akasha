import { useCallback, useEffect, useMemo, useState } from "react";
import type { ThemeId } from "../themeTypes";
import { InfoTip } from "./Tooltip";

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

function readThemeDefaults(theme: ThemeId): Record<string, string> {
  const out: Record<string, string> = {};
  if (typeof document === "undefined") return out;
  const root = document.documentElement;
  const prev = root.getAttribute("data-theme");
  root.setAttribute("data-theme", theme);
  for (const { key } of EDITABLE_THEME_VARS) {
    root.style.removeProperty(key);
  }
  for (const { key } of EDITABLE_THEME_VARS) {
    const val = getComputedStyle(root).getPropertyValue(key).trim();
    if (val) out[key] = val;
  }
  if (prev) root.setAttribute("data-theme", prev);
  else root.removeAttribute("data-theme");
  return out;
}

type Props = {
  theme: ThemeId;
  t: (key: string) => string;
  onChange?: (overrides: ThemeOverrides) => void;
};

export function ThemeEditorPanel({ theme, t, onChange }: Props) {
  const [overrides, setOverrides] = useState<ThemeOverrides>(() => loadThemeOverrides());
  const current = useMemo(() => overrides[theme] ?? {}, [overrides, theme]);
  const overrideCount = useMemo(() => themeOverrideCount(overrides, theme), [overrides, theme]);
  const [expanded, setExpanded] = useState(() => themeOverrideCount(loadThemeOverrides(), theme) > 0);
  const [defaults, setDefaults] = useState<Record<string, string>>({});

  useEffect(() => {
    setDefaults(readThemeDefaults(theme));
  }, [theme]);

  useEffect(() => {
    if (overrideCount > 0) setExpanded(true);
  }, [theme, overrideCount]);

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
    <details
      className="theme-editor-details"
      open={expanded}
      onToggle={(e) => setExpanded(e.currentTarget.open)}
    >
      <summary className="theme-editor-summary">
        {t("theme_editor.title")}
        {overrideCount > 0 ? <span className="theme-editor-badge">({overrideCount})</span> : null}
      </summary>
      <div className="theme-editor-panel">
        <p className="settings-doc muted">{t("theme_editor.desc")}</p>
        <dl className="settings-list theme-editor-vars">
          {EDITABLE_THEME_VARS.map(({ key, labelKey, hintKey }) => (
            <div key={key} className="theme-editor-row">
              <dt>
                {t(labelKey)}
                <InfoTip label={t(labelKey)} content={t(hintKey)} />
              </dt>
              <dd>
                <input
                  type="text"
                  className="settings-input theme-editor-input"
                  value={current[key] ?? ""}
                  placeholder={defaults[key] || key}
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
    </details>
  );
}
