import { expect, test } from "@playwright/test";

// UI-only regression. HTTP/SQLCipher/native-credential evidence is supplied by
// the Rust suites; these responses are deliberately controlled fixtures.
for (const width of [1440, 900]) test(`remote choice and revocation at ${width}px`, async ({ page }) => {
  await page.setViewportSize({ width, height: 1000 });
  await page.addInitScript(() => {
    localStorage.clear();
    for (const [key, value] of Object.entries({ akasha_setup_wizard_done: "1", akasha_onboarding_dismissed: "1", akasha_locale: "fr", akasha_chat_composer_mode: "ask" })) localStorage.setItem(key, value);
    Object.assign(window, { __TAURI_INTERNALS__: {
      invoke: async (command: string, args: unknown) => {
        const response = await fetch("/__privacy_ipc", { method: "POST", body: JSON.stringify({ command, args }) });
        return response.json();
      }, transformCallback: () => 1, unregisterCallback: () => {},
    } });
  });
  const service = { name: "Exemple de test", endpoint: "https://model.example/v1", local: false, recipient: "model:test", data_classes: ["message", "history", "memory", "documents", "tool_results"] };
  let revoked = false;
  let sends = 0;
  const privacy = () => ({ services: [service], search_services: [], memory: { state: "encrypted", encrypted_at_rest: true }, grants: [{ ...service, scope: "task:12345678", expires_at: "2099-01-01T00:00:00Z", revoked }] });
  await page.route("**/__privacy_ipc", async (route) => {
    const { command, args = {} } = route.request().postDataJSON();
    let value: unknown = {};
    if (command === "check_health") value = { ok: true, port: 3876 };
    if (command === "daemon_get_text") value = { ok: true, status: 200, text: JSON.stringify(args.path === "/api/privacy" ? privacy() : { items: [] }) };
    if (command === "daemon_request") { expect(JSON.parse(args.body)).toEqual({ scope: "task:12345678", recipient: "model:test" }); revoked = true; value = { ok: true, status: 200, text: '{"revoked":true}' }; }
    if (command === "send_message_ack") { expect(args.remoteConsents).toEqual([{ recipient: "model:test", data_classes: service.data_classes }]); sends++; value = { ack: true, task_id: "", session_id: "", message: "Reçu" }; }
    if (["get_tasks", "get_sessions", "get_notes"].includes(command)) value = { tasks: [], sessions: [], notes: [] };
    if (command === "get_pending_human_input") value = { pending: [] };
    await route.fulfill({ json: value });
  });
  await page.goto("/");
  const details = page.locator("details.remote-privacy");
  await details.locator("summary").click();
  await details.getByRole("checkbox").check();
  await page.getByPlaceholder("Votre message…").fill("Explique cette demande synthétique");
  await page.getByRole("button", { name: "Envoyer le message", exact: true }).click();
  await expect.poll(() => sends).toBe(1);
  await expect(details.getByRole("checkbox")).not.toBeChecked();
  await page.locator(".permissions-bell-trigger").click();
  await expect(page.getByText("Mémoire long terme chiffrée.", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Révoquer", exact: true }).click();
  await expect(page.getByText("Aucune autorisation active.", { exact: true })).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  await page.screenshot({ path: `test-results/privacy-${width}.png`, fullPage: true });
});
