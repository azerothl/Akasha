#!/usr/bin/env node
/**
 * Akasha browser automation runner (spec 39).
 * Reads JSON commands from stdin (one per line), executes via Playwright, writes JSON result to stdout.
 * Commands: init, navigate, snapshot, close.
 * Usage: node run.mjs [--headless]  (default headless=true)
 */

import { chromium } from 'playwright';
import { createInterface } from 'readline';
import { Writable } from 'node:stream';

/** Readline must not use process.stdout here — it would mix with JSON lines from send(). */
const readlineSink = new Writable({
  write(_chunk, _encoding, callback) {
    callback();
  },
});

const args = process.argv.slice(2);
const headless = !args.includes('--headed');

let browser = null;
let context = null;
let page = null;

function send(obj) {
  console.log(JSON.stringify(obj));
}

async function handleInit(params = {}) {
  try {
    if (browser) {
      await browser.close();
        browser = null;
      context = null;
      page = null;
    }
    const headlessMode = params.headless !== false;
    browser = await chromium.launch({ headless: headlessMode });
    context = await browser.newContext({
      userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36',
      viewport: { width: 1280, height: 720 },
      ignoreHTTPSErrors: true,
    });
    page = await context.newPage();
    page.setDefaultTimeout((params.action_timeout_secs || 30) * 1000);
    send({ ok: true, result: 'initialized' });
  } catch (e) {
    send({ ok: false, error: e.message || String(e) });
  }
}

async function handleNavigate(params) {
  if (!page) {
    send({ ok: false, error: 'Browser not initialized; send init first.' });
    return;
  }
  try {
    const url = params.url || '';
    if (!url.startsWith('http://') && !url.startsWith('https://')) {
      send({ ok: false, error: 'Only http and https URLs are allowed.' });
      return;
    }
    const response = await page.goto(url, { waitUntil: 'domcontentloaded', timeout: (params.timeout_secs || 30) * 1000 });
    const title = await page.title();
    const status = response ? response.status() : 0;
    send({ ok: true, result: { title, status: status, url: page.url() } });
  } catch (e) {
    send({ ok: false, error: e.message || String(e) });
  }
}

async function handleSnapshot() {
  if (!page) {
    send({ ok: false, error: 'Browser not initialized; send init first.' });
    return;
  }
  try {
    const text = await page.evaluate(() => {
      const body = document.body;
      if (!body) return '';
      const getText = (el) => {
        if (el.nodeType === Node.TEXT_NODE) return el.textContent || '';
        if (el.nodeType !== Node.ELEMENT_NODE) return '';
        const tag = el.tagName.toLowerCase();
        if (tag === 'script' || tag === 'style') return '';
        let out = '';
        for (const child of el.childNodes) out += getText(child);
        if (tag === 'p' || tag === 'div' || tag === 'br' || tag === 'li' || tag === 'h1' || tag === 'h2' || tag === 'h3' || tag === 'tr') out += '\n';
        if (tag === 'a' && el.href) out += ` [${el.href}]`;
        return out;
      };
      return getText(body).replace(/\n{3,}/g, '\n\n').trim();
    });
    const links = await page.evaluate(() => {
      const as = Array.from(document.querySelectorAll('a[href]'));
      return as.slice(0, 100).map(a => ({ href: a.href, text: (a.textContent || '').trim().slice(0, 80) }));
    });
    send({ ok: true, result: { text: text.slice(0, 100000), links } });
  } catch (e) {
    send({ ok: false, error: e.message || String(e) });
  }
}

async function handleClose() {
  try {
    if (browser) {
      await browser.close();
      browser = null;
      context = null;
      page = null;
    }
    send({ ok: true, result: 'closed' });
  } catch (e) {
    send({ ok: false, error: e.message || String(e) });
  }
}

async function dispatch(line) {
  let cmd;
  try {
    cmd = JSON.parse(line);
  } catch {
    send({ ok: false, error: 'Invalid JSON' });
    return;
  }
  const action = cmd.cmd || cmd.action;
  const params = cmd.params || cmd;
  switch (action) {
    case 'init':
      await handleInit(params);
      break;
    case 'navigate':
      await handleNavigate(params);
      break;
    case 'snapshot':
      await handleSnapshot();
      break;
    case 'close':
      await handleClose();
      break;
    default:
      send({ ok: false, error: `Unknown command: ${action}` });
  }
}

async function main() {
  const rl = createInterface({ input: process.stdin, output: readlineSink, terminal: false });
  for await (const line of rl) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    await dispatch(trimmed);
  }
  await handleClose();
  process.exit(0);
}

main().catch((e) => {
  send({ ok: false, error: e.message || String(e) });
  process.exit(1);
});
