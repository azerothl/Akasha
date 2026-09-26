import { expect, test } from "@playwright/test";

test.describe("Cockpit agentic (P6)", () => {
  test.beforeEach(async ({ page }) => {
    await page.addInitScript(() => {
      try {
        localStorage.setItem("akasha_onboarding_dismissed", "1");
        localStorage.setItem("akasha_setup_wizard_done", "1");
      } catch {
        /* ignore */
      }
    });
    await page.route("**/api/tasks", async (route) => {
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          tasks: [
            {
              id: "11111111-1111-1111-1111-111111111111",
              status: "running",
              label: "Active demo task",
              session_id: "sess-demo",
            },
          ],
        }),
      });
    });
    await page.route("**/api/router/metrics**", async (route) => {
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          "ollama/qwen": {
            total_requests: 3,
            successful_requests: 3,
            failed_requests: 0,
            total_tokens: 1200,
            total_cost_usd: 0,
          },
        }),
      });
    });
  });

  test("composer modes and active work drawer", async ({ page }) => {
    await page.goto("/");
    await expect(page.getByRole("toolbar", { name: /Chat options|Options de chat/i })).toBeVisible({
      timeout: 30000,
    });
    await page.getByRole("button", { name: /^Ask$/i }).click();
    await expect(page.getByRole("button", { name: /^Ask$/i })).toHaveAttribute("aria-pressed", "true");
    await page.getByRole("button", { name: /Architect|Architecte/i }).click();
    await expect(page.getByRole("button", { name: /Architect|Architecte/i })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    await expect(page.getByRole("region", { name: /Active work|Travail actif/i })).toBeVisible({
      timeout: 15000,
    });
    await expect(page.getByText(/Active demo task/i)).toBeVisible();
  });

  test("subagent threads inspect and cancel-all (P6-B4)", async ({ page }) => {
    await page.unroute("**/api/tasks");
    await page.route("**/api/tasks", async (route) => {
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          tasks: [
            {
              id: "11111111-1111-1111-1111-111111111111",
              status: "running",
              label: "Parent orchestrator",
              session_id: "sess-demo",
              assigned_agent: "conversation",
            },
            {
              id: "22222222-2222-2222-2222-222222222222",
              status: "running",
              label: "Child researcher",
              parent_task_id: "11111111-1111-1111-1111-111111111111",
              assigned_agent: "researcher",
            },
            {
              id: "33333333-3333-3333-3333-333333333333",
              status: "running",
              label: "Child coder",
              parent_task_id: "11111111-1111-1111-1111-111111111111",
              assigned_agent: "coder",
            },
          ],
        }),
      });
    });
    let cancelTreeHit = false;
    await page.route("**/api/tasks/*/cancel-tree", async (route) => {
      cancelTreeHit = true;
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          cancelled: true,
          root_task_id: "11111111-1111-1111-1111-111111111111",
          cancelled_ids: [
            "11111111-1111-1111-1111-111111111111",
            "22222222-2222-2222-2222-222222222222",
            "33333333-3333-3333-3333-333333333333",
          ],
          skipped: [],
        }),
      });
    });
    await page.goto("/");
    await expect(page.getByRole("region", { name: /Active work|Travail actif/i })).toBeVisible({
      timeout: 15000,
    });
    await expect(page.getByText(/Parent orchestrator/i)).toBeVisible();
    await page.getByRole("button", { name: /Inspect|Inspecter/i }).click();
    await expect(page.getByRole("region", { name: /Subagent threads|Threads sous-agents/i })).toBeVisible();
    await expect(page.getByText(/Child researcher/i)).toBeVisible();
    await page.getByRole("button", { name: /Cancel all|Tout annuler/i }).click();
    expect(cancelTreeHit).toBe(true);
  });

  test("usage settings tab", async ({ page }) => {
    await page.goto("/");
    await page.getByRole("tab", { name: /Settings|Paramètres/i }).click();
    await page.getByRole("tab", { name: /System|Système/i }).click();
    await page.getByRole("tab", { name: /^Usage$/i }).click();
    await expect(page.getByText(/LLM usage|Usage LLM/i)).toBeVisible({ timeout: 10000 });
  });
});
