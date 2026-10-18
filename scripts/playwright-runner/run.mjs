#!/usr/bin/env node
/**
 * Akasha browser automation runner (spec 39).
 * Reads JSON commands from stdin (one per line), executes via Playwright, writes JSON result to stdout.
 * Commands: init, navigate, snapshot, click, fill, wait, screenshot, close.
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
let allowedDomains = [];
let blockedDomains = [];
let consoleLog = [];
let networkLog = [];
const sockets = new Set();

function appendLog(buffer, record) {
  buffer.push({ at: new Date().toISOString(), ...record });
  if (buffer.length > 200) buffer.splice(0, buffer.length - 200);
}

function scrubText(text) {
  return String(text).replace(/((?:api[_-]?key|token|password|secret|authorization|bearer)\s*[:=]\s*)[^\s,;]+/gi, '$1[redacted]').slice(0, 2000);
}

function scrubUrl(raw) {
  try {
    const url = new URL(raw);
    url.username = ''; url.password = '';
    for (const key of url.searchParams.keys()) if (/token|key|secret|password|auth|signature/i.test(key)) url.searchParams.set(key, '[redacted]');
    return url.toString().slice(0, 1024);
  } catch { return String(raw).slice(0, 1024); }
}

function domainAllowed(raw) {
  try {
    const url = new URL(raw);
    if (['about:', 'data:', 'blob:'].includes(url.protocol)) return true;
    if (!['http:', 'https:', 'ws:', 'wss:'].includes(url.protocol)) return false;
    const host = url.hostname.toLowerCase();
    const match = rule => rule === '*' || host === rule || host.endsWith(`.${rule}`);
    return !blockedDomains.some(match) && allowedDomains.some(match);
  } catch { return false; }
}

async function configurePolicy(params = {}) {
  if (!context) throw new Error('Browser not initialized');
  if (!Array.isArray(params.allowed_domains) || !Array.isArray(params.blocked_domains)) throw new Error('Domain policy arrays required');
  allowedDomains = params.allowed_domains.map(value => String(value).trim().toLowerCase());
  blockedDomains = params.blocked_domains.map(value => String(value).trim().toLowerCase());
  for (const socket of sockets) if (!domainAllowed(socket.url())) { await socket.close({ code: 1008, reason: 'Domain policy changed' }).catch(() => {}); sockets.delete(socket); }
  if (page && !domainAllowed(page.url())) {
    await page.goto('about:blank');
    throw new Error('Current page is no longer authorized; navigate to an allowed domain');
  }
}


function send(obj) {
  console.log(JSON.stringify(obj));
}

function formatPlaywrightError(e, action, timeoutSecs) {
  const msg = e?.message || String(e);
  const looksLikeTimeout = /timed out|timeout/i.test(msg);
  const looksLikeMissingBrowser = /Executable doesn't exist|browserType\.launch|playwright install/i.test(msg);
  if (looksLikeMissingBrowser) {
    return `${action}: Playwright browser is not installed. Run "npx playwright install chromium" (or use install_playwright in Akasha tools).`;
  }
  if (looksLikeTimeout) {
    return `${action}: timed out after ${timeoutSecs}s. Retry with a higher timeout_secs. If browser binaries are missing, run "npx playwright install chromium" (install_playwright).`;
  }
  return msg;
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
      serviceWorkers: 'block',
    });
    allowedDomains = []; blockedDomains = []; consoleLog = []; networkLog = [];
    sockets.clear();
    await context.route('**/*', async route => {
      if (domainAllowed(route.request().url())) await route.continue();
      else {appendLog(networkLog, { kind: 'blocked', url: scrubUrl(route.request().url()) }); await route.abort('blockedbyclient');}
    });
    // Playwright >= 1.48; a routed socket does not connect until connectToServer is called.
    await context.routeWebSocket('**/*', socket => {
      if (domainAllowed(socket.url())) {sockets.add(socket); socket.connectToServer();}
      else void socket.close({code:1008, reason:'Domain denied'});
    });
    page = await context.newPage();
    page.on('console', event => appendLog(consoleLog, { kind:event.type(), text:scrubText(event.text()) }));
    page.on('pageerror', error => appendLog(consoleLog, { kind:'pageerror', text:scrubText(error.message) }));
    page.on('response', response => appendLog(networkLog, {kind:'response',url:scrubUrl(response.url()),status:response.status()}));
    page.on('requestfailed', request => appendLog(networkLog, {kind:'failed',url:scrubUrl(request.url()),error:scrubText(request.failure()?.errorText ?? '')}));
    page.setDefaultTimeout((params.action_timeout_secs || 30) * 1000);
    send({ ok: true, result: 'initialized', protocol: 2 });
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
    const allowHttp = !!params.allow_http;
    if (!url.startsWith('https://') && !(allowHttp && url.startsWith('http://'))) {
      send({ ok: false, error: allowHttp ? 'Invalid URL: expected http(s) URL.' : 'Domain policy: only https URLs are allowed by default (set allow_http=true only for trusted local/test domains).' });
      return;
    }
    const timeoutSecs = params.timeout_secs ?? 30;
    const response = await page.goto(url, { waitUntil: 'domcontentloaded', timeout: timeoutSecs * 1000 });
    const title = await page.title();
    const status = response ? response.status() : 0;
    send({ ok: true, result: { title, status: status, url: page.url() } });
  } catch (e) {
    send({ ok: false, error: formatPlaywrightError(e, 'navigate', params.timeout_secs ?? 30) });
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
    send({ ok: false, error: formatPlaywrightError(e, 'snapshot', 30) });
  }
}

async function handleClick(params) {
  if (!page) {
    send({ ok: false, error: 'Browser not initialized; send init first.' });
    return;
  }
  try {
    const sel = (params && params.selector) || '';
    if (!sel) {
      send({ ok: false, error: 'click requires params.selector (CSS selector)' });
      return;
    }
    const timeout = (params.timeout_secs ?? 30) * 1000;
    await page.click(sel, { timeout });
    send({ ok: true, result: { clicked: true, selector: sel } });
  } catch (e) {
    send({ ok: false, error: formatPlaywrightError(e, 'click', params.timeout_secs ?? 30) });
  }
}

async function handleFill(params) {
  if (!page) {
    send({ ok: false, error: 'Browser not initialized; send init first.' });
    return;
  }
  try {
    const sel = (params && params.selector) || '';
    const value = (params && params.value) != null ? String(params.value) : '';
    if (!sel) {
      send({ ok: false, error: 'fill requires params.selector' });
      return;
    }
    const timeout = (params.timeout_secs ?? 30) * 1000;
    await page.fill(sel, value, { timeout });
    send({ ok: true, result: { filled: true, selector: sel } });
  } catch (e) {
    send({ ok: false, error: formatPlaywrightError(e, 'fill', params.timeout_secs ?? 30) });
  }
}

async function handleWait(params) {
  if (!page) {
    send({ ok: false, error: 'Browser not initialized; send init first.' });
    return;
  }
  try {
    const ms = params && params.milliseconds;
    const sel = params && params.selector;
    if (typeof ms === 'number' && ms >= 0 && !sel) {
      const capped = Math.min(ms, 120000);
      await new Promise((r) => setTimeout(r, capped));
      send({ ok: true, result: { waited_ms: capped } });
      return;
    }
    if (!sel || typeof sel !== 'string') {
      send({ ok: false, error: 'wait requires params.selector (string) or params.milliseconds (number)' });
      return;
    }
    const timeout = (params.timeout_secs ?? 30) * 1000;
    await page.waitForSelector(sel, { timeout });
    send({ ok: true, result: { waited: true, selector: sel } });
  } catch (e) {
    send({ ok: false, error: e.message || String(e) });
  }
}

async function handleScreenshot(params) {
  if (!page) {
    send({ ok: false, error: 'Browser not initialized; send init first.' });
    return;
  }
  try {
    const fullPage = !!(params && params.full_page);
    const buf = await page.screenshot({ type: 'png', fullPage });
    send({ ok: true, result: { format: 'png', data_base64: buf.toString('base64'), url:scrubUrl(page.url()), viewport:page.viewportSize() } });
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
    case 'configure_policy':
      try { await configurePolicy(params); send({ok:true,result:'policy configured'}); }
      catch (error) {send({ok:false,error:scrubText(error.message)});}
      break;
    case 'viewport':
      if (!page) {send({ok:false,error:'Navigate first'}); break;}
      try {
        const width=Number(params.width), height=Number(params.height);
        if (!Number.isInteger(width) || !Number.isInteger(height) || width<320 || width>3840 || height<240 || height>2160) throw new Error('Viewport outside supported bounds');
        await page.setViewportSize({width,height}); send({ok:true,result:{width,height}});
      } catch (error) {send({ok:false,error:scrubText(error.message)});}
      break;
    case 'console':
      send({ok:true,result:{url:scrubUrl(page?.url() ?? ''), entries:consoleLog}});
      break;
    case 'network':
      send({ok:true,result:{url:scrubUrl(page?.url() ?? ''), entries:networkLog}});
      break;
    case 'navigate':
      await handleNavigate(params);
      break;
    case 'snapshot':
      await handleSnapshot();
      break;
    case 'click':
      await handleClick(params);
      break;
    case 'fill':
      await handleFill(params);
      break;
    case 'wait':
      await handleWait(params);
      break;
    case 'screenshot':
      await handleScreenshot(params);
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
