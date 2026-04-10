#!/usr/bin/env node
/**
 * Playwright E2E static server: serves `dist/` like `vite preview`, but proxies
 * `/__e2e_daemon/*` to the daemon before any SPA fallback (so the UI gets JSON, not index.html).
 */
import http from "node:http";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const dist = path.resolve(__dirname, "..", "dist");
const DAEMON_HOST = "127.0.0.1";
const DAEMON_PORT = 3876;
const HOST = process.env.HOST ?? "127.0.0.1";
const PORT = Number(process.env.PORT ?? 4173);

const MIME = new Map([
  [".html", "text/html; charset=utf-8"],
  [".js", "text/javascript; charset=utf-8"],
  [".mjs", "text/javascript; charset=utf-8"],
  [".css", "text/css; charset=utf-8"],
  [".json", "application/json; charset=utf-8"],
  [".png", "image/png"],
  [".jpg", "image/jpeg"],
  [".jpeg", "image/jpeg"],
  [".svg", "image/svg+xml"],
  [".ico", "image/x-icon"],
  [".woff2", "font/woff2"],
  [".webp", "image/webp"],
  [".map", "application/json"],
]);

function mimeFor(file) {
  return MIME.get(path.extname(file).toLowerCase()) ?? "application/octet-stream";
}

function proxyToDaemon(req, res) {
  const u = new URL(req.url ?? "/", `http://${HOST}`);
  const daemonPath = (u.pathname.replace(/^\/__e2e_daemon/, "") || "/") + u.search;
  const headers = { ...req.headers };
  delete headers.host;
  delete headers.connection;

  const proxyReq = http.request(
    {
      hostname: DAEMON_HOST,
      port: DAEMON_PORT,
      path: daemonPath,
      method: req.method,
      headers,
    },
    (proxyRes) => {
      res.writeHead(proxyRes.statusCode ?? 502, proxyRes.headers);
      proxyRes.pipe(res);
    },
  );
  proxyReq.on("error", (err) => {
    if (!res.headersSent) {
      res.writeHead(502, { "content-type": "text/plain; charset=utf-8" });
      res.end(`e2e daemon proxy: ${err.message}`);
    }
  });
  req.pipe(proxyReq);
}

function serveDist(req, res) {
  const u = new URL(req.url ?? "/", `http://${HOST}`);
  let rel = u.pathname === "/" ? "index.html" : u.pathname.slice(1);
  if (!rel || rel.includes("..")) {
    res.writeHead(403, { "content-type": "text/plain; charset=utf-8" });
    res.end("Forbidden");
    return;
  }

  const resolvedDist = path.resolve(dist);
  const full = path.resolve(resolvedDist, rel);
  const relToDist = path.relative(resolvedDist, full);
  if (relToDist.startsWith("..") || path.isAbsolute(relToDist)) {
    res.writeHead(403, { "content-type": "text/plain; charset=utf-8" });
    res.end("Forbidden");
    return;
  }

  fs.readFile(full, (err, data) => {
    if (!err) {
      res.writeHead(200, { "content-type": mimeFor(full) });
      res.end(data);
      return;
    }
    const indexHtml = path.join(dist, "index.html");
    fs.readFile(indexHtml, (err2, html) => {
      if (err2) {
        res.writeHead(404, { "content-type": "text/plain; charset=utf-8" });
        res.end("Not found");
        return;
      }
      res.writeHead(200, { "content-type": "text/html; charset=utf-8" });
      res.end(html);
    });
  });
}

if (!fs.existsSync(path.join(dist, "index.html"))) {
  console.error(`[e2e-preview] missing ${path.join(dist, "index.html")} — run: vite build --mode e2e`);
  process.exit(1);
}

http
  .createServer((req, res) => {
    const url = req.url ?? "/";
    if (url.startsWith("/__e2e_daemon")) {
      proxyToDaemon(req, res);
      return;
    }
    serveDist(req, res);
  })
  .listen(PORT, HOST, () => {
    console.error(`[e2e-preview] http://${HOST}:${PORT}/ dist=${dist}`);
  });
