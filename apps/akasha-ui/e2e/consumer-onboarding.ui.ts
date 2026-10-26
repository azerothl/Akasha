import { expect, test } from "@playwright/test";
for (const width of [1440, 900]) test(`three-step consumer startup at ${width}px`, async ({ page }) => {
  await page.setViewportSize({ width, height: 1000 });
  await page.addInitScript(() => {
    localStorage.clear(); localStorage.setItem("akasha_locale", "fr");
    Object.assign(window, { __TAURI_INTERNALS__: {
      invoke: async (command: string, args: unknown) => (await fetch("/__consumer_ipc", { method: "POST", body: JSON.stringify({ command, args }) })).json(),
      transformCallback: () => 1, unregisterCallback: () => {},
    } });
  });
  await page.route("**/__consumer_ipc", async (route) => {
    const { command, args = {} } = route.request().postDataJSON();
    let value: unknown = {};
    if (command === "check_health") value = { ok: true, port: 3876 };
    if (command === "daemon_get_text" || command === "daemon_request") {
      const data = args.path === "/api/privacy" ? { consumer_beta_build: true, services: [], search_services: [], grants: [], memory: { state: "encrypted", encrypted_at_rest: true } }
        : args.path === "/api/router/ollama/models" ? { models: ["qwen3.5:9b"], local_models: ["qwen3.5:9b"] }
        : args.path === "/api/router/embedded/models" ? { models: [] }
        : args.path === "/api/message" ? { task_id: "demo" }
        : args.path === "/api/tasks/demo" ? { status: "completed", outcome: { response: '{"montant":42,"echeance":"2026-11-10","source":"demo-facture.txt"}' } }
        : args.path === "/api/router/embedded/hardware" ? { profile: { ram_gb: 32, gpu_name: "Matériel de test" } } : {};
      if (args.path === "/api/message") { const body = JSON.parse(args.body); expect(body.composer_mode).toBe("ask"); expect(body.attachments[0].name).toBe("demo-facture.txt"); }
      value = { ok: true, status: 200, text: JSON.stringify(data) };
    }
    if (["get_tasks", "get_sessions", "get_notes"].includes(command)) value = { tasks: [], sessions: [], notes: [] };
    if (command === "get_pending_human_input") value = { pending: [] };
    await route.fulfill({ json: value });
  });
  await page.goto("/");
  const dialog = page.getByRole("dialog", { name: "Préparer Akasha" });
  await expect(dialog).toBeVisible();
  await expect(dialog.locator("ol li")).toHaveCount(3);
  await dialog.getByRole("button", { name: "Préparer ce modèle" }).click();
  await dialog.getByRole("button", { name: "Continuer", exact: true }).click();
  const privacy = page.getByRole("dialog", { name: "Choisir sa confidentialité" });
  await privacy.getByRole("checkbox").check();
  await privacy.getByRole("button", { name: "Continuer", exact: true }).click();
  const demo = page.getByRole("dialog", { name: "Réussir une première tâche" });
  await demo.getByRole("button", { name: "Comprendre ce document" }).click();
  await expect(demo.getByText(/Vérifié dans la source/)).toBeVisible();
  await expect(demo.getByRole("button", { name: "Commencer avec Akasha" })).toBeEnabled();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await page.screenshot({ path: `test-results/consumer-onboarding-${width}.png`, fullPage: true });
});
