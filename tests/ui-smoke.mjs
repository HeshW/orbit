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
const VIEWS = ['processes', 'performance', 'history', 'startup', 'users', 'services', 'network', 'security', 'optimize', 'rig', 'settings'];
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

async function menuLabels(page) {
  await page.waitForSelector('#context-menu:not([hidden]) .menu-item', { timeout: WAIT });
  return page.locator('#context-menu .menu-item__label').allInnerTexts();
}

// History: right-click a running app (mock chrome.exe) and an app that is not running;
// Shift+F10 opens the same menu from the keyboard; Remove from history drops the row.
async function historyMenu(page) {
  await showView(page, 'history');
  const chrome = page.locator('#view-history tr.row[data-key$="\\\\chrome.exe"]');
  await chrome.waitFor({ timeout: WAIT });
  await chrome.click({ button: 'right' });
  let labels = await menuLabels(page);
  for (const want of ['Search online', 'Open file location', 'Properties', 'Copy name', 'Copy path', 'Show in Processes', 'End task', 'Remove from history']) {
    if (!labels.includes(want)) throw new Error(`History menu for chrome.exe lacks "${want}": ${labels.join(', ')}`);
  }
  await page.keyboard.press('Escape');
  await page.waitForSelector('#context-menu[hidden]', { state: 'attached', timeout: WAIT });

  // Keyboard: Shift+F10 on the focused row. The mock runs none of these apps, and the
  // seeded history has some of them on any day.
  const idle = page.locator(['rustc', 'photoshop', 'blender', 'audacity']
    .map((n) => `#view-history tr.row[data-key$="\\\\${n}.exe"]`).join(', ')).first();
  await idle.focus();
  await page.keyboard.press('Shift+F10');
  labels = await menuLabels(page);
  if (!labels.includes('Remove from history')) throw new Error(`Shift+F10 menu: ${labels.join(', ')}`);
  if (labels.includes('End task') || labels.includes('Show in Processes')) throw new Error('History menu offers End task for an app that is not running');
  await page.keyboard.press('Escape');
  await page.waitForSelector('#context-menu[hidden]', { state: 'attached', timeout: WAIT });

  const before = await page.locator('#view-history tr.row').count();
  // A row in view: scrolling closes the menu.
  const row = page.locator('#view-history tr.row').nth(3);
  const key = await row.getAttribute('data-key');
  await row.click({ button: 'right' });
  await menuLabels(page);
  await page.locator('#context-menu .menu-item', { hasText: 'Remove from history' }).click();
  await page.click('#dialog [data-act="ok"]');
  await page.waitForFunction(([k, n]) => {
    const rows = [...document.querySelectorAll('#view-history tr.row')];
    return rows.length === n - 1 && !rows.some((r) => r.dataset.key === k);
  }, [key, before], { timeout: WAIT }).catch(() => {
    throw new Error('Remove from history left the row in the table');
  });
}

// My Rig: seven cards from the mock, Copy specs puts the summary on the clipboard; key 0
// opens My Rig and Ctrl+, opens Settings.
async function myRig(page) {
  await showView(page, 'processes');
  await page.keyboard.press('0');
  await page.waitForSelector('#view-rig.is-active', { timeout: WAIT }).catch(() => {
    throw new Error('key 0 does not open My Rig');
  });
  await page.waitForFunction(() => document.querySelectorAll('#view-rig .rig-card').length === 7, null, { timeout: WAIT }).catch(() => {
    throw new Error('My Rig does not show seven cards');
  });
  const text = await page.locator('#view-rig .rig-grid').innerText();
  for (const want of ['Intel Core i7-13700H', '32 GB', 'DDR5', 'RTX 4070', 'LPCN49WW', '1 TB', '3840 x 2160', 'Windows 11 Home']) {
    if (!text.includes(want)) throw new Error(`My Rig lacks "${want}"`);
  }
  await page.click('#view-rig [data-copy]');
  await page.waitForFunction(() => /Copied your specs/.test(document.getElementById('toasts')?.innerText || ''), null, { timeout: WAIT }).catch(() => {
    throw new Error('Copy specs showed no "Copied" toast');
  });
  const copied = await page.evaluate(async () => {
    const { call } = await import('/js/api.js');
    const { specsText } = await import('/js/views/rig.js');
    return specsText(await call('get_rig_specs'));
  });
  if (!/^PC: Legion Pro 5/.test(copied) || !/\nCPU: 13th Gen Intel Core i7-13700H \(14 cores, 20 threads, 2\.40 GHz base\)\n/.test(copied)) {
    throw new Error(`Copy specs text: ${copied.split('\n').slice(0, 2).join(' | ')}`);
  }
  await page.keyboard.press('Control+,');
  await page.waitForSelector('#view-settings.is-active', { timeout: WAIT }).catch(() => {
    throw new Error('Ctrl+, does not open Settings');
  });
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
  await check('history menu', () => historyMenu(page));
  await check('my rig', () => myRig(page));
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
console.log(`\nPassed: ${THEMES.length} themes x ${VIEWS.length} views, ${OPTIMIZE_TABS.length} Optimize tabs, palette and details drawer, GPU adapters, History menu, My Rig, no console errors.`);
