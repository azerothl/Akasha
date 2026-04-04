import { expect, test } from "@playwright/test";
import * as fs from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

function workspaceRoot(): string {
  return path.join(__dirname, "..", "..", "..");
}

function screenshotsDir(): string {
  const dir = path.join(workspaceRoot(), "docs", "screenshots");
  fs.mkdirSync(dir, { recursive: true });
  return dir;
}

test.describe("Akasha UI (E2E build)", () => {
  test("daemon health and chat shell", async ({ page }) => {
    await page.goto("/");
    await expect(page.locator(".daemon-status-ok")).toBeVisible({ timeout: 120_000 });
    await expect(page.locator(".status-dot.connected")).toBeVisible();
    await page.screenshot({
      path: path.join(screenshotsDir(), "ui-chat.png"),
      fullPage: true,
    });
  });

  test("documentation tab loads markdown from daemon", async ({ page }) => {
    await page.goto("/");
    await expect(page.locator(".daemon-status-ok")).toBeVisible({ timeout: 120_000 });
    await page.locator("#tab-docs").click();
    await expect(page.locator('[aria-labelledby="tab-docs"]')).toBeVisible();
    await expect(page.locator(".doc-markdown")).toBeVisible({ timeout: 60_000 });
    await page.screenshot({
      path: path.join(screenshotsDir(), "ui-docs.png"),
      fullPage: true,
    });
  });
});
