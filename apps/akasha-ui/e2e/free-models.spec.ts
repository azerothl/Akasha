import { expect, test } from "@playwright/test";

for (const width of [1280, 390]) {
  test(`free mode requires explicit consent to restore paid routes at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 900 });
    await page.addInitScript(() => {
      localStorage.setItem("akasha_setup_wizard_done", "1");
      localStorage.setItem("akasha_onboarding_dismissed", "1");
      localStorage.setItem("akasha_locale", "fr");
      localStorage.setItem("akasha_ui_mode", "expert");
      const state = {
        enabled: false, openrouter_ready: true, openrouter_fresh: true,
        catalog: {
          openrouter: { checked_at: 1780000000, models: [{ id: "example/model:free", provider: "openrouter", context_length: 32768, eligible: true }] },
          huggingface: { checked_at: 1780000000, models: [] },
        },
      };
      const writes: unknown[] = [];
      Object.assign(window, { freeModelsFixture: { state, writes }, __TAURI_INTERNALS__: {
        invoke: async (command: string, args: { action?: string; enabled?: boolean }) => {
          if (command === "check_health") return { ok: true, port: 3876 };
          if (command === "free_models_control") {
            if (args.action === "set") { writes.push(args); state.enabled = !!args.enabled; }
            return structuredClone(state);
          }
          if (command === "get_tasks") return { tasks: [] };
          if (command === "get_sessions") return { sessions: [] };
          return {};
        },
        transformCallback: () => 1, unregisterCallback: () => {},
      } });
    });
    await page.goto("/#/router");
    const panel = page.getByRole("region", { name: "Modèles gratuits", exact: true });
    await expect(panel).toBeVisible();
    const toggle = panel.getByRole("checkbox");
    await toggle.check();
    await expect(toggle).toBeChecked();
    await expect(panel).toContainText("Secours payant bloqué");
    await panel.locator("summary").filter({ hasText: "OpenRouter" }).click();
    await expect(panel).toContainText("example/model:free");
    await panel.locator("summary").filter({ hasText: "Hugging Face" }).click();
    await expect(panel).toContainText("leur utilisation automatique reste bloquée");
    await page.screenshot({ path: `test-results/free-models-${width}.png`, fullPage: true });
    page.once("dialog", dialog => dialog.dismiss());
    await toggle.click();
    await expect(toggle).toBeChecked();
    page.once("dialog", dialog => dialog.accept());
    await toggle.uncheck();
    await expect(toggle).not.toBeChecked();
    const writes = await page.evaluate(() => (window as unknown as { freeModelsFixture: { writes: { confirmPaidRoutes: boolean }[] } }).freeModelsFixture.writes);
    expect(writes).toHaveLength(2);
    expect(writes[0].confirmPaidRoutes).toBe(false);
    expect(writes[1].confirmPaidRoutes).toBe(true);
  });
}
