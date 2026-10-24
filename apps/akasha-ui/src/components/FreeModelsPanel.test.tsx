// @vitest-environment jsdom
import { act, fireEvent, render, screen, waitFor, cleanup } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { FreeModelsPanel } from "./FreeModelsPanel";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const mockedInvoke = vi.mocked(invoke);
const status = (enabled: boolean) => ({ enabled, openrouter_ready:true, openrouter_fresh:true, catalog: { openrouter:{ checked_at:1,models:[] }, huggingface:{ checked_at:1,models:[] } } });
beforeEach(() => { vi.clearAllMocks(); });
afterEach(() => { cleanup(); vi.restoreAllMocks(); });
describe("strict free-model controls", () => {
  it("enables the strict policy without granting paid fallback permission", async () => {
    mockedInvoke.mockResolvedValueOnce(status(false)).mockResolvedValueOnce(status(true));
    render(<FreeModelsPanel port={3876} />);
    fireEvent.click(await screen.findByRole("checkbox"));
    await waitFor(() => expect(mockedInvoke).toHaveBeenLastCalledWith("free_models_control", { port:3876,action:"set",enabled:true,confirmPaidRoutes:false }));
    await screen.findByText(/Secours payant bloqué/);
  });
  it("keeps the boundary enabled when the user declines paid routes", async () => {
    mockedInvoke.mockResolvedValue(status(true));
    vi.spyOn(window,"confirm").mockReturnValue(false);
    render(<FreeModelsPanel port={3876} />);
    fireEvent.click(await screen.findByRole("checkbox"));
    expect(mockedInvoke).toHaveBeenCalledTimes(1);
    expect((screen.getByRole("checkbox") as HTMLInputElement).checked).toBe(true);
  });
  it("sends explicit confirmation only after the user accepts disabling", async () => {
    mockedInvoke.mockResolvedValueOnce(status(true)).mockResolvedValueOnce(status(false));
    vi.spyOn(window,"confirm").mockReturnValue(true);
    render(<FreeModelsPanel port={3876} />);
    fireEvent.click(await screen.findByRole("checkbox"));
    await waitFor(() => expect(mockedInvoke).toHaveBeenLastCalledWith("free_models_control", { port:3876,action:"set",enabled:false,confirmPaidRoutes:true }));
  });
  it("displays catalog errors and keeps the active policy after a failed update", async () => {
    mockedInvoke.mockResolvedValueOnce(status(true)).mockRejectedValueOnce("catalog unavailable");
    render(<FreeModelsPanel port={3876} />);
    await screen.findByRole("checkbox");
    await act(async () => { fireEvent.click(screen.getByRole("button", { name:"Actualiser le catalogue" })); });
    expect(screen.getByRole("alert").textContent).toContain("catalog unavailable");
    expect((screen.getByRole("checkbox") as HTMLInputElement).checked).toBe(true);
  });
});
