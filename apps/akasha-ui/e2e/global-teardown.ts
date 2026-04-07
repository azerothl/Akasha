import { spawn } from "node:child_process";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

export default async function globalTeardown(): Promise<void> {
  const pidFile = path.join(os.tmpdir(), "akasha-playwright-daemon.pid");
  if (!fs.existsSync(pidFile)) return;
  try {
    const raw = fs.readFileSync(pidFile, "utf8");
    const j = JSON.parse(raw) as { pid?: number; dataDir?: string };
    if (j.pid) {
      if (process.platform === "win32") {
        spawn("taskkill", ["/PID", String(j.pid), "/T", "/F"], {
          stdio: "ignore",
          shell: true,
        });
      } else {
        try {
          process.kill(j.pid, "SIGTERM");
        } catch {
          /* ignore */
        }
      }
    }
    if (j.dataDir && fs.existsSync(j.dataDir)) {
      try {
        fs.rmSync(j.dataDir, { recursive: true, force: true });
      } catch {
        /* ignore */
      }
    }
  } finally {
    try {
      fs.unlinkSync(pidFile);
    } catch {
      /* ignore */
    }
  }
}
