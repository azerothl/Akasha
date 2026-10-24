import { defineConfig, devices } from "@playwright/test";

// Fully mocked daemon and IPC: exercises update consent without starting/installing Akasha.
export default defineConfig({
  testDir: "./e2e",
  testMatch: "runtime-update.spec.ts",
  workers: 1,
  use: { baseURL: "http://127.0.0.1:1420", ...devices["Desktop Chrome"] },
  webServer: {
    command: "node node_modules/vite/bin/vite.js --mode e2e --host 127.0.0.1",
    url: "http://127.0.0.1:1420",
    reuseExistingServer: false,
  },
});
