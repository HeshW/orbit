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
const OPTIMIZE_TABS = ['health', 'cleanup', 'boost', 'drivers', 'debloat'];
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

// Performance temperatures (0.7, 9b). In every theme: the CPU and GPU tiles show °C beside the
// load, each temperature carries the temps.js level of its own number and is drawn in that
// level's colour (--ok, --warn, --danger), and both Sensors blocks render with their source.
async function sensorsTheme(page) {
  await showView(page, 'performance');
  await page.click('#view-performance .perf-tile--cpu');
  await page.waitForFunction(() => {
    const t = (k) => document.querySelector(`#view-performance .perf-tile--${k} [data-ref="t"]`);
    return ['cpu', 'gpu'].every((k) => t(k) && !t(k).hidden && /^\d+°C$/.test(t(k).textContent));
  }, null, { timeout: WAIT }).catch(() => {
    throw new Error('CPU and GPU tiles show no temperature');
  });
  const bad = await page.evaluate(async () => {
    const { tempLevel } = await import('/js/temps.js');
    const colour = (v) => {
      const probe = document.createElement('span');
      probe.style.color = `var(${v})`;
      document.querySelector('#view-performance').append(probe);
      const c = getComputedStyle(probe).color;
      probe.remove();
      return c;
    };
    const want = { normal: colour('--ok'), warm: colour('--warn'), hot: colour('--danger') };
    const out = [];
    const els = [
      ['cpu', '.perf-tile--cpu [data-ref="t"]'], ['gpu', '.perf-tile--gpu [data-ref="t"]'],
      ['cpu', '#perf-pane-cpu [data-ref="sensTemp"]'], ['cpu', '#perf-pane-cpu .perf-sensor-core__temp'],
    ];
    for (const [kind, sel] of els) {
      for (const el of document.querySelectorAll(`#view-performance ${sel}`)) {
        const c = parseFloat(el.textContent);
        const level = tempLevel(kind, c);
        if (!el.classList.contains(`temp--${level}`)) out.push(`${sel} ${el.textContent} lacks temp--${level}`);
        else if (el.getClientRects().length && getComputedStyle(el).color !== want[level]) out.push(`${sel} ${el.textContent} not in the ${level} colour`);
      }
    }
    return out;
  });
  if (bad.length) throw new Error(bad.join('; '));
  const cpu = await page.locator('#perf-pane-cpu .perf-sensors').innerText();
  if (!/From LibreHardwareMonitor/.test(cpu) || !/PACKAGE POWER/i.test(cpu) || !/AVERAGE CLOCK/i.test(cpu)) throw new Error(`CPU Sensors block: ${cpu.replace(/\s+/g, ' ')}`);
  const cells = await page.locator('#perf-pane-cpu .perf-sensor-core').allInnerTexts();
  if (cells.length !== 4 || !cells.every((t) => /Core \d[\s\S]*\d+°C[\s\S]*GHz/i.test(t))) throw new Error(`per-core cells: ${cells.join(' | ').replace(/\s+/g, ' ')}`);
  await page.click('#view-performance .perf-tile--gpu');
  await page.waitForSelector('#perf-pane-gpu [data-ref="sensBody"]:not([hidden])', { timeout: WAIT }).catch(() => {
    throw new Error('GPU Sensors block is hidden');
  });
  const gpu = await page.locator('#perf-pane-gpu .perf-sensors').innerText();
  for (const want of ['From NVIDIA', 'PEAK IN WINDOW', 'SLOWS DOWN AT', 'FAN', 'POWER', 'CORE CLOCK', 'MEMORY CLOCK']) {
    if (!gpu.toUpperCase().includes(want.toUpperCase())) throw new Error(`GPU Sensors block lacks "${want}": ${gpu.replace(/\s+/g, ' ')}`);
  }
  // The Intel card has no library Orbit reads: said once, on its own adapter line.
  await page.waitForSelector('#perf-pane-gpu .perf-adapter__note:not([hidden])', { timeout: WAIT }).catch(() => {});
  const notes = await page.locator('#perf-pane-gpu .perf-adapter__note:not([hidden])').allInnerTexts();
  if (notes.length !== 1 || !/not available on this GPU/.test(notes[0])) throw new Error(`adapter notes: ${notes.join(' | ')}`);
  await page.click('#view-performance .perf-tile--cpu');
}

// Thresholds and text (temps.js), °F from Settings, no CPU source (hint with both links,
// which open through open_url), and a hot PC (?hot) in the danger colour.
async function sensors(page, base, watch) {
  const bad = await page.evaluate(async () => {
    const { tempLevel, tempText, TEMP_LIMITS } = await import('/js/temps.js');
    const cases = [
      [tempLevel('cpu', 79.9), 'normal'], [tempLevel('cpu', 80), 'warm'], [tempLevel('cpu', 89.9), 'warm'],
      [tempLevel('cpu', 90), 'hot'], [tempLevel('gpu', 86.9), 'warm'], [tempLevel('gpu', 87), 'hot'],
      [tempLevel('gpu', null), null], [tempLevel('cpu', NaN), null],
      [tempText(62), '62°C'], [tempText(100, 'f'), '212°F'], [tempText(null), '–'], [tempText(61.25, 'c', 1), '61.3°C'],
      [TEMP_LIMITS.cpu.hot, 90], [TEMP_LIMITS.gpu.warm, 80],
    ];
    return cases.map(([got, want], i) => (got === want ? null : `case ${i}: ${got} !== ${want}`)).filter(Boolean);
  });
  if (bad.length) throw new Error(bad.join('; '));

  await showView(page, 'settings');
  await page.selectOption('#set-temp', 'f');
  await showView(page, 'performance');
  await page.waitForFunction(() => /^\d+°F$/.test(document.querySelector('#view-performance .perf-tile--cpu [data-ref="t"]')?.textContent || ''), null, { timeout: WAIT }).catch(() => {
    throw new Error('°F setting does not reach the CPU tile');
  });
  await showView(page, 'settings');
  await page.selectOption('#set-temp', 'c');

  const other = await page.context().newPage();
  watch(other);
  try {
    await other.goto(`${base}?nocputemp`, { waitUntil: 'load' });
    await showView(other, 'performance');
    await other.click('#view-performance .perf-tile--cpu');
    await other.waitForSelector('#perf-pane-cpu [data-ref="sensEmpty"]:not([hidden])', { timeout: WAIT }).catch(() => {
      throw new Error('?nocputemp: no hint in the CPU Sensors block');
    });
    const hint = await other.locator('#perf-pane-cpu [data-ref="sensEmpty"]').innerText();
    if (!/Windows doesn't report CPU temperature/.test(hint)) throw new Error(`?nocputemp hint: ${hint}`);
    const links = await other.locator('#perf-pane-cpu [data-ref="sensEmpty"] a[data-url]').evaluateAll((as) => as.map((a) => a.dataset.url));
    if (links.join(' ') !== 'https://www.hwinfo.com/ https://github.com/LibreHardwareMonitor/LibreHardwareMonitor') throw new Error(`?nocputemp links: ${links.join(' ')}`);
    if (!await other.locator('#perf-pane-cpu [data-ref="sensBody"]').isHidden()) throw new Error('?nocputemp: Sensors body still shown');
    if (!await other.locator('#view-performance .perf-tile--cpu [data-ref="t"]').isHidden()) throw new Error('?nocputemp: CPU tile still shows a temperature');
    const url = other.url();
    await other.click('#perf-pane-cpu [data-ref="sensEmpty"] a[data-url*="hwinfo"]');
    await other.waitForTimeout(200);
    if (other.url() !== url) throw new Error('the HWiNFO link navigated the window instead of opening the browser');

    await other.goto(`${base}?hot`, { waitUntil: 'load' });
    await showView(other, 'performance');
    await other.click('#view-performance .perf-tile--cpu');
    await other.waitForSelector('#view-performance .perf-tile--cpu [data-ref="t"].temp--hot', { timeout: WAIT }).catch(() => {
      throw new Error('?hot: the CPU tile is not hot');
    });
    const red = await other.evaluate(() => {
      const el = document.querySelector('#perf-pane-cpu [data-ref="sensTemp"]');
      const probe = document.createElement('span');
      probe.style.color = 'var(--danger)';
      el.after(probe);
      const same = getComputedStyle(el).color === getComputedStyle(probe).color;
      probe.remove();
      return same && el.classList.contains('temp--hot');
    });
    if (!red) throw new Error('?hot: the package temperature is not in the danger colour');
  } finally {
    await other.close();
  }
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
  for (const want of ['Search online', 'Open file location', 'Properties', 'Scan with Defender', 'Copy name', 'Copy path', 'Show in Processes', 'End task', 'Remove from history']) {
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

// Security > Malware scan (Microsoft Defender, mock): status, a quick scan with progress and
// a clean result, a full scan cancelled, a picked file with threats, "Scan with Defender" in
// the Processes menu, and (?otherav) another antivirus named with the scan buttons hidden.
async function defender(page, base, watch) {
  const panel = '#view-security [data-defender]';
  const state = (s) => page.waitForSelector(`${panel} .df-run[data-state="${s}"]`, { timeout: WAIT }).catch(() => {
    throw new Error(`Defender panel never showed "${s}"`);
  });
  await showView(page, 'security');
  await page.waitForFunction((sel) => /Microsoft Defender is on/.test(document.querySelector(sel)?.innerText || ''), panel, { timeout: WAIT }).catch(() => {
    throw new Error('Defender panel does not say "Microsoft Defender is on"');
  });
  const facts = await page.locator(`${panel} .df-facts`).innerText();
  for (const want of ['REAL-TIME PROTECTION', 'Up to date', '1.463.223.0', 'LAST QUICK SCAN', 'LAST FULL SCAN', 'Never', '4.18.26100.4']) {
    if (!facts.toUpperCase().includes(want.toUpperCase())) throw new Error(`Defender facts lack "${want}": ${facts.replace(/\s+/g, ' ')}`);
  }

  await page.click(`${panel} [data-df-scan="quick"]`);
  await state('running');
  if (!await page.locator(`${panel} [data-df-scan="full"]`).isDisabled()) throw new Error('scan buttons stay enabled while a scan runs');
  await page.waitForFunction((sel) => /^0:0[1-9]$/.test(document.querySelector(`${sel} [data-df-elapsed]`)?.textContent || ''), panel, { timeout: WAIT }).catch(() => {
    throw new Error('running scan shows no elapsed time');
  });
  await state('clean');
  if (!/No threats found\. Quick scan, 0:0\d\./.test(await page.locator(`${panel} .df-run`).innerText())) throw new Error('quick scan result text');
  if (!/Today/.test(await page.locator(`${panel} .df-facts`).innerText())) throw new Error('last quick scan did not update to today');

  await page.click(`${panel} [data-df-scan="full"]`);
  await page.click('#dialog [data-act="ok"]');
  await state('running');
  await page.click(`${panel} [data-df-act="cancel"]`);
  await state('cancelled');

  await page.click(`${panel} [data-df-scan="pick"]`);
  await menuLabels(page);
  await page.locator('#context-menu .menu-item', { hasText: 'A file' }).click();
  await state('threats');
  const threats = await page.locator(`${panel} .df-threat`).allInnerTexts();
  if (threats.length !== 2 || !threats.includes('Trojan:Win32/Wacatac.B!ml')) throw new Error(`threat names: ${threats.join(', ')}`);
  if (!await page.locator(`${panel} [data-df-act="threats"]`).isVisible()) throw new Error('no Open Windows Security button for threats');

  await showView(page, 'processes');
  const row = page.locator('#view-processes .proc-canvas > .proc-row.row').first();
  await row.click({ button: 'right' });
  const labels = await menuLabels(page);
  if (!labels.includes('Scan with Defender')) throw new Error(`Processes menu lacks "Scan with Defender": ${labels.join(', ')}`);
  await page.locator('#context-menu .menu-item', { hasText: 'Scan with Defender' }).click();
  await page.waitForFunction(() => /No threats found in /.test(document.getElementById('toasts')?.innerText || ''), null, { timeout: WAIT }).catch(() => {
    throw new Error('Scan with Defender from Processes showed no result toast');
  });

  // Another antivirus is active: named, Defender explained, no scan buttons.
  const other = await page.context().newPage();
  watch(other);
  try {
    await other.goto(`${base}?otherav`, { waitUntil: 'load' });
    await showView(other, 'security');
    await other.waitForFunction((sel) => /Norton 360 protects this PC/.test(document.querySelector(sel)?.innerText || ''), panel, { timeout: WAIT }).catch(() => {
      throw new Error('?otherav: panel does not name Norton 360');
    });
    const text = await other.locator(panel).innerText();
    if (!/Microsoft Defender is off because Norton 360/.test(text)) throw new Error(`?otherav text: ${text.replace(/\s+/g, ' ')}`);
    if (await other.locator(`${panel} [data-df-scan]`).count()) throw new Error('?otherav still shows scan buttons');
  } finally {
    await other.close();
  }
}

// Optimize > Debloat (mock): Health's startup warning links here; Analyze groups the PC with
// every verdict; only Safe items can be ticked; Review changes previews exactly what changes
// (admin-only items skipped); applying lists the changes; Undo and Undo all put things back;
// a removed app's Undo opens its Store page.
async function debloat(page) {
  const pane = '#op-pane-debloat';
  const waitTitle = (re, what) => page.waitForFunction(([sel, src]) => new RegExp(src).test(document.querySelector(sel)?.textContent || ''),
    [`${pane} [data-db-title]`, re.source], { timeout: WAIT }).catch(() => { throw new Error(`summary never said ${what}`); });
  const toastSays = (re, what) => page.waitForFunction((src) => new RegExp(src).test(document.getElementById('toasts')?.innerText || ''),
    re.source, { timeout: WAIT }).catch(() => { throw new Error(`no toast ${what}`); });
  const item = (id) => page.locator(`${pane} [data-db-item]`).filter({ has: page.locator(`[data-db-pick="${id.replace(/\\/g, '\\\\')}"]`) });
  const pick = (id) => page.locator(`${pane} [data-db-pick="${id.replace(/\\/g, '\\\\')}"]`).check();

  await showView(page, 'optimize');
  await page.click('#view-optimize .op-tabs [data-tab="health"]');
  const link = page.locator('#op-pane-health .hl-item', { hasText: 'Startup load' }).locator('button', { hasText: 'Review in Debloat' });
  await link.click({ timeout: WAIT }).catch(() => { throw new Error('Health has no "Review in Debloat" on the startup warning'); });
  await page.waitForSelector(`${pane}:not([hidden])`, { timeout: WAIT });

  if (await page.locator(`${pane} [data-db-start]`).count()) await page.click(`${pane} [data-db-start]`);
  else await page.click(`${pane} [data-db-analyze]`);
  await waitTitle(/^16 things are safe to turn off$/, '"16 things are safe to turn off"');
  const groups = await page.locator(`${pane} [data-db-group] .db-group-title`).allInnerTexts();
  if (groups.join('|') !== 'Startup apps|Scheduled tasks|Services|Built-in apps') throw new Error(`groups: ${groups.join(', ')}`);
  for (const g of ['startup', 'task', 'service', 'app']) {
    for (const v of ['off', 'keep']) {
      if (!await page.locator(`${pane} [data-db-group="${g}"] [data-db-verdict="${v}"]`).count()) throw new Error(`${g} has no ${v} item`);
    }
    if (g !== 'app' && !await page.locator(`${pane} [data-db-group="${g}"] [data-db-verdict="unknown"]`).count()) throw new Error(`${g} has no unknown item`);
  }
  // Keep and Unknown can't be ticked; Unknown offers Search online and, for startup, the impact hint.
  if (await page.locator(`${pane} .db-item--keep [data-db-pick], ${pane} .db-item--unknown [data-db-pick]`).count()) throw new Error('a Keep or Unknown item has a tick box');
  await page.locator(`${pane} [data-db-sub="startup:unknown"] > summary`).click();
  const synapse = await page.locator(`${pane} .db-item--unknown`, { hasText: 'Synapse3' }).innerText();
  if (!/Search online/i.test(synapse) || !/high startup cost/.test(synapse)) throw new Error(`unknown startup app: ${synapse.replace(/\s+/g, ' ')}`);
  const steam = await item('startup:HKCU\\Run\\Steam').innerText();
  if (!/Safe to turn off/.test(steam) || !/You lose/.test(steam) || !/may start a little faster/.test(steam)) throw new Error(`Steam row: ${steam.replace(/\s+/g, ' ')}`);

  // Preview: exactly what changes; the service is skipped for admin rights.
  await pick('startup:HKCU\\Run\\Steam');
  await pick('app:Microsoft.BingWeather_8wekyb3d8bbwe');
  await pick('task:\\ZoomUpdateTaskUser-S-1-5-21-1001');
  await pick('service:DiagTrack');
  if (!/Review changes \(4\)/i.test(await page.locator(`${pane} [data-db-review]`).innerText())) throw new Error('Review changes does not count 4 ticks');
  await page.click(`${pane} [data-db-review]`);
  await page.waitForSelector('#dialog [data-db-preview]', { timeout: WAIT });
  const pv = await page.locator('#dialog').innerText();
  for (const want of ['Apply 3 changes?', 'Steam', 'On at sign-in', 'Removed for you', 'Zoom updater', 'Skipped', 'Windows telemetry', 'Needs administrator rights.', 'Microsoft Store']) {
    if (!pv.includes(want)) throw new Error(`preview lacks "${want}": ${pv.replace(/\s+/g, ' ')}`);
  }
  await page.click('#dialog [data-act="ok"]');
  await toastSays(/Changed 3 items/, '"Changed 3 items"');
  await waitTitle(/^13 things/, '13 after applying');
  const rows = page.locator(`${pane} [data-db-change]`);
  if (await rows.count() !== 3) throw new Error(`Changes lists ${await rows.count()} rows`);
  if (!/Open Store page/i.test(await rows.filter({ hasText: 'Weather' }).innerText())) throw new Error('removed app has no Open Store page');

  // Undo one.
  await rows.filter({ hasText: 'Steam' }).locator('[data-db-undo]').click();
  await toastSays(/Steam starts at sign-in again/, 'for undoing Steam');
  await waitTitle(/^14 things/, '14 after undo');
  if (!/Undone/.test(await page.locator(`${pane} [data-db-change]`, { hasText: 'Steam' }).innerText())) throw new Error('Steam change not marked Undone');

  // As administrator the service can change too.
  await pick('service:DiagTrack');
  await page.locator(`${pane} [data-db-admin]`).click({ timeout: WAIT }).catch(() => { throw new Error('no Restart as administrator for a picked service'); });
  await page.waitForFunction((sel) => !document.querySelector(sel), `${pane} [data-db-admin]`, { timeout: WAIT }).catch(() => {
    throw new Error('still asks for administrator rights after the restart');
  });
  await page.click(`${pane} [data-db-review]`);
  await page.waitForSelector('#dialog [data-db-preview]', { timeout: WAIT });
  const pv2 = await page.locator('#dialog').innerText();
  if (!/Set to Manual: Automatic/.test(pv2)) throw new Error(`service preview: ${pv2.replace(/\s+/g, ' ')}`);
  await page.click('#dialog [data-act="ok"]');
  await toastSays(/Changed 1 item/, 'for the service');

  // Undo all: everything but the removed app, which waits for the Store.
  await page.click(`${pane} [data-db-undo-all]`);
  await page.click('#dialog [data-act="ok"]');
  await toastSays(/Undid 2 changes\. 1 removed app can come back from the Store\./, 'for Undo all');
  await waitTitle(/^15 things/, '15 after Undo all');
  await page.locator(`${pane} [data-db-change]`, { hasText: 'Weather' }).locator('[data-db-undo]').click();
  await toastSays(/Opened the Store page for Weather/, 'for the Store page');
  await waitTitle(/^16 things/, '16 once Weather is back');
  if (!/Undone/.test(await page.locator(`${pane} [data-db-change]`, { hasText: 'Weather' }).innerText())) throw new Error('Weather not marked Undone after reinstall');
  if (await page.locator(`${pane} [data-db-undo-all]`).isEnabled()) throw new Error('Undo all still enabled with nothing left');
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

// Motion: what a sample button, card and view do with Animations on, with the Settings
// switch off, and with Windows asking for less motion. Content must end fully visible.
async function motionState(page, view) {
  await showView(page, view);
  return page.evaluate(async () => {
    const v = document.querySelector('.view.is-active');
    const blocks = [...v.querySelectorAll('.view-head, .panel')].filter((el) => el.getClientRects().length);
    const entering = [...v.querySelectorAll('.enter-up')].map((el) => getComputedStyle(el));
    const btn = v.querySelector('.btn') || document.querySelector('.btn');
    const card = document.querySelector('.panel--hover, .lv-theme');
    const { countUp } = await import('/js/motion.js');
    const n = document.createElement('span');
    n.textContent = '32 GB';
    document.body.append(n);
    countUp(n);
    const counting = n.textContent;
    n.remove();
    return {
      lift: getComputedStyle(document.documentElement).getPropertyValue('--lift').trim(),
      btnTransition: getComputedStyle(btn).transitionDuration,
      sweep: getComputedStyle(btn, '::after').content,
      cardTransition: card ? getComputedStyle(card).transitionDuration : 'none',
      entering: entering.length,
      enterAnim: entering.map((s) => `${s.animationName} ${s.animationDuration}`)[0] || '',
      counting,
      blocks: blocks.length,
    };
  });
}

async function settledOpacity(page) {
  await page.waitForFunction(() => !document.querySelector('.view.is-active .enter-up'), null, { timeout: WAIT }).catch(() => {
    throw new Error('view blocks are still mid-entrance after 8 s');
  });
  return page.evaluate(() => [...document.querySelectorAll('.view.is-active .view-head, .view.is-active .panel')]
    .filter((el) => el.getClientRects().length)
    .map((el) => getComputedStyle(el).opacity)
    .filter((o) => o !== '1').length);
}

const animOff = (s) => /^(0s,?\s*)+$/.test(`${s.btnTransition}`) && /^(0s,?\s*)+$/.test(`${s.cardTransition}`)
  && s.lift === '0px' && s.sweep === 'none' && s.entering === 0 && s.counting === '32 GB';

async function motion(page) {
  const sw = '#view-settings [data-switch="reducedMotion"]';
  await showView(page, 'settings');
  if (await page.getAttribute(sw, 'aria-checked') !== 'true') throw new Error('Animations switch is not on by default');
  if (await page.evaluate(() => document.documentElement.classList.contains('reduce-motion'))) throw new Error('reduce-motion is set by default');

  // On: buttons and cards transition, the view's blocks rise in, numbers count from zero.
  let s = await motionState(page, 'rig');
  if (s.lift !== '-3px') throw new Error(`--lift is "${s.lift}", want -3px`);
  if (!/0\.25s/.test(s.btnTransition) || !/0\.25s/.test(s.cardTransition)) throw new Error(`no hover transition: button ${s.btnTransition}, card ${s.cardTransition}`);
  if (s.sweep === 'none') throw new Error('button has no hover sweep');
  if (!s.entering || !/^orbit-enter 0\.45s/.test(s.enterAnim)) throw new Error(`view switch did not animate: ${s.entering} blocks, "${s.enterAnim}"`);
  if (s.counting === '32 GB') throw new Error('countUp did not start from zero');
  const hidden = await settledOpacity(page);
  if (hidden) throw new Error(`${hidden} block(s) not at opacity 1 after the entrance`);

  // Settings switch off: every transition, lift, sweep, entrance and count-up stops.
  await showView(page, 'settings');
  await page.click(sw);
  await page.waitForFunction(() => document.documentElement.classList.contains('reduce-motion'), null, { timeout: WAIT });
  if (await page.getAttribute(sw, 'aria-checked') !== 'false') throw new Error('Animations switch did not turn off');
  s = await motionState(page, 'rig');
  if (!animOff(s)) throw new Error(`motion still on with Animations off: ${JSON.stringify(s)}`);
  if (await settledOpacity(page)) throw new Error('blocks not at opacity 1 with Animations off');
  await showView(page, 'settings');
  await page.click(sw);
  await page.waitForFunction(() => !document.documentElement.classList.contains('reduce-motion'), null, { timeout: WAIT });

  // Windows "show animations" off does the same with the switch on.
  await page.emulateMedia({ reducedMotion: 'reduce' });
  s = await motionState(page, 'performance');
  await page.emulateMedia({ reducedMotion: 'no-preference' });
  if (!animOff(s)) throw new Error(`motion still on under prefers-reduced-motion: ${JSON.stringify(s)}`);
}

// LegacyTech mark in the sidebar (0.7, 9a): drawn in the logo kit's colours for the theme
// (color-dark in Night, Void and Signal, color-light in Daylight), between the nav and the
// Live / System card without touching either.
const LOGO_COLOURS = {
  dark: ['rgb(242, 245, 255)', 'rgb(79, 140, 255)', 'rgb(56, 213, 240)'],
  light: ['rgb(10, 15, 31)', 'rgb(47, 111, 235)', 'rgb(11, 141, 184)'],
};

async function logoTheme(page, theme) {
  const s = await page.evaluate(() => {
    const logo = document.getElementById('sidebar-logo');
    if (!logo) return null;
    const bg = (sel) => getComputedStyle(logo.querySelector(sel)).backgroundColor;
    const r = logo.getBoundingClientRect();
    const nav = [...document.querySelectorAll('#sidebar .nav-item')].pop().getBoundingClientRect();
    const foot = document.getElementById('mini-readout').getBoundingClientRect();
    return {
      label: logo.getAttribute('aria-label'),
      shown: logo.getClientRects().length > 0,
      blocks: logo.querySelectorAll('.lt-px').length,
      colours: [bg('.lt-px'), bg('.lt-px--blue'), bg('.lt-sig')],
      clear: r.top >= nav.bottom && r.bottom <= foot.top,
    };
  });
  if (!s) throw new Error('no #sidebar-logo');
  if (s.label !== 'LegacyTech') throw new Error(`logo label is "${s.label}"`);
  if (!s.shown || s.blocks !== 7) throw new Error(`logo not drawn at 900px high (${s.blocks} blocks)`);
  if (!s.clear) throw new Error('logo overlaps the nav or the Live / System card');
  const want = LOGO_COLOURS[theme === 'daylight' ? 'light' : 'dark'];
  if (s.colours.join('|') !== want.join('|')) throw new Error(`logo colours ${s.colours.join(', ')}, want ${want.join(', ')}`);
}

// The mark's motion: the cyan pixel pulses while animations are allowed and the window shows,
// faster with CPU load; nothing animates with the switch off, under prefers-reduced-motion or
// while hidden. A short window hides it; a click opens Settings at About; Tab moves past it.
async function logoState(page) {
  return page.evaluate(() => {
    const logo = document.getElementById('sidebar-logo');
    return {
      pulsing: logo.classList.contains('is-pulsing'),
      anims: logo.getAnimations({ subtree: true }).map((a) => a.animationName),
      dim: [...logo.querySelectorAll('.lt-px, .lt-sig')].filter((el) => getComputedStyle(el).opacity !== '1').length,
    };
  });
}

const logoStill = (s) => !s.pulsing && !s.anims.length && !s.dim;

async function logo(page) {
  await showView(page, 'processes');
  await page.waitForFunction(() => document.getElementById('sidebar-logo')?.getAnimations({ subtree: true })
    .some((a) => a.animationName === 'lt-pulse'), null, { timeout: WAIT }).catch(() => {
    throw new Error('the signal pixel does not pulse with animations on');
  });
  if ((await logoState(page)).anims.includes('lt-assemble')) throw new Error('assembly still running long after start');

  // CPU load sets the pulse rate (updates paused so the mock's ticks do not race).
  const rates = await page.evaluate(async () => {
    const { setSettings } = await import('/js/store.js');
    const { logoTick, pulseRate } = await import('/js/logo.js');
    setSettings({ paused: true });
    const anim = document.querySelector('#sidebar-logo .lt-sig').getAnimations()[0];
    const at = async (usage) => {
      logoTick({ cpu: { usage } });
      await anim.ready;
      return anim.playbackRate;
    };
    const out = [await at(100), await at(0), await at(50), pulseRate(0), pulseRate(100)];
    setSettings({ paused: false });
    return out;
  });
  if (rates.join(',') !== '3,1,2,1,3') throw new Error(`pulse rates at 100/0/50% CPU: ${rates.join(', ')}`);

  // Window hidden (tray, minimised): no animation at all; back again when shown.
  const hidden = await page.evaluate(() => {
    Object.defineProperty(document, 'hidden', { configurable: true, get: () => true });
    document.dispatchEvent(new Event('visibilitychange'));
    const el = document.getElementById('sidebar-logo');
    const s = { pulsing: el.classList.contains('is-pulsing'), anims: el.getAnimations({ subtree: true }).length };
    delete document.hidden;
    document.dispatchEvent(new Event('visibilitychange'));
    s.back = el.classList.contains('is-pulsing');
    return s;
  });
  if (hidden.pulsing || hidden.anims || !hidden.back) throw new Error(`hidden window: ${JSON.stringify(hidden)}`);

  // Animations switch off, then Windows' setting: the finished mark, nothing moving.
  const sw = '#view-settings [data-switch="reducedMotion"]';
  await showView(page, 'settings');
  await page.click(sw);
  await page.waitForFunction(() => document.documentElement.classList.contains('reduce-motion'), null, { timeout: WAIT });
  let s = await logoState(page);
  await page.click(sw);
  await page.waitForFunction(() => !document.documentElement.classList.contains('reduce-motion'), null, { timeout: WAIT });
  if (!logoStill(s)) throw new Error(`logo animates with Animations off: ${JSON.stringify(s)}`);
  await page.emulateMedia({ reducedMotion: 'reduce' });
  // The media query's change event lands on the next frame.
  await page.waitForFunction(() => !document.getElementById('sidebar-logo').classList.contains('is-pulsing'), null, { timeout: WAIT }).catch(() => {});
  s = await logoState(page);
  await page.emulateMedia({ reducedMotion: 'no-preference' });
  if (!logoStill(s)) throw new Error(`logo animates under prefers-reduced-motion: ${JSON.stringify(s)}`);

  // A fresh start with the switch off: no assembly either.
  const fresh = await page.context().newPage();
  try {
    await fresh.addInitScript(() => {
      const saved = JSON.parse(localStorage.getItem('orbit.settings') || '{}') || {};
      localStorage.setItem('orbit.settings', JSON.stringify({ ...saved, reducedMotion: true }));
    });
    await fresh.goto(page.url(), { waitUntil: 'load' });
    await fresh.waitForSelector('#view-processes.is-active, .view.is-active', { timeout: WAIT });
    s = await logoState(fresh);
    if (!logoStill(s)) throw new Error(`logo animates at start with Animations off: ${JSON.stringify(s)}`);
  } finally {
    await fresh.evaluate(() => {
      const saved = JSON.parse(localStorage.getItem('orbit.settings') || '{}') || {};
      localStorage.setItem('orbit.settings', JSON.stringify({ ...saved, reducedMotion: false }));
    }).catch(() => {});
    await fresh.close();
  }

  // Too short for it: not drawn (so not focusable either); the card sits right under the nav.
  const size = page.viewportSize();
  await page.setViewportSize({ width: size.width, height: 620 });
  const short = await page.evaluate(() => {
    const nav = [...document.querySelectorAll('#sidebar .nav-item')].pop().getBoundingClientRect();
    const foot = document.getElementById('mini-readout').getBoundingClientRect();
    return { shown: document.getElementById('sidebar-logo').getClientRects().length > 0, gap: Math.round(foot.top - nav.bottom) };
  });
  await page.setViewportSize(size);
  if (short.shown) throw new Error('logo still drawn in a 620px window');
  if (short.gap !== 12) throw new Error(`card is ${short.gap}px under the nav in a short window, want 12`);

  // Click: Settings at About. Keyboard: Tab goes on to the Live / System card.
  await showView(page, 'processes');
  await page.click('#sidebar-logo');
  await page.waitForSelector('#view-settings.is-active', { timeout: WAIT }).catch(() => {
    throw new Error('clicking the logo does not open Settings');
  });
  const about = await page.evaluate(() => {
    const r = document.querySelector('#view-settings .lv-about').getBoundingClientRect();
    const m = document.getElementById('main').getBoundingClientRect();
    return r.top < m.bottom && r.bottom > m.top;
  });
  if (!about) throw new Error('clicking the logo does not show About');
  await page.focus('#sidebar-logo');
  await page.keyboard.press('Tab');
  if (await page.evaluate(() => document.activeElement?.id) !== 'mini-readout') throw new Error('Tab from the logo does not reach the Live / System card');
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
  const watch = (p) => {
    p.on('console', (msg) => {
      if (msg.type() === 'error') fail(`console error: ${msg.text()}`);
    });
    p.on('pageerror', (err) => fail(`page error: ${err.message}`));
  };
  watch(page);

  console.log(`Orbit UI smoke test (${channel || 'chromium'}) at ${base}`);
  await check('load', async () => {
    await page.goto(base, { waitUntil: 'load' });
    await page.waitForSelector('#view-processes.is-active', { timeout: WAIT });
  });

  for (const theme of THEMES) {
    await check(`${theme}: theme`, () => setTheme(page, theme));
    await check(`${theme}: logo`, () => logoTheme(page, theme));
    for (const view of VIEWS) {
      await check(`${theme}: ${view}`, () => showView(page, view));
    }
    await check(`${theme}: optimize tabs`, () => optimizeTabs(page));
    await check(`${theme}: palette`, () => palette(page));
    await check(`${theme}: details drawer`, () => drawer(page));
    await check(`${theme}: temperatures`, () => sensorsTheme(page));
  }
  await check('gpu adapters', () => gpuAdapters(page));
  await check('temperatures', () => sensors(page, base, watch));
  await check('history menu', () => historyMenu(page));
  await check('defender', () => defender(page, base, watch));
  await check('debloat', () => debloat(page));
  await check('my rig', () => myRig(page));
  await check('motion', () => motion(page));
  await check('logo', () => logo(page));
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
console.log(`\nPassed: ${THEMES.length} themes x ${VIEWS.length} views, ${OPTIMIZE_TABS.length} Optimize tabs, palette and details drawer, temperatures, GPU adapters, History menu, Defender scans, Debloat, My Rig, motion on and off, sidebar logo, no console errors.`);
