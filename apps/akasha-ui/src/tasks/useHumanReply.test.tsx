// @vitest-environment jsdom
import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { useHumanReply } from "./useHumanReply";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const mockedInvoke = vi.mocked(invoke);
function setup() {
  const refresh = vi.fn().mockResolvedValue(undefined);
  const accepted = vi.fn();
  const hook = renderHook(() => useHumanReply({
    pending: { task: { question: "Continue?", context: "", request_id: "current" } },
    port: 3876, english: false, refresh, accepted,
  }));
  return { ...hook, refresh, accepted };
}
beforeEach(() => vi.clearAllMocks());
describe("human reply delivery", () => {
  it("sends the exact approval identity and blocks simultaneous clicks", async () => {
    let resolve!: (value: { ok: boolean }) => void;
    mockedInvoke.mockImplementation(() => new Promise((done) => { resolve = done as typeof resolve; }));
    const { result, accepted } = setup();
    let first!: Promise<void>;
    act(() => { first = result.current.submit("task", "Continuer"); });
    expect(result.current.busy).toBe(true);
    await act(async () => { await result.current.submit("task", "Arrêter"); });
    expect(mockedInvoke).toHaveBeenCalledTimes(1);
    expect(mockedInvoke).toHaveBeenCalledWith("post_task_human_reply", {
      taskId: "task", requestId: "current", response: "Continuer", port: 3876,
    });
    await act(async () => { resolve({ ok: true }); await first; });
    expect(accepted).toHaveBeenCalledWith("task", "current");
    expect(result.current.busy).toBe(false);
  });
  it.each(["404 Not Found", "410 Gone: human_input_expired", "409 Conflict: stale_approval_request"])("refreshes an obsolete question after %s without replaying the answer", async (error) => {
    mockedInvoke.mockRejectedValueOnce(error);
    const { result, accepted, refresh } = setup();
    await act(async () => { await result.current.submit("task", "Continuer"); });
    expect(result.current.error).toMatch(/expiré|changé/);
    expect(accepted).not.toHaveBeenCalled();
    expect(refresh).toHaveBeenCalledOnce();
    expect(mockedInvoke).toHaveBeenCalledOnce();
    expect(result.current.busy).toBe(false);
  });
  it("keeps the question and allows retry after a connection failure", async () => {
    mockedInvoke.mockRejectedValueOnce("network unavailable").mockResolvedValueOnce({ ok: true });
    const { result, accepted } = setup();
    await act(async () => { await result.current.submit("task", "Mon texte"); });
    expect(result.current.error).toContain("Votre texte est conservé");
    expect(accepted).not.toHaveBeenCalled();
    await act(async () => { await result.current.submit("task", "Mon texte"); });
    expect(accepted).toHaveBeenCalledOnce();
    expect(result.current.error).toBeNull();
  });
  it("requires an explicit acknowledgement before clearing the question", async () => {
    mockedInvoke.mockResolvedValueOnce({});
    const { result, accepted } = setup();
    await act(async () => { await result.current.submit("task", "Oui"); });
    expect(accepted).not.toHaveBeenCalled();
    expect(result.current.error).toContain("Impossible");
  });
});
