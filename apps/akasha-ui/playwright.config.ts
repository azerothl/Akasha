import { defineConfig, devices } from "@playwright/test";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

export default defineConfig({
  testDir: path.join(__dirname, "e2e"),
  fullyParallel: false,
  workers: 1,
  globalSetup: path.join(__dirname, "e2e/global-setup.ts"),
  globalTeardown: path.join(__dirname, "e2e/global-teardown.ts"),
  reporter: "list",
  use: {
    baseURL: "http://127.0.0.1:4173",
    trace: "on-first-retry",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: {
    command: "vite preview --host 127.0.0.1 --port 4173 --strictPort",
    cwd: __dirname,
    url: "http://127.0.0.1:4173",
    reuseExistingServer: !process.env.CI,
  },
});
