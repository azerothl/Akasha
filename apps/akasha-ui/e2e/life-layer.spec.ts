import { expect, test } from "@playwright/test";

test.describe("Life layer P7", () => {
  test.beforeEach(async ({ page }) => {
    await page.route("**/api/life/packs", async (route) => {
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          packs: [
            {
              id: "morning_brief",
              enabled: false,
              schedule_id: null,
              hour_local: 8,
              minute_local: 0,
              timezone: "UTC",
              notify_channel: "telegram",
            },
            {
              id: "overnight_pack",
              enabled: false,
              schedule_id: null,
              hour_local: 2,
              minute_local: 0,
              timezone: "UTC",
              notify_channel: null,
            },
          ],
        }),
      });
    });
    await page.route("**/api/life/morning-brief", async (route) => {
      await route.fulfill({
        status: 201,
        contentType: "application/json",
        body: JSON.stringify({
          ok: true,
          pack: {
            id: "morning_brief",
            enabled: true,
            schedule_id: "00000000-0000-0000-0000-000000000001",
            hour_local: 8,
            minute_local: 0,
            timezone: "UTC",
            notify_channel: "telegram",
          },
        }),
      });
    });
    await page.route("**/api/schedules/from-nl", async (route) => {
      const post = route.request().postDataJSON() as { commit?: boolean };
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          preview: {
            name: "morning_brief",
            description: "Brief matinal",
            rrule: "FREQ=DAILY;BYHOUR=7;BYMINUTE=30;BYSECOND=0",
            timezone: "UTC",
            hour_local: 7,
            minute_local: 30,
            tag: "morning_brief",
            notify_channel: "telegram",
            message: "brief",
            confidence: 0.85,
          },
          committed: Boolean(post?.commit),
          schedule_id: post?.commit ? "00000000-0000-0000-0000-000000000002" : undefined,
        }),
      });
    });
    await page.route("**/api/schedules", async (route) => {
      if (route.request().method() === "GET") {
        await route.fulfill({
          status: 200,
          contentType: "application/json",
          body: JSON.stringify({ schedules: [] }),
        });
        return;
      }
      await route.continue();
    });
    await page.addInitScript(() => {
      try {
        localStorage.setItem("akasha_setup_wizard_done", "1");
        localStorage.setItem("akasha_onboarding_dismissed", "1");
      } catch {
        /* ignore */
      }
    });
  });

  test("calendar schedules shows Life layer packs", async ({ page }) => {
    await page.goto("/");
    await page.getByRole("tab", { name: /Calendar|Calendrier/i }).click();
    await page.getByRole("tab", { name: /Tâches récurrentes|Recurring/i }).click();
    await expect(page.getByText(/Life layer/i)).toBeVisible({ timeout: 15000 });
    await expect(page.getByText(/Brief matinal|Morning brief/i)).toBeVisible();
    await expect(page.getByText(/Pack nuit|Overnight pack/i)).toBeVisible();
  });
});

