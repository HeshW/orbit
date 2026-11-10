// Orbit UI smoke test: serves ui/ over HTTP, opens it in Chromium against the mock backend
// (ui/js/api.js runs it when there is no Tauri), and in each theme visits every sidebar view,
// opens the command palette and the process details drawer. Fails on any console error,
// page error or empty view.
//
//   npm run test:ui                       Playwright's Chromium (npx playwright-core install chromium)
//   ORBIT_BROWSER=msedge npm run test:ui  the installed Microsoft Edge, no browser download

import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { extname, join, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';

const ROOT = resolve(fileURLToPath(new URL('../ui/', import.meta.url)));
const VIEWS = ['processes', 'performance', 'history', 'startup', 'users', 'services', 'network', 'security', 'optimize', 'settings'];
const OPTIMIZE_TABS = ['health', 'cleanup', 'boost', 'drivers'];
const THEMES = ['night', 'void', 'signal', 'daylight'];
const TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
  '.ico': 'image/x-icon',
  '.ttf': 'font/ttf',
  '.woff2': 'font/woff2',
};
const WAIT = 8000;

// ------------------------------------------------------------------ static server

function serve() {
  const server = createServer(async (req, res) => {
    try {
      let path = decodeURIComponent(new URL(req.url, 'http://x').pathname);
      if (path.endsWith('/')) path += 'index.html';
      const file = resolve(join(ROOT, path));
      if (file !== ROOT && !file.startsWith(ROOT + sep)) throw new Error('outside');
      const body = await readFile(file);
      res.writeHead(200, { 'content-type': TYPES[extname(file).toLowerCase()] || 'application/octet-stream', 'cache-control': 'no-store' });
      res.end(body);
    } catch {
      res.writeHead(404, { 'content-type': 'text/plain' });
      res.end('Not found');
    }
  });
  return new Promise((ok) => server.listen(0, '127.0.0.1', () => ok(server)));
}

// ------------------------------------------------------------------ checks

const failures = [];
let step = 'load';

function fail(msg) {
  failures.push(`[${step}] ${msg}`);
  console.error(`  FAIL ${msg}`);
}

async function check(label, fn) {
  step = label;
  try {
    await fn();
    console.log(`  ok   ${label}`);
  } catch (err) {
    fail(String(err?.message || err).split('\n')[0]);
  }
}

async function showView(page, view) {
  await page.click(`#sidebar .nav-item[data-view="${view}"]`);
  await page.waitForSelector(`#view-${view}.is-active`, { timeout: WAIT });
  // A view counts as empty when it shows no text or has no size once it had time to render.
  await page.waitForFunction((id) => {
    const el = document.getElementById(id);
    return !!el && el.offsetHeight > 0 && el.innerText.trim().length > 0;
  }, `view-${view}`, { timeout: WAIT }).catch(() => {
    throw new Error(`#view-${view} is empty`);
  });
}

// Optimize: every sub-tab shows text; Health fills in from the mock report.
async function optimizeTabs(page) {
  await showView(page, 'optimize');
  for (const tab of OPTIMIZE_TABS) {
    await page.click(`#view-optimize .op-tabs [data-tab="${tab}"]`);
    await page.waitForFunction((id) => {
      const el = document.getElementById(id);
      return !!el && !el.hidden && el.offsetHeight > 0 && el.innerText.trim().length > 0;
    }, `op-pane-${tab}`, { timeout: WAIT }).catch(() => {
      throw new Error(`Optimize > ${tab} is empty`);
    });
  }
  await page.click('#view-optimize .op-tabs [data-tab="cleanup"]');
  await page.waitForSelector('#op-pane-cleanup .cl-cat', { timeout: WAIT }).catch(() => {
    throw new Error('Optimize > Cleanup shows no categories');
  });
  await page.click('#view-optimize .op-tabs [data-tab="boost"]');
  await page.waitForFunction(() => /Boost is (off|on)/.test(document.querySelector('#op-pane-boost .bo-state')?.textContent || ''), null, { timeout: WAIT }).catch(() => {
    throw new Error('Optimize > Boost shows no status');
  });
  await page.click('#view-optimize .op-tabs [data-tab="health"]');
  await page.waitForSelector('#op-pane-health .hl-item', { timeout: WAIT }).catch(() => {
    throw new Error('Optimize > Health shows no checks');
  });
}

// Performance > GPU names each adapter (mock: two) with its video memory and driver.
async function gpuAdapters(page) {
  await showView(page, 'performance');
  await page.click('#view-performance .perf-tile--gpu');
  await page.waitForFunction(() => document.querySelectorAll('#perf-pane-gpu .perf-adapter').length === 2, null, { timeout: WAIT }).catch(() => {
    throw new Error('Performance > GPU does not list two adapters');
  });
  const text = await page.locator('#perf-pane-gpu .perf-adapters').innerText();
  if (!/RTX 4070/.test(text) || !/VRAM/.test(text) || !/Driver \d/.test(text)) throw new Error(`GPU adapters miss name, VRAM or driver: ${text.replace(/\s+/g, ' ')}`);
  const title = await page.locator('#perf-pane-gpu [data-ref="title"]').innerText();
  if (!/RTX 4070/.test(title)) throw new Error(`GPU pane title is "${title}"`);
  await page.click('#view-performance .perf-tile--cpu');
}

async function setTheme(page, theme) {
  await showView(page, 'settings');
  await page.click(`#view-settings .lv-theme[data-pick="${theme}"]`);
  await page.waitForFunction((t) => document.documentElement.dataset.theme === t, theme, { timeout: WAIT });
}

async function palette(page) {
  await page.keyboard.press('Control+K');
  await page.waitForSelector('#palette.is-open .palette-input', { timeout: WAIT });
  await page.keyboard.type('perf');
  await page.waitForSelector('#palette .palette-item', { timeout: WAIT }).catch(() => {
    throw new Error('palette shows no items for "perf"');
  });
  await page.keyboard.press('Escape');
  await page.waitForSelector('#palette:not(.is-open)', { state: 'attached', timeout: WAIT });
}

async function drawer(page) {
  await showView(page, 'processes');
  const row = page.locator('#view-processes .proc-canvas > .proc-row.row').first();
  await row.waitFor({ timeout: WAIT });
  await row.dblclick();
  await page.waitForSelector('#details.is-open', { timeout: WAIT }).catch(() => {
    throw new Error('details drawer did not open on double-click');
  });
  const text = await page.locator('#details').innerText();
  if (!text.trim()) throw new Error('details drawer is empty');
  await page.keyboard.press('Escape');
  await page.waitForSelector('#details:not(.is-open)', { state: 'attached', timeout: WAIT });
}

// ------------------------------------------------------------------ run

const server = await serve();
const base = `http://127.0.0.1:${server.address().port}/`;
const channel = process.env.ORBIT_BROWSER || undefined;
let browser;

try {
  browser = await chromium.launch({ channel, headless: true });
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  const page = await context.newPage();
  page.on('console', (msg) => {
    if (msg.type() === 'error') fail(`console error: ${msg.text()}`);
  });
  page.on('pageerror', (err) => fail(`page error: ${err.message}`));

  console.log(`Orbit UI smoke test (${channel || 'chromium'}) at ${base}`);
  await check('load', async () => {
    await page.goto(base, { waitUntil: 'load' });
    await page.waitForSelector('#view-processes.is-active', { timeout: WAIT });
  });

  for (const theme of THEMES) {
    await check(`${theme}: theme`, () => setTheme(page, theme));
    for (const view of VIEWS) {
      await check(`${theme}: ${view}`, () => showView(page, view));
    }
    await check(`${theme}: optimize tabs`, () => optimizeTabs(page));
    await check(`${theme}: palette`, () => palette(page));
    await check(`${theme}: details drawer`, () => drawer(page));
  }
  await check('gpu adapters', () => gpuAdapters(page));
  // Let late timers (snapshot ticks, debounced renders) report their errors too.
  step = 'idle';
  await page.waitForTimeout(1500);
} catch (err) {
  fail(String(err?.message || err).split('\n')[0]);
} finally {
  await browser?.close();
  server.close();
}

if (failures.length) {
  console.error(`\n${failures.length} problem(s):\n${failures.join('\n')}`);
  process.exit(1);
}
console.log(`\nPassed: ${THEMES.length} themes x ${VIEWS.length} views, ${OPTIMIZE_TABS.length} Optimize tabs, palette and details drawer, GPU adapters, no console errors.`);
