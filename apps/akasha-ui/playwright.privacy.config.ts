import { defineConfig } from "@playwright/test";
export default defineConfig({
  testDir: "./e2e", testMatch: ["privacy.ui.ts", "consumer-onboarding.ui.ts", "consumer-documents.ui.ts", "consumer-flows.ui.ts", "consumer-home.ui.ts", "consumer-runtime-port.ui.ts", "consumer-calendar.ui.ts"], workers: 1,
  use: { baseURL: "http://127.0.0.1:4197" },
  // These fixtures replace Tauri IPC, so build that transport explicitly even
  // after the ordinary HTTP E2E suite. Never proxy to an installed user daemon.
  webServer: { command: "npm run build -- --mode ipc-test && node scripts/e2e-preview.mjs", env: { PORT: "4197", VITE_E2E: "false", AKASHA_E2E_DAEMON_DISABLED: "1" }, url: "http://127.0.0.1:4197", reuseExistingServer: false },
});
