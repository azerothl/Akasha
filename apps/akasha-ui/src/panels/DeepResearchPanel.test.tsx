// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { I18nProvider } from "../useI18n";
import { NotificationProvider } from "../notifications/NotificationContext";
import { DeepResearchPanel } from "./DeepResearchPanel";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

beforeEach(() => {
  localStorage.clear();
});

describe("DeepResearchPanel", () => {
  it("shows the unconsulted-citation diagnostic and blocks report export", async () => {
    const diagnostic = "research_report_unconsulted_citations: https://example.invalid/not-consulted";
    const fetchEndpoint = vi.fn(async (path: string, init?: RequestInit) => {
      if (path === "/api/research/deep/start" && init?.method === "POST") {
        const request = JSON.parse(String(init.body ?? "{}")) as { topic?: string };
        if (!request.topic?.trim()) {
          return { ok: false, status: 400, text: JSON.stringify({ error: "missing_topic" }) };
        }
        return { ok: true, status: 200, text: JSON.stringify({ run_id: "run-235" }) };
      }
      if (path === "/api/research/deep/runs/run-235") {
        return {
          ok: true,
          status: 200,
          text: JSON.stringify({
            id: "run-235",
            topic: "Une recherche avec une citation non consultée",
            phase: "error",
            round: 1,
            max_rounds: 4,
            branches: [],
            evolving_report: "",
            report_markdown: null,
            report_meta: null,
            search_degraded: false,
            error: diagnostic,
          }),
        };
      }
      throw new Error(`Unexpected research API request: ${path}`);
    });
    const openSpy = vi.spyOn(window, "open").mockReturnValue(null);

    render(
      <I18nProvider>
        <NotificationProvider>
          <DeepResearchPanel fetchEndpoint={fetchEndpoint} locale="fr" />
        </NotificationProvider>
      </I18nProvider>,
    );

    await waitFor(() => expect(fetchEndpoint).toHaveBeenCalledWith(
      "/api/research/deep/start",
      expect.objectContaining({ method: "POST" }),
    ));

    fireEvent.change(screen.getByRole("textbox"), {
      target: { value: "Une recherche avec une citation non consultée" },
    });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Lancer la recherche" }));
    });

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain(diagnostic);
    expect(fetchEndpoint).toHaveBeenCalledWith("/api/research/deep/runs/run-235");
    expect(screen.queryByRole("toolbar")).toBeNull();
    expect(screen.queryByLabelText("Rapport de recherche")).toBeNull();
    expect(openSpy).not.toHaveBeenCalled();
  });
});
