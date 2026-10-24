import { defineConfig, devices } from "@playwright/test";
export default defineConfig({
  testDir: "./e2e", testMatch: "free-models.spec.ts", workers: 1, timeout: 30_000,
  use: { baseURL: "http://127.0.0.1:4176" },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: { command: "npm run preview -- --host 127.0.0.1 --port 4176", url: "http://127.0.0.1:4176", reuseExistingServer: false },
});
