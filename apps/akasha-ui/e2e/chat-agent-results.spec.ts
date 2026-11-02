import { expect, test } from "@playwright/test";
import path from "node:path";

// The React application drives an actual isolated daemon. Only the native IPC
// bridge is replaced, since Chromium cannot host Tauri commands.
const daemon = process.env.AUDIT_DAEMON_BASE;
const provider = process.env.AUDIT_PROVIDER_BASE;
test.skip(!daemon || !provider, "Run through spec/dev/quality/chat_agent_audit.py --ui");

test.beforeEach(async ({ page, request }) => {
  await page.addInitScript(() => {
    localStorage.clear();
    localStorage.setItem("akasha_setup_wizard_done", "1");
    localStorage.setItem("akasha_onboarding_dismissed", "1");
    localStorage.setItem("akasha_locale", "fr");
    localStorage.setItem("akasha_chat_composer_mode", "ask");
    localStorage.setItem("akasha_chat_web_search", "1");
    Object.assign(window, { __TAURI_INTERNALS__: {
      invoke: async (command: string, args: unknown) => {
        const response = await fetch("/__audit_ipc", { method: "POST", body: JSON.stringify({ command, args }) });
        if (!response.ok) throw new Error(await response.text());
        return response.json();
      },
      transformCallback: () => 1,
      unregisterCallback: () => {},
    } });
  });
  await page.route("**/__audit_ipc", async route => {
    const { command, args = {} } = route.request().postDataJSON();
    let value: unknown = {};
    if (command === "check_health") value = { ok: true, port: 3876 };
    else if (command === "send_message_ack") {
      expect(args.displayMessage).not.toContain("Préférence de réponse");
      expect(args.message).toContain("Préférence de réponse");
      const result = await request.post(`${daemon}/api/message`, { data: {
        message: args.message, display_message: args.displayMessage,
        session_id: args.sessionId || `ui-${Date.now()}`, composer_mode: args.composerMode, incognito: true,
        remote_consents: args.remoteConsents,
      } });
      value = await result.json();
    } else if (command === "daemon_get_text" || command === "daemon_request") {
      const response = await request.fetch(`${daemon}${args.path}`, { method: command === "daemon_get_text" ? "GET" : args.method, data: args.body ? JSON.parse(args.body) : undefined });
      value = { ok: response.ok(), status: response.status(), text: await response.text() };
    } else if (command === "get_task_status") value = await (await request.get(`${daemon}/api/tasks/${args.taskId}`)).text();
    else if (command === "get_task_events") value = await (await request.get(`${daemon}/api/tasks/${args.taskId}/events`)).json();
    else if (command === "get_tasks") value = { tasks: [] };
    else if (command === "get_sessions") value = { sessions: [] };
    else if (command === "get_notes") value = { notes: [] };
    else if (command === "get_pending_human_input") value = { pending: [] };
    await route.fulfill({ json: value });
  });
  await page.goto("/");
  await expect(page.getByPlaceholder("Votre message…")).toBeVisible();
});

// Drain background IPC requests before Playwright disposes its request context.
test.afterEach(async ({ page }) => { await page.unrouteAll({ behavior: "wait" }); });

for (const scenario of [
  { id: "qa", question: "Quelle est la capitale de la France ?", result: "La capitale de la France est Paris.", error: false },
  { id: "json_repair", question: "Recherche Akasha audit release et ouvre sa source pour donner sa version de test.", result: "9.8.7-AUDIT", error: false },
  { id: "json_stuck", question: "Recherche Akasha audit release et donne sa version.", result: "Les deux tentatives de correction ont échoué", error: true },
]) {
  test(`chat displays the actual result: ${scenario.id}`, async ({ page, request }) => {
    await request.post(`${provider}/control`, { data: { case: scenario.id } });
    const consent = page.locator("details.remote-privacy");
    await consent.locator("summary").click();
    await consent.getByRole("checkbox", { name: /Modèle distant · openai / }).check();
    await page.getByPlaceholder("Votre message…").fill(scenario.question);
    await page.getByRole("button", { name: "Envoyer le message", exact: true }).click();
    const answer = page.locator(".message.assistant").filter({ hasText: scenario.result }).last();
    await expect(answer).toBeVisible({ timeout: 45_000 });
    await expect(answer).not.toContainText("tool_call_start");
    await expect(answer).not.toContainText('"tool_use"');
    if (scenario.error) await expect(answer).toHaveClass(/error/);
    await page.screenshot({ path: path.resolve(process.env.AUDIT_SCREENSHOTS || "test-results", `chat-${scenario.id}.png`), fullPage: true });
  });
}
