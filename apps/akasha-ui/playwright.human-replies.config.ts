import { defineConfig, devices } from "@playwright/test";

/** IPC is mocked by the test; this suite never starts or changes a real daemon. */
export default defineConfig({
  testDir: "./e2e",
  testMatch: "human-replies.spec.ts",
  workers: 1,
  timeout: 30_000,
  use: { baseURL: "http://127.0.0.1:4175" },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: {
    command: "npm run preview -- --host 127.0.0.1 --port 4175",
    url: "http://127.0.0.1:4175",
    reuseExistingServer: false,
  },
});
