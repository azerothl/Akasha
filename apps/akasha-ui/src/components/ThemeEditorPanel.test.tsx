// @vitest-environment jsdom
import { describe, expect, it, beforeEach, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import {
  ThemeEditorPanel,
  THEME_OVERRIDE_STORAGE_KEY,
  themeOverrideCount,
  loadThemeOverrides,
} from "./ThemeEditorPanel";

const t = (key: string) => key;

describe("ThemeEditorPanel", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.stubGlobal(
      "getComputedStyle",
      vi.fn(() => ({
        getPropertyValue: (key: string) => (key === "--akasha-bg" ? "#0d1117" : ""),
      })),
    );
  });

  it("is collapsed by default when no overrides exist", () => {
    render(<ThemeEditorPanel theme="dark_akasha" t={t} />);
    const details = document.querySelector("details.theme-editor-details");
    expect(details).not.toBeNull();
    expect(details?.hasAttribute("open")).toBe(false);
  });

  it("counts overrides from localStorage for open state", () => {
    localStorage.setItem(
      THEME_OVERRIDE_STORAGE_KEY,
      JSON.stringify({ dark_akasha: { "--akasha-accent": "#ff0000" } }),
    );
    const overrides = loadThemeOverrides();
    expect(themeOverrideCount(overrides, "dark_akasha")).toBe(1);
    render(<ThemeEditorPanel theme="dark_akasha" t={t} />);
    expect(screen.getByText("(1)")).toBeTruthy();
  });

  it("themeOverrideCount counts non-empty values", () => {
    const overrides = loadThemeOverrides();
    expect(themeOverrideCount(overrides, "dark_akasha")).toBe(0);
    const withOne = {
      ...overrides,
      dark_akasha: { "--akasha-bg": "#111", "--akasha-text": "" },
    };
    expect(themeOverrideCount(withOne, "dark_akasha")).toBe(1);
  });
});
