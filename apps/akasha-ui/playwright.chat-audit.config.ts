import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  testDir: "./e2e",
  testMatch: "chat-agent-results.spec.ts",
  outputDir: "../../docs/validation/chat-agent-audit-2026-10-26/browser-results",
  workers: 1,
  timeout: 60_000,
  reporter: "list",
  use: { baseURL: "http://127.0.0.1:4176", viewport: { width: 1440, height: 1000 } },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: {
    command: "npm run preview -- --host 127.0.0.1 --port 4176",
    url: "http://127.0.0.1:4176",
    reuseExistingServer: false,
  },
});
