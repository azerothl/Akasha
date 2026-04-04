import { spawn } from "node:child_process";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

function workspaceRoot(): string {
  return path.join(__dirname, "..", "..", "..");
}

function daemonExe(): string {
  const root = workspaceRoot();
  const base = path.join(root, "target", "debug", "akasha-daemon");
  return process.platform === "win32" ? `${base}.exe` : base;
}

async function waitForHttpOk(url: string, attempts = 60): Promise<void> {
  for (let i = 0; i < attempts; i++) {
    try {
      const r = await fetch(url);
      if (r.ok) return;
    } catch {
      /* retry */
    }
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error(`Timeout waiting for ${url}`);
}

export default async function globalSetup(): Promise<void> {
  const exe = daemonExe();
  if (!fs.existsSync(exe)) {
    throw new Error(`Daemon binary not found: ${exe} (run cargo build -p akasha-daemon first)`);
  }
  const dataDir = fs.mkdtempSync(path.join(os.tmpdir(), "akasha-e2e-"));
  const pidFile = path.join(os.tmpdir(), "akasha-playwright-daemon.pid");

  const child = spawn(exe, [], {
    cwd: workspaceRoot(),
    env: {
      ...process.env,
      AKASHA_DATA_DIR: dataDir,
      AKASHA_PORT: "3876",
    },
    stdio: "ignore",
    detached: false,
  });

  if (!child.pid) {
    throw new Error("Failed to spawn akasha-daemon");
  }

  fs.writeFileSync(
    pidFile,
    JSON.stringify({ pid: child.pid, dataDir }, null, 2),
    "utf8",
  );

  await waitForHttpOk("http://127.0.0.1:3876/");
}
