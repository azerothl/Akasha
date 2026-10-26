import { defineConfig } from "@playwright/test";
export default defineConfig({
  testDir: "./e2e", testMatch: ["privacy.ui.ts", "consumer-onboarding.ui.ts", "consumer-documents.ui.ts", "consumer-flows.ui.ts", "consumer-home.ui.ts", "consumer-runtime-port.ui.ts"], workers: 1,
  use: { baseURL: "http://127.0.0.1:4197" },
  webServer: { command: "node scripts/e2e-preview.mjs", env: { PORT: "4197" }, url: "http://127.0.0.1:4197", reuseExistingServer: false },
});
