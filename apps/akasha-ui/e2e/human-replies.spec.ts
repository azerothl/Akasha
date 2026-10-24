import { expect, test } from "@playwright/test";

test.beforeEach(async ({ page }) => {
  await page.clock.install();
  await page.addInitScript(() => {
    localStorage.setItem("akasha_setup_wizard_done", "1");
    localStorage.setItem("akasha_onboarding_dismissed", "1");
    localStorage.setItem("akasha_locale", "fr");
    const state = {
      pending: [{ task_id: "task-one", request_id: "first", question: "Continuer la tâche ?", context: "", choices: ["Continuer", "Arrêter"] }],
      replies: [] as unknown[],
      failure: "",
    };
    Object.assign(window, { humanReplyFixture: state, __TAURI_INTERNALS__: {
      invoke: async (command: string, args: Record<string, unknown>) => {
        if (command === "check_health") return { ok: true, port: 3876 };
        if (command === "get_pending_human_input") return { pending: state.pending.map(p => ({ ...p })) };
        if (command === "post_task_human_reply") {
          state.replies.push(args);
          if (state.failure === "expired") { state.pending = []; throw "404 Not Found"; }
          if (state.failure === "changed") {
            state.pending[0].request_id = "second";
            state.pending[0].question = "Nouvelle décision requise";
            state.failure = "";
            throw "409 Conflict: stale_approval_request";
          }
          state.pending = [];
          return { ok: true };
        }
        if (command === "get_tasks") return { tasks: [] };
        if (command === "get_notes") return { notes: [] };
        if (command === "get_sessions") return { sessions: [] };
        return {};
      },
      transformCallback: () => 1,
      unregisterCallback: () => {},
    } });
  });
  await page.route("**/__e2e_daemon/**", route => route.fulfill({ json: { status: "ok", tasks: [] } }));
  await page.goto("/");
  await page.locator(".global-human-input-banner button").click();
  await expect(page.getByRole("dialog", { name: "Action requise" })).toBeVisible();
});

test("expired question closes the dialog and explains the failure", async ({ page }) => {
  await page.evaluate(() => { (window as any).humanReplyFixture.failure = "expired"; });
  await page.getByRole("dialog").getByRole("button", { name: "Continuer", exact: true }).click();
  await expect(page.getByRole("dialog", { name: "Action requise" })).toHaveCount(0);
  await expect(page.getByRole("alert")).toContainText("Cette question a expiré");
  await expect(page.locator(".global-human-input-banner")).toHaveCount(0);
});

test("changed question requires a fresh click and submits its new identity", async ({ page }) => {
  await page.evaluate(() => { (window as any).humanReplyFixture.failure = "changed"; });
  const dialog = page.getByRole("dialog", { name: "Action requise" });
  await dialog.getByRole("button", { name: "Continuer", exact: true }).click();
  await expect(dialog.getByRole("alert")).toContainText("La question a changé");
  await expect(dialog).toContainText("Nouvelle décision requise");
  expect(await page.evaluate(() => (window as any).humanReplyFixture.replies.length)).toBe(1);
  await dialog.getByRole("button", { name: "Continuer", exact: true }).click();
  await expect(dialog).toHaveCount(0);
  expect(await page.evaluate(() => (window as any).humanReplyFixture.replies[1].requestId)).toBe("second");
});

test("an empty server snapshot removes a stale question without a reply", async ({ page }) => {
  await page.evaluate(() => { (window as any).humanReplyFixture.pending = []; });
  await page.clock.fastForward(26000);
  await expect(page.getByRole("dialog", { name: "Action requise" })).toHaveCount(0);
  expect(await page.evaluate(() => (window as any).humanReplyFixture.replies.length)).toBe(0);
});
