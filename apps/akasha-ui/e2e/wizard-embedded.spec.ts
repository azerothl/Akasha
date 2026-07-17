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
          calibration_done: false,
          hardware_tier: "gpu_low_4gb",
        }),
      });
    });
    await page.route("**/api/router/embedded/hardware", async (route) => {
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          profile: { tier_id: "gpu_low_4gb", ram_gb: 32, vram_mb: 4096, gpu_name: "RTX 3050 Ti" },
          models_for_tier: ["qwen3.5-0.8b-q4", "smollm2-360m-instruct-q4"],
          static_candidates: [{ model_id: "qwen3.5-0.8b-q4", n_gpu_layers: 0, engine: "llama_cpp_cpu" }],
        }),
      });
    });
    await page.route("**/api/router/embedded/models", async (route) => {
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          default_id: "qwen2.5-1.5b-instruct-q4",
          models: [
            { id: "qwen3.5-0.8b-q4", label: "Qwen3.5 0.8B Q4" },
            { id: "qwen2.5-1.5b-instruct-q4", label: "Qwen2.5 1.5B Q4" },
          ],
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
    await page.route("**/api/router/embedded/calibrate/status", async (route) => {
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

  test("wizard shows calibration step when GGUF present but not calibrated", async ({ page }) => {
    await page.route("**/api/router/embedded-status", async (route) => {
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          embedded_available: true,
          embedded_loaded: false,
          backend: null,
          device: null,
          gguf_present: true,
          llama_cpp_compiled: true,
          ready_for_chat: true,
          calibration_done: false,
          action: "embedded-calibrate",
          compiled_backends: ["llama_cpp"],
          hint: "calibration required",
          hardware_tier: "gpu_low_4gb",
        }),
      });
    });
    await page.goto("/");
    await expect(page.getByRole("dialog")).toBeVisible({ timeout: 15000 });
    for (let i = 0; i < 3; i++) {
      await page.getByRole("button", { name: /Next|Suivant/i }).click();
    }
    await expect(page.getByText(/calibrat|Calibration/i)).toBeVisible({ timeout: 10000 });
    await expect(page.getByRole("button", { name: /Run calibration|Lancer la calibration/i })).toBeVisible();
  });
});
