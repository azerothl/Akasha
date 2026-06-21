import { expect, test } from "@playwright/test";

test.describe("Onboarding wizard (embedded)", () => {
  test.beforeEach(async ({ page }) => {
    await page.route("**/api/router/embedded-status", async (route) => {
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          embedded_available: true,
          embedded_loaded: false,
          backend: null,
          device: null,
          compiled_backends: ["llama_cpp", "candle"],
          hint: "GGUF missing",
          llama_cpp_compiled: true,
          gguf_present: false,
          ready_for_chat: true,
          action: "embedded-download",
        }),
      });
    });
    await page.route("**/api/router/embedded/models", async (route) => {
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          default_id: "qwen2.5-1.5b-instruct-q4",
          models: [{ id: "qwen2.5-1.5b-instruct-q4", label: "Qwen2.5 1.5B Q4" }],
        }),
      });
    });
    await page.route("**/api/router/embedded/download/status", async (route) => {
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({ state: "idle", percent: 0 }),
      });
    });
    await page.addInitScript(() => {
      try {
        localStorage.removeItem("akasha_onboarding_dismissed");
        localStorage.removeItem("akasha_setup_wizard_done");
      } catch {
        /* ignore */
      }
    });
  });

  test("wizard shows embedded download when GGUF missing", async ({ page }) => {
    await page.goto("/");
    await expect(page.getByRole("dialog")).toBeVisible({ timeout: 15000 });
    await page.getByRole("button", { name: /Next|Suivant/i }).click();
    await page.getByRole("button", { name: /Next|Suivant/i }).click();
    await expect(page.getByText(/embedded-download|Télécharger|Download model/i)).toBeVisible({
      timeout: 10000,
    });
  });
});
