import { defineConfig, devices } from "@playwright/test";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

export default defineConfig({
  testDir: path.join(__dirname, "e2e"),
  fullyParallel: false,
  workers: 1,
  timeout: 180_000,
  globalSetup: path.join(__dirname, "e2e/global-setup.ts"),
  globalTeardown: path.join(__dirname, "e2e/global-teardown.ts"),
  reporter: "list",
  use: {
    baseURL: "http://127.0.0.1:4173",
    trace: "on-first-retry",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: {
    /** Custom server: proxies /__e2e_daemon before SPA (vite preview was serving index.html for that path). */
    command: "node scripts/e2e-preview.mjs",
    cwd: __dirname,
    env: {
      ...process.env,
      HOST: "127.0.0.1",
      PORT: "4173",
    },
    url: "http://127.0.0.1:4173",
    reuseExistingServer: false,
  },
});
