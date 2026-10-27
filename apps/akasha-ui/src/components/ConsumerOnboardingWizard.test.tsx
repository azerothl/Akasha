// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { ConsumerOnboardingWizard } from "./ConsumerOnboardingWizard";
afterEach(cleanup);

it.each([true, false])("prepares an embedded candidate with installed=%s without a redundant download", async (installed) => {
  const calls: { path: string; body?: unknown }[] = [];
  const fetchEndpoint = vi.fn(async (path: string, init?: RequestInit) => {
    calls.push({ path, body: init?.body ? JSON.parse(init.body as string) : undefined });
    let value: unknown = {};
    if (path === "/api/router/embedded/models") value = { models: [{ id: "local-fixture", label: "Local fixture" }] };
    if (path === "/api/capabilities") value = { models: [{ id: "local-fixture", gguf_on_disk: installed }, { id: "another-model", gguf_on_disk: true }] };
    if (path === "/api/router/embedded/download") {
      if (installed) throw new Error("Offline: a local model must not be downloaded");
    }
    return { ok: true, status: 200, text: JSON.stringify(value) };
  });
  render(<ConsumerOnboardingWizard locale="fr" daemonOk fetchEndpoint={fetchEndpoint} onComplete={vi.fn()} onDismiss={vi.fn()} />);
  await screen.findByRole("option", { name: "Local fixture" });
  fireEvent.click(screen.getByRole("button", { name: "Préparer ce modèle" }));
  if (installed) {
    await screen.findByText("Modèle local sélectionné. La démonstration vérifiera sa réponse.");
    expect(calls.filter(call => call.path === "/api/router/embedded/download")).toHaveLength(0);
    expect(calls).toContainEqual({ path: "/api/router/embedded/runtime", body: { model_id: "local-fixture", engine_mode: "auto" } });
    expect(calls).toContainEqual({ path: "/api/router/route", body: { category: "conversation", provider: "akasha_core", model: "local-fixture" } });
  } else {
    await waitFor(() => expect(calls).toContainEqual({ path: "/api/router/embedded/download", body: { id: "local-fixture" } }));
    expect(calls.filter(call => call.path === "/api/router/route")).toHaveLength(0);
  }
});

function setup(answer: string) {
  const fetchEndpoint = vi.fn().mockImplementation((path: string, init?: RequestInit) => {
    let value: unknown = {};
    if (path === "/api/privacy") value = { memory: { state: "encrypted", encrypted_at_rest: true } };
    if (path === "/api/router/embedded/models") value = { models: [] };
    if (path === "/api/router/ollama/models") value = { models: ["qwen3.5:9b"] };
    if (path === "/api/message") {
      const body = JSON.parse(init?.body as string);
      expect(body.composer_mode).toBe("ask");
      expect(body.incognito).toBe(true);
      expect(body.remote_consents).toBeUndefined();
      expect(body.attachments[0].name).toBe("demo-facture.txt");
      value = { task_id: "demo" };
    }
    if (path === "/api/tasks/demo") value = { status: "completed", outcome: { response: answer } };
    return Promise.resolve({ ok: true, status: 200, text: JSON.stringify(value) });
  });
  const complete = vi.fn();
  render(<ConsumerOnboardingWizard locale="fr" daemonOk fetchEndpoint={fetchEndpoint} onComplete={complete} onDismiss={vi.fn()} />);
  return { complete, fetchEndpoint };
}
async function reachDemo() {
  await screen.findByRole("option", { name: /Qwen3.5/ });
  const next = screen.getByRole("button", { name: "Continuer" }) as HTMLButtonElement;
  expect(next.disabled).toBe(true);
  fireEvent.click(screen.getByRole("button", { name: "Préparer ce modèle" }));
  await waitFor(() => expect(next.disabled).toBe(false));
  fireEvent.click(next);
  expect((screen.getByRole("button", { name: "Continuer" }) as HTMLButtonElement).disabled).toBe(true);
  fireEvent.click(screen.getByRole("checkbox"));
  fireEvent.click(screen.getByRole("button", { name: "Continuer" }));
}
it("validates the source fields before recording first success", async () => {
  const { complete } = setup('{"montant":42,"echeance":"2026-11-10","source":"demo-facture.txt"}');
  await reachDemo();
  fireEvent.click(screen.getByRole("button", { name: "Comprendre ce document" }));
  const finish = screen.getByRole("button", { name: "Commencer avec Akasha" }) as HTMLButtonElement;
  await waitFor(() => expect(finish.disabled).toBe(false));
  expect(complete).not.toHaveBeenCalled();
  fireEvent.click(finish);
  expect(complete).toHaveBeenCalledOnce();
});
it("does not accept a completed task whose answer has not been verified", async () => {
  const { complete } = setup("Tout est fait !");
  await reachDemo();
  fireEvent.click(screen.getByRole("button", { name: "Comprendre ce document" }));
  expect(await screen.findByRole("alert")).toBeDefined();
  expect((screen.getByRole("button", { name: "Commencer avec Akasha" }) as HTMLButtonElement).disabled).toBe(true);
  expect(complete).not.toHaveBeenCalled();
});
