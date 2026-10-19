// @vitest-environment jsdom
import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { InfoTip } from "../components/Tooltip";
import { I18nProvider } from "../useI18n";

describe("InfoTip", () => {
  it("renders trigger with aria help role", () => {
    localStorage.setItem("akasha_locale", "fr");
    render(<I18nProvider><InfoTip label="Help" content="Tooltip body" /></I18nProvider>);
    expect(screen.getByRole("button", { name: "Aide : Help" })).toBeTruthy();
    expect(screen.getByText("Help", { selector: ".sr-only" })).toBeTruthy();
  });
});
