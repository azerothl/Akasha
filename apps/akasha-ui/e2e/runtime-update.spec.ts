import { expect, test, type Page } from "@playwright/test";

async function setup(page: Page, initialState = "ready") {
  await page.addInitScript((state) => {
    localStorage.setItem("akasha_setup_wizard_done", "1");
    localStorage.setItem("akasha_onboarding_dismissed", "1");
    localStorage.setItem("akasha_locale", "fr");
    const fixture = { state, installCalls: 0 };
    Object.assign(window, {
      __runtimeUpdateFixture: fixture,
      __TAURI_INTERNALS__: {
        invoke: async (command: string, args?: { confirmed?: boolean }) => {
          if (command === "get_daemon_launch_config") return { port: 3876 };
          if (command === "get_app_version") return "1.0.0";
          if (command === "get_update_status") return {
            remote_version: "1.0.0", runtime_update: {
              supported: true, installed_version: "1.0.0", remote_version: "1.1.0", state: fixture.state,
            },
          };
          if (command === "install_runtime_update") {
            if (args?.confirmed !== true) throw new Error("Missing confirmation");
            fixture.installCalls++;
            fixture.state = "applying";
            return "Scheduled";
          }
          throw new Error(`Unmocked IPC: ${command}`);
        },
      },
    });
  }, initialState);
  await page.route("**/__e2e_daemon/**", async (route) => {
    const path = new URL(route.request().url()).pathname;
    const json = path.endsWith("/api/tasks") ? { tasks: [] }
      : path.endsWith("/api/notes") ? { notes: [] } : { status: "ok" };
    await route.fulfill({ json });
  });
  await page.goto("/");
}

test("runtime update requires consent and prevents duplicate installs", async ({ page }) => {
  await setup(page);
  const button = page.getByRole("button", { name: "Installer et redémarrer le moteur" });
  await expect(button).toBeEnabled();
  page.once("dialog", (dialog) => dialog.dismiss());
  await button.click();
  expect(await page.evaluate(() => (window as unknown as { __runtimeUpdateFixture: { installCalls: number } }).__runtimeUpdateFixture.installCalls)).toBe(0);
  page.once("dialog", (dialog) => dialog.accept());
  await button.click();
  await expect(button).toBeDisabled();
  expect(await page.evaluate(() => (window as unknown as { __runtimeUpdateFixture: { installCalls: number } }).__runtimeUpdateFixture.installCalls)).toBe(1);
});

test("background downloads cannot be installed until ready", async ({ page }) => {
  await setup(page, "downloading");
  const button = page.getByRole("button", { name: "Installer et redémarrer le moteur" });
  await expect(button).toBeDisabled();
  await expect(page.getByText("Téléchargement des fichiers modifiés en arrière-plan…")).toBeVisible();
  await page.evaluate(() => { (window as unknown as { __runtimeUpdateFixture: { state: string } }).__runtimeUpdateFixture.state = "ready"; });
  await expect(button).toBeEnabled({ timeout: 10000 });
});
