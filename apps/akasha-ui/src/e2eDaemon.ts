/** Browser Playwright E2E: HTTP to the daemon via the preview proxy (no Tauri IPC). */

export const E2E_WEB =
  import.meta.env.VITE_E2E === "true" || import.meta.env.MODE === "e2e";

/** Same-origin path proxied by `scripts/e2e-preview.mjs` when mode is e2e. */
export function e2eDaemonHttpUrl(path: string): string {
  const p = path.startsWith("/") ? path : `/${path}`;
  if (E2E_WEB) return `/__e2e_daemon${p}`;
  return `http://127.0.0.1:3876${p}`;
}

export async function e2eDaemonGetJson<T>(path: string): Promise<T> {
  const res = await fetch(e2eDaemonHttpUrl(path));
  if (!res.ok) {
    throw new Error(`e2e daemon GET ${path}: HTTP ${res.status}`);
  }
  return (await res.json()) as T;
}
