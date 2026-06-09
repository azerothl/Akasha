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
  test.beforeEach(async ({ page }) => {
    await page.addInitScript(() => {
      try {
        localStorage.setItem("akasha_onboarding_dismissed", "1");
      } catch {
        /* ignore */
      }
    });
  });

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
    await page.goto("/#/docs");
    await expect(page.locator(".daemon-status-ok")).toBeVisible({ timeout: 120_000 });
    await expect(page.locator('[aria-labelledby="tab-docs"]')).toBeVisible();
    await expect(page.locator(".doc-markdown")).toBeVisible({ timeout: 120_000 });
    const navCount = await page.locator(".doc-nav").count();
    if (navCount > 0) {
      await expect(page.locator(".doc-nav")).toBeVisible();
    }
    await page.screenshot({
      path: path.join(screenshotsDir(), "ui-docs.png"),
      fullPage: true,
    });
  });

  test("compare panel screenshot", async ({ page }) => {
    await page.goto("/#/compare");
    await expect(page.locator(".daemon-status-ok")).toBeVisible({ timeout: 120_000 });
    await expect(page.locator("#panel-compare")).toBeVisible({ timeout: 60_000 });
    await page.screenshot({
      path: path.join(screenshotsDir(), "ui-compare.png"),
      fullPage: true,
    });
  });

  test("cookbook models screenshot", async ({ page }) => {
    await page.goto("/#/cookbook");
    await expect(page.locator(".daemon-status-ok")).toBeVisible({ timeout: 120_000 });
    await expect(page.locator("#panel-cookbook")).toBeVisible({ timeout: 60_000 });
    await page.locator(".cookbook-panel-tabs button").first().click();
    await expect(page.locator(".cookbook-panel")).toBeVisible();
    await page.screenshot({
      path: path.join(screenshotsDir(), "ui-cookbook-models.png"),
      fullPage: true,
    });
  });

  test("cookbook recipes screenshot", async ({ page }) => {
    await page.goto("/#/cookbook");
    await expect(page.locator(".daemon-status-ok")).toBeVisible({ timeout: 120_000 });
    await expect(page.locator("#panel-cookbook")).toBeVisible({ timeout: 60_000 });
    await page.locator(".cookbook-panel-tabs button").nth(1).click();
    await expect(page.locator(".cookbook-panel")).toBeVisible();
    await page.screenshot({
      path: path.join(screenshotsDir(), "ui-cookbook-recipes.png"),
      fullPage: true,
    });
  });

  test("cookbook panel screenshot", async ({ page }) => {
    await page.goto("/#/cookbook");
    await expect(page.locator(".daemon-status-ok")).toBeVisible({ timeout: 120_000 });
    await expect(page.locator("#panel-cookbook")).toBeVisible({ timeout: 60_000 });
    await page.screenshot({
      path: path.join(screenshotsDir(), "ui-cookbook.png"),
      fullPage: true,
    });
  });

  test("research panel screenshot", async ({ page }) => {
    await page.goto("/#/research");
    await expect(page.locator(".daemon-status-ok")).toBeVisible({ timeout: 120_000 });
    await expect(page.locator("#panel-research")).toBeVisible({ timeout: 60_000 });
    await expect(page.locator("#panel-research")).not.toHaveAttribute("hidden");
    await page.screenshot({
      path: path.join(screenshotsDir(), "ui-research.png"),
      fullPage: true,
    });
  });

  test("mission panel screenshot", async ({ page }) => {
    await page.goto("/#/mission");
    await expect(page.locator(".daemon-status-ok")).toBeVisible({ timeout: 120_000 });
    await expect(page.locator('[aria-labelledby="tab-mission"]')).toBeVisible({ timeout: 60_000 });
    await page.screenshot({
      path: path.join(screenshotsDir(), "ui-mission.png"),
      fullPage: true,
    });
  });

  test("notes panel screenshot", async ({ page }) => {
    await page.goto("/#/notes");
    await expect(page.locator(".daemon-status-ok")).toBeVisible({ timeout: 120_000 });
    await expect(page.locator("#panel-notes")).toBeVisible({ timeout: 60_000 });
    await page.screenshot({
      path: path.join(screenshotsDir(), "ui-notes.png"),
      fullPage: true,
    });
  });
});
