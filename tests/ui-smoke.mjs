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
const OPTIMIZE_TABS = ['health', 'cleanup', 'boost', 'rules', 'drivers', 'debloat'];
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
let passed = 0;

function fail(msg) {
  failures.push(`[${step}] ${msg}`);
  console.error(`  FAIL ${msg}`);
}

async function check(label, fn) {
  step = label;
  try {
    await fn();
    passed++;
    console.log(`  ok   ${label}`);
  } catch (err) {
    // The first line, plus a little of Playwright's call log so a timeout says what it waited for.
    fail(String(err?.message || err).split('\n').filter((l) => l.trim()).slice(0, 4).join(' | '));
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

// Per-app rules (0.9): create one from the Processes menu (Realtime is not offered), see its
// badge, then list, edit, switch off and delete it in Optimize > Rules; Orbit, Windows,
// Windows Security and anti-cheat names are refused with a plain reason; the log shows
// applied, blocked and changed-back entries.
async function rules(page) {
  const ANTICHEAT = "Anti-cheat software blocks changes like this. Orbit won't try.";
  const REFUSED = {
    'csrss.exe': "Windows needs this process. Orbit won't change it.",
    'orbit.exe': "Orbit can't change itself.",
    'MsMpEng.exe': "Windows Security protects itself. Orbit won't change it.",
    'vgc.exe': ANTICHEAT,
    'EasyAntiCheat_EOS.exe': ANTICHEAT,
    'BEService_x64.exe': ANTICHEAT,
  };
  await showView(page, 'processes');
  await page.fill('#search', 'Notion.exe');
  const row = page.locator('#view-processes .proc-canvas > .proc-row.row').first();
  await row.waitFor({ timeout: WAIT });
  // The search filters on the next frame: wait until the first row is the one asked for.
  await page.waitForFunction(() => /^Notion\.exe/.test(document.querySelector('#view-processes .proc-canvas > .proc-row.row')?.innerText || ''), null, { timeout: WAIT });
  await row.click({ button: 'right' });
  const labels = await menuLabels(page);
  if (!labels.includes('Always use this priority…')) throw new Error(`Processes menu lacks "Always use this priority…": ${labels.join(', ')}`);
  await page.locator('#context-menu .menu-item', { hasText: 'Always use this priority' }).click();
  await page.waitForSelector('#dialog [data-ru-prio]', { timeout: WAIT });
  const options = await page.locator('#dialog [data-ru-prio] option').allInnerTexts();
  if (options.some((o) => /realtime/i.test(o))) throw new Error(`the priority list offers Realtime: ${options.join(', ')}`);
  for (const want of ['High', 'Above normal', 'Normal', 'Below normal', 'Low']) if (!options.includes(want)) throw new Error(`the priority list lacks ${want}: ${options.join(', ')}`);
  await page.selectOption('#dialog [data-ru-prio]', 'high');
  await page.check('#dialog [data-ru-limit]');
  await page.check('#dialog [data-ru-cpu="0"]');
  await page.check('#dialog [data-ru-cpu="1"]');
  await page.click('#dialog [data-act="ok"]');
  await row.locator('.proc-rule').waitFor({ timeout: WAIT }).catch(async () => {
    throw new Error('the process row shows no rule badge after the rule was made');
  });
  const tip = await row.locator('.proc-rule').getAttribute('title');
  if (!/Rule: High priority, 2 cores\. Applied/.test(tip || '')) throw new Error(`badge tooltip is "${tip}"`);
  await page.fill('#search', '');

  await showView(page, 'optimize');
  await page.click('#view-optimize .op-tabs [data-tab="rules"]');
  await page.waitForFunction(() => /Notion\.exe/.test(document.querySelector('#op-pane-rules')?.innerText || ''), null, { timeout: WAIT }).catch(() => {
    throw new Error('Rules list does not show the new rule');
  });
  const names = await page.locator('#op-pane-rules .ru-rule .ru-name').allInnerTexts();
  for (const want of ['Notion.exe', 'Discord.exe', 'League of Legends.exe', 'steamservice.exe']) {
    if (!names.some((n) => n.includes(want))) throw new Error(`Rules list lacks ${want}: ${names.join(' | ')}`);
  }
  const notion = page.locator('#op-pane-rules .ru-rule', { hasText: 'Notion.exe' });
  const line = (await notion.innerText()).replace(/\s+/g, ' ');
  if (!/Any folder/.test(line) || !/High priority, 2 processors/.test(line) || !/Applied/.test(line)) throw new Error(`Notion.exe rule reads "${line}"`);
  const steam = (await page.locator('#op-pane-rules .ru-rule', { hasText: 'steamservice.exe' }).innerText()).replace(/\s+/g, ' ');
  if (!/Restart Orbit as administrator/.test(steam) || !/Restart as administrator/i.test(steam)) throw new Error(`steamservice.exe rule reads "${steam}"`);

  // Edit: change the priority.
  await notion.locator('[data-ru-edit]').click();
  await page.waitForSelector('#dialog [data-ru-prio]', { timeout: WAIT });
  await page.selectOption('#dialog [data-ru-prio]', 'belowNormal');
  await page.click('#dialog [data-act="ok"]');
  await page.waitForFunction(() => /Below normal priority, 2 processors/.test(document.querySelector('#op-pane-rules')?.innerText || ''), null, { timeout: WAIT }).catch(() => {
    throw new Error('editing a rule does not change the list');
  });
  // Switch off and back on.
  await notion.locator('[data-ru-toggle]').click();
  await page.waitForSelector('#op-pane-rules .ru-rule.is-off', { timeout: WAIT });
  await notion.locator('[data-ru-toggle]').click();
  await page.waitForFunction(() => !document.querySelector('#op-pane-rules .ru-rule.is-off'), null, { timeout: WAIT });

  // Refusals: through the dialog (an app picked from the running list) and the command itself.
  await page.click('#op-pane-rules [data-ru-add]');
  await page.waitForSelector('#dialog [data-ru-app]', { timeout: WAIT });
  const apps = await page.locator('#dialog [data-ru-app] option').allInnerTexts();
  if (apps.includes('csrss.exe') || apps.includes('System')) throw new Error('the app list offers Windows processes');
  const defender = apps.find((a) => /^(MsMpEng|NisSrv)\.exe$/.test(a));
  if (defender) {
    await page.selectOption('#dialog [data-ru-app]', defender);
    await page.waitForFunction(() => /Windows Security protects itself/.test(document.querySelector('#dialog .dialog__error')?.textContent || ''), null, { timeout: WAIT }).catch(() => {
      throw new Error(`${defender} is not refused in the dialog`);
    });
    if (!await page.locator('#dialog [data-act="ok"]').isDisabled()) throw new Error('the dialog lets a refused app be saved');
  }
  await page.click('#dialog [data-act="cancel"]');
  const results = await page.evaluate(async (refused) => {
    const { call } = await import('/js/api.js');
    const out = {};
    const make = async (label, rule) => {
      try {
        await call('rules_create', { rule: { id: '', enabled: true, priority: null, affinity: null, ...rule } });
        out[label] = 'created';
      } catch (err) {
        out[label] = String(err);
      }
    };
    for (const n of refused) await make(n, { match: { kind: 'name', value: n }, priority: 'high' });
    await make('realtime', { match: { kind: 'name', value: 'game.exe' }, priority: 'realtime' });
    await make('nothing', { match: { kind: 'name', value: 'game.exe' } });
    await make('cores', { match: { kind: 'name', value: 'game.exe' }, affinity: '4294967296' });
    await make('none', { match: { kind: 'name', value: 'game.exe' }, affinity: '0' });
    await make('dup', { match: { kind: 'name', value: 'DISCORD.exe' }, priority: 'low' });
    return out;
  }, Object.keys(REFUSED));
  for (const [name, why] of Object.entries(REFUSED)) {
    if (results[name] !== why) throw new Error(`rule for ${name} gave "${results[name]}", wanted "${why}"`);
  }
  if (!/Realtime/.test(results.realtime)) throw new Error(`Realtime rule: ${results.realtime}`);
  if (!/Pick a priority/.test(results.nothing)) throw new Error(`empty rule: ${results.nothing}`);
  if (!/does not have/.test(results.cores) || results.none !== 'Pick at least one processor.') throw new Error(`cores: ${results.cores} / ${results.none}`);
  if (!/already a rule/.test(results.dup)) throw new Error(`duplicate rule: ${results.dup}`);

  // The log: applied, blocked and changed-back entries.
  await page.click('#view-optimize .op-tabs [data-tab="health"]');
  await page.click('#view-optimize .op-tabs [data-tab="rules"]');
  await page.waitForSelector('#op-pane-rules .ru-rule', { timeout: WAIT });
  await page.click('#op-pane-rules .ru-log > summary');
  await page.waitForSelector('#op-pane-rules .ru-entry', { timeout: WAIT });
  const kinds = await page.locator('#op-pane-rules .ru-entry').evaluateAll((els) => els.map((e) => [...e.classList].find((c) => /^ru-entry--/.test(c))));
  for (const k of ['applied', 'blocked', 'reverted']) if (!kinds.includes(`ru-entry--${k}`)) throw new Error(`the log has no "${k}" entry: ${kinds.join(', ')}`);
  const logText = (await page.locator('#op-pane-rules .ru-log').innerText()).replace(/\s+/g, ' ');
  if (!/Kept being changed back by something else/.test(logText) || !/Restart Orbit as administrator/.test(logText)) throw new Error(`log text: ${logText}`);

  // Delete (with a confirmation).
  const before = await page.locator('#op-pane-rules .ru-rule').count();
  await notion.locator('[data-ru-delete]').click();
  await page.waitForSelector('#dialog [data-act="ok"]', { timeout: WAIT });
  await page.click('#dialog [data-act="ok"]');
  await page.waitForFunction((n) => document.querySelectorAll('#op-pane-rules .ru-rule').length === n - 1, before, { timeout: WAIT }).catch(() => {
    throw new Error('deleting a rule leaves it in the list');
  });
  await showView(page, 'processes');
  await page.fill('#search', 'Notion.exe');
  await page.waitForFunction(() => {
    const first = document.querySelector('#view-processes .proc-canvas > .proc-row.row');
    return !!first && /^Notion\.exe/.test(first.innerText) && !first.querySelector('.proc-rule');
  }, null, { timeout: WAIT }).catch(() => {
    throw new Error('the badge stays after the rule was deleted');
  });
  await page.fill('#search', '');
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
// Network > Usage (1.5): totals and the per-app table for a period, then back to Connections.
async function netUsageTheme(page) {
  await showView(page, 'network');
  await page.click('#view-network .net-tabs [data-tab="usage"]');
  await page.waitForFunction(() => /\d\s?(B|KB|MB|GB|TB)$/.test(document.querySelector('#net-pane-usage [data-nu="down"]')?.textContent || ''), null, { timeout: WAIT }).catch(() => {
    throw new Error('Network > Usage shows no download total');
  });
  if (await page.locator('#view-network .view-head .lv-stats').isVisible()) throw new Error('connection counts still shown on the Usage tab');
  await page.click('#net-pane-usage [data-period="month"]');
  await page.waitForSelector('#net-pane-usage .nu-table tbody tr', { timeout: WAIT }).catch(() => {
    throw new Error('Network > Usage > 30 days lists no apps');
  });
  const share = await page.locator('#net-pane-usage .nu-table tbody tr .c-share').first().innerText();
  if (!/%$/.test(share.trim())) throw new Error(`share column: ${share}`);
  // 30 days come from Windows' own record (1.6): its note and a row for traffic tied to no app.
  await page.waitForFunction(() => /Windows' own record/.test(document.querySelector('#net-pane-usage [data-nu-note]')?.innerText || ''), null, { timeout: WAIT }).catch(async () => {
    throw new Error(`30 days note: ${await page.locator('#net-pane-usage [data-nu-note]').innerText()}`);
  });
  if (!(await page.locator('#net-pane-usage .nu-table tbody tr', { hasText: 'Not tied to an app' }).count())) throw new Error('30 days: no "Not tied to an app" row');
  await page.click('#net-pane-usage [data-period="day"]');
  await page.waitForFunction(() => /administrator/.test(document.querySelector('#net-pane-usage [data-nu-note]')?.innerText || ''), null, { timeout: WAIT }).catch(() => {
    throw new Error('no admin note on 24 hours while unelevated');
  });
  await page.click('#view-network .net-tabs [data-tab="connections"]');
  await page.waitForSelector('#net-pane-connections .net-table tbody tr', { timeout: WAIT }).catch(() => {
    throw new Error('Connections table is empty after leaving Usage');
  });
}

// Network > Usage as administrator, with nothing recorded, and when the command fails.
async function netUsage(page, base, watch) {
  const other = await page.context().newPage();
  watch(other);
  const usageTab = async (url) => {
    await other.goto(url, { waitUntil: 'load' });
    await showView(other, 'network');
    await other.click('#view-network .net-tabs [data-tab="usage"]');
  };
  try {
    await usageTab(base);
    await other.click('#net-pane-usage [data-nu-act="admin"]');
    await other.click('#net-pane-usage [data-period="hour"]');
    await other.waitForSelector('#net-pane-usage .nu-table tbody tr', { timeout: WAIT }).catch(() => {
      throw new Error('as admin, the last hour lists no apps');
    });
    await other.waitForFunction(() => /only while it runs/.test(document.querySelector('#net-pane-usage [data-nu-note]')?.innerText || ''), null, { timeout: WAIT }).catch(async () => {
      throw new Error(`admin note: ${await other.locator('#net-pane-usage [data-nu-note]').innerText()}`);
    });
    const totals = await other.locator('#net-pane-usage .nu-table tbody tr .nu-total-val').allInnerTexts();
    if (totals.length < 2) throw new Error('fewer than two apps in the last hour');

    await usageTab(`${base}?nonetusage`);
    await other.waitForFunction(() => /No per-app numbers/.test(document.querySelector('#net-pane-usage .lv-empty')?.innerText || ''), null, { timeout: WAIT }).catch(() => {
      throw new Error('?nonetusage: no empty state');
    });
    const down = await other.locator('#net-pane-usage [data-nu="down"]').innerText();
    if (down.trim() !== '0 B') throw new Error(`?nonetusage download: ${down}`);

    await usageTab(`${base}?netusagefail`);
    await other.locator('#net-pane-usage .lv-empty [data-nu-act="retry"]').waitFor({ timeout: WAIT });

    // Windows' record unreadable: 30 days fall back to Orbit's own counts and say so.
    await usageTab(`${base}?nowinrecord`);
    await other.click('#net-pane-usage [data-period="month"]');
    await other.waitForFunction(() => /administrator/.test(document.querySelector('#net-pane-usage [data-nu-note]')?.innerText || ''), null, { timeout: WAIT }).catch(async () => {
      throw new Error(`?nowinrecord 30 days note: ${await other.locator('#net-pane-usage [data-nu-note]').innerText()}`);
    });
    if (await other.locator('#net-pane-usage .nu-table tbody tr', { hasText: 'Not tied to an app' }).count()) throw new Error('?nowinrecord still shows Windows rows');
  } finally {
    // The tab choice is remembered per origin: leave the next page on Connections.
    await other.evaluate(() => localStorage.setItem('orbit.networkTab', 'connections')).catch(() => {});
    await other.close();
  }
}

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
// History rebuilds its table on show, when the data loads and when app icons arrive, so a
// click on a slow machine can land on a row that was just replaced. Wait for the load, then
// right-click in short attempts.
async function historySettled(page) {
  await page.waitForFunction(() => !document.querySelector('#view-history [data-refresh].is-busy'), null, { timeout: WAIT });
  await page.waitForTimeout(400);
}

async function rightClickRow(page, row, what) {
  let last;
  for (let i = 0; i < 5; i++) {
    try {
      await row.click({ button: 'right', timeout: 3000 });
      return;
    } catch (err) {
      last = err;
      await page.keyboard.press('Escape').catch(() => {});
      await page.waitForTimeout(300);
    }
  }
  throw new Error(`could not right-click ${what}: ${String(last?.message || last).split('\n')[0]}`);
}

async function historyMenu(page) {
  await showView(page, 'history');
  const chrome = page.locator('#view-history tr.row[data-key$="\\\\chrome.exe"]');
  await chrome.waitFor({ timeout: WAIT });
  await historySettled(page);
  await rightClickRow(page, chrome, 'the chrome.exe row');
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
  await rightClickRow(page, row, `the ${key} row`);
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
  // "running" shows before the (mock) backend has the scan, and mock calls take 25-95 ms, so
  // a Cancel sent at once can overtake defender_scan and find nothing to cancel. Wait for the
  // backend's first elapsed tick.
  await page.waitForFunction((sel) => /^0:0[1-9]$/.test(document.querySelector(`${sel} [data-df-elapsed]`)?.textContent || ''), panel, { timeout: WAIT }).catch(() => {
    throw new Error('full scan shows no elapsed time');
  });
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

// Startup suggestions card (0.8): the Hide button (labelled) hides it in Startup and in
// Health, the choice survives a reload, and a changed set of apps brings the card back.
// Per-app rules, part 2 (0.9): the master switch (badges go away), "only while in front"
// through the dialog, Reset (the app back to Normal, the rule off), the new refusals, a
// game's own rule from Boost (only while Boost is on), the backup round trip (Export writes
// the rules, Import through the file input adds them after a confirm), and efficiency mode,
// offered only on Windows 11 (?win11), never on the Windows 10 preview.
async function rulesMore(page, base, watch) {
  const pane = '#op-pane-rules';
  const openRules = async () => {
    await showView(page, 'optimize');
    await page.click('#view-optimize .op-tabs [data-tab="rules"]');
    await page.waitForSelector(`${pane} .ru-rule`, { timeout: WAIT });
  };
  const paneText = () => page.locator(pane).innerText().then((t) => t.replace(/\s+/g, ' '));
  const waitText = (re, why) => page.waitForFunction(([sel, src]) => new RegExp(src).test((document.querySelector(sel)?.innerText || '').replace(/\s+/g, ' ')), [pane, re.source], { timeout: WAIT }).catch(async () => {
    throw new Error(`${why}: ${await paneText()}`);
  });
  const discordBadge = async (want) => {
    await showView(page, 'processes');
    await page.fill('#search', 'Discord.exe');
    await page.waitForFunction((on) => {
      const first = document.querySelector('#view-processes .proc-canvas > .proc-row.row');
      return !!first && /^Discord\.exe/.test(first.innerText) && !!first.querySelector('.proc-rule') === on;
    }, want, { timeout: WAIT }).catch(() => {
      throw new Error(`Discord.exe badge should be ${want ? 'shown' : 'gone'}`);
    });
    await page.fill('#search', '');
  };

  await openRules();
  if (!/League of Legends\.exe.*High priority in front, Normal behind/.test(await paneText())) throw new Error(`in-front rule summary: ${await paneText()}`);

  // Master switch: off hides every badge and says so; on brings them back.
  await discordBadge(true);
  await openRules();
  const master = page.locator(`${pane} [data-ru-master]`);
  await master.click();
  await page.waitForSelector(`${pane} [data-ru-off]:not([hidden])`, { timeout: WAIT });
  if (await master.getAttribute('aria-checked') !== 'false') throw new Error('master switch did not turn off');
  await discordBadge(false);
  await openRules();
  await master.click();
  await page.waitForSelector(`${pane} [data-ru-off][hidden]`, { state: 'attached', timeout: WAIT });
  await discordBadge(true);

  // Only while in front, through the dialog. Windows 10: no efficiency mode.
  await openRules();
  const discord = page.locator(`${pane} .ru-rule`, { hasText: 'Discord.exe' });
  await discord.locator('[data-ru-edit]').click();
  await page.waitForSelector('#dialog [data-ru-front]', { timeout: WAIT });
  if (await page.locator('#dialog [data-ru-eco]').count()) throw new Error('efficiency mode is offered on Windows 10');
  if (!await page.locator('#dialog [data-ru-behind-field]').isHidden()) throw new Error('"When it\'s behind" shows before "Only while in front" is ticked');
  if (!await page.locator('#dialog [data-ru-boost]').count()) throw new Error('the dialog has no "Only while Boost is on"');
  await page.selectOption('#dialog [data-ru-prio]', 'high');
  await page.check('#dialog [data-ru-front]');
  await page.locator('#dialog [data-ru-behind-field]').waitFor({ state: 'visible', timeout: WAIT });
  const behind = await page.locator('#dialog [data-ru-behind] option').allInnerTexts();
  if (behind.join('|') !== 'Normal|Below normal') throw new Error(`"When it's behind" offers ${behind.join(', ')}`);
  await page.selectOption('#dialog [data-ru-behind]', 'belowNormal');
  await page.click('#dialog [data-act="ok"]');
  await waitText(/Discord\.exe.*High priority in front, Below normal behind/, 'editing an in-front rule');

  // Reset: Discord back to Normal, rule off, a "Put back" line in the log.
  await discord.locator('[data-ru-reset]').click();
  await page.waitForSelector('#dialog [data-act="ok"]', { timeout: WAIT });
  const ask = (await page.locator('#dialog').innerText()).replace(/\s+/g, ' ');
  if (!/goes back to Normal priority/.test(ask) || !/switches off/.test(ask)) throw new Error(`Reset asks "${ask}"`);
  await page.click('#dialog [data-act="ok"]');
  await page.waitForFunction((sel) => [...document.querySelectorAll(`${sel} .ru-rule.is-off`)].some((li) => /Discord\.exe/.test(li.innerText)), pane, { timeout: WAIT }).catch(() => {
    throw new Error('Reset did not switch the rule off');
  });
  const prios = await page.evaluate(async () => {
    const { call } = await import('/js/api.js');
    const { store } = await import('/js/store.js');
    const rows = store.snapshot.processes.filter((p) => p.name === 'Discord.exe');
    return Promise.all(rows.map((r) => call('get_priority', { pid: r.pid })));
  });
  if (!prios.length || prios.some((p) => p !== 'normal')) throw new Error(`after Reset Discord.exe runs at ${prios.join(', ')}`);
  await page.evaluate((sel) => { document.querySelector(`${sel} .ru-log`).open = true; }, pane);
  await page.waitForSelector(`${pane} .ru-entry--reset`, { timeout: WAIT }).catch(() => {
    throw new Error('the log has no "Put back" line after Reset');
  });
  if (!/Put back to Normal priority\. The rule is off\./.test(await page.locator(`${pane} .ru-log`).innerText())) throw new Error('Reset log line');
  // The Processes menu opens that switched-off rule rather than adding a second one.
  await showView(page, 'processes');
  await page.fill('#search', 'Discord.exe');
  await page.waitForFunction(() => /^Discord\.exe/.test(document.querySelector('#view-processes .proc-canvas > .proc-row.row')?.innerText || ''), null, { timeout: WAIT });
  await page.locator('#view-processes .proc-canvas > .proc-row.row').first().click({ button: 'right' });
  await menuLabels(page);
  await page.locator('#context-menu .menu-item', { hasText: 'Always use this priority' }).click();
  await page.waitForSelector('#dialog [data-ru-prio]', { timeout: WAIT });
  // The title can fill in a moment after the controls on a slow runner.
  await page.waitForFunction(() => /Discord/.test(document.querySelector('#dialog')?.innerText || ''), null, { timeout: WAIT }).catch(() => {});
  const editTitle = (await page.locator('#dialog').innerText()).split('\n').find((l) => /Discord/.test(l)) || '';
  if (!/^Rule for Discord\.exe/.test(editTitle.trim())) throw new Error(`the menu on a switched-off rule opens "${editTitle}"`);
  await page.click('#dialog [data-act="cancel"]');
  await page.fill('#search', '');
  await openRules();

  // The new refusals, from the command.
  const results = await page.evaluate(async () => {
    const { call } = await import('/js/api.js');
    const out = {};
    const make = async (label, rule) => {
      try {
        await call('rules_create', { rule: { id: '', enabled: true, priority: null, affinity: null, match: { kind: 'name', value: 'newgame.exe' }, ...rule } });
        out[label] = 'created';
      } catch (err) {
        out[label] = String(err);
      }
    };
    await make('eco', { efficiency: true });
    await make('front', { inFront: true });
    await make('behindRt', { priority: 'high', inFront: true, behind: 'realtime' });
    await make('behindSame', { priority: 'normal', inFront: true, behind: 'normal' });
    await make('shipped', { match: { kind: 'name', value: 'cmd.exe' }, priority: 'high' });
    await make('defenderDir', { match: { kind: 'path', value: 'C:\\ProgramData\\Microsoft\\Windows Defender\\Platform\\4.18\\MsMpEngCP.exe' }, priority: 'low' });
    return out;
  });
  const want = {
    eco: 'Efficiency mode needs Windows 11.',
    front: 'Pick a priority to use while the app is in front.',
    behindRt: 'Orbit never sets Realtime. It can freeze Windows.',
    behindSame: 'Pick a different priority for when the app is in front.',
    defenderDir: "Windows Security protects itself. Orbit won't change it.",
    shipped: "Windows needs this process. Orbit won't change it.",
  };
  for (const [k, v] of Object.entries(want)) if (results[k] !== v) throw new Error(`${k}: "${results[k]}", wanted "${v}"`);

  // Boost: a game's own rule, only while Boost is on.
  await page.click('#view-optimize .op-tabs [data-tab="boost"]');
  await page.click('#op-pane-boost [data-bo-browse]');
  const chip = page.locator('#op-pane-boost [data-bo-rule="hollow_knight.exe"]');
  await chip.waitFor({ timeout: WAIT }).catch(() => {
    throw new Error('a game in Boost has no rule button');
  });
  const chipText = (await chip.textContent()).trim();
  if (chipText !== 'Add rule') throw new Error(`game rule button reads "${chipText}"`);
  await chip.click();
  await page.waitForSelector('#dialog [data-ru-boost]', { timeout: WAIT });
  if (!await page.locator('#dialog [data-ru-boost]').isChecked()) throw new Error('a rule from Boost is not "only while Boost is on"');
  const title = await page.locator('#dialog').innerText();
  if (!/hollow_knight\.exe/.test(title)) throw new Error(`Boost's rule dialog is for "${title.split('\n')[0]}"`);
  await page.click('#dialog [data-act="ok"]');
  await page.waitForFunction(() => /^Rule$/.test(document.querySelector('#op-pane-boost [data-bo-rule="hollow_knight.exe"]')?.textContent.trim() || ''), null, { timeout: WAIT }).catch(() => {
    throw new Error('the game shows no rule after one was added');
  });
  const tip = await chip.getAttribute('title');
  if (!/High priority, while Boost is on/.test(tip || '')) throw new Error(`game rule tooltip: ${tip}`);
  await openRules();
  await waitText(/hollow_knight\.exe.*High priority, while Boost is on.*Waiting for Boost/, 'Boost rule in the Rules list');
  await page.click('#view-optimize .op-tabs [data-tab="boost"]');
  await page.click('#op-pane-boost [data-bo-remove="games"][data-name="hollow_knight.exe"]');

  // Backup: Export carries the rules; Import adds new ones after listing them.
  const exported = await page.evaluate(async () => (await import('/js/panels/backup.js')).exportData());
  if (!Array.isArray(exported.rules?.rules) || !exported.rules.rules.some((r) => r.match.value === 'hollow_knight.exe') || exported.rules.rulesEnabled !== true) {
    throw new Error(`Export has no rules: ${JSON.stringify(exported.rules).slice(0, 120)}`);
  }
  const file = {
    theme: exported.theme,
    rules: {
      rulesEnabled: true,
      rules: [
        { match: { kind: 'name', value: 'imported.exe' }, priority: 'aboveNormal', enabled: true },
        { match: { kind: 'name', value: 'csrss.exe' }, priority: 'high', enabled: true },
        exported.rules.rules[0],
      ],
    },
  };
  await showView(page, 'settings');
  await page.locator('[data-backup-file]').setInputFiles({ name: 'orbit-settings.json', mimeType: 'application/json', buffer: Buffer.from(JSON.stringify(file)) });
  await page.waitForSelector('#dialog [data-act="ok"]', { timeout: WAIT }).catch(() => {
    throw new Error('importing a file with a new rule asks nothing');
  });
  const list = (await page.locator('#dialog').innerText()).replace(/\s+/g, ' ');
  if (!/Per-app rules: 1 new/.test(list) || !/csrss\.exe: Windows needs this process/.test(list)) throw new Error(`Import lists "${list}"`);
  await page.click('#dialog [data-act="ok"]');
  await openRules();
  await waitText(/imported\.exe.*Above normal priority/, 'the imported rule');
  if (/csrss\.exe/.test(await paneText())) throw new Error('a refused rule was imported');

  // Efficiency mode on Windows 11 only.
  const other = await page.context().newPage();
  watch(other);
  try {
    await other.goto(`${base}?win11`, { waitUntil: 'load' });
    await other.click('#sidebar .nav-item[data-view="optimize"]');
    await other.click('#view-optimize .op-tabs [data-tab="rules"]');
    await other.click(`${pane} [data-ru-add]`);
    await other.waitForSelector('#dialog [data-ru-eco]', { timeout: WAIT }).catch(() => {
      throw new Error('?win11: no efficiency mode in the dialog');
    });
    await other.selectOption('#dialog [data-ru-app]', 'Notion.exe');
    await other.check('#dialog [data-ru-eco]');
    if (!await other.locator('#dialog [data-ru-prio]').isDisabled() || !await other.locator('#dialog [data-ru-front]').isDisabled()) {
      throw new Error('?win11: efficiency mode leaves the priority or "in front" editable');
    }
    await other.click('#dialog [data-act="ok"]');
    await other.waitForFunction((sel) => /Notion\.exe.*Efficiency mode/.test((document.querySelector(sel)?.innerText || '').replace(/\s+/g, ' ')), pane, { timeout: WAIT }).catch(() => {
      throw new Error('?win11: the efficiency rule is not listed');
    });
  } finally {
    await other.close();
  }
}

async function suggestionsHide(page, base) {
  const card = '#view-startup .sg';
  const rows = () => page.locator(`${card} .sg-row`).count();
  await page.evaluate(() => import('/js/store.js').then((m) => m.setSettings({ hiddenSuggestions: '' })));
  await showView(page, 'startup');
  await page.waitForSelector(`${card}:not([hidden]) .sg-row`, { timeout: WAIT });
  const n = await rows();
  if (n < 2) throw new Error(`expected at least 2 suggestions, got ${n}`);
  const hide = page.locator(`${card} [data-sg-hide]`);
  const label = await hide.getAttribute('aria-label');
  if (!/hide/i.test(label || '')) throw new Error(`Hide button has no accessible label: ${label}`);
  await hide.click();
  await page.waitForFunction((sel) => document.querySelector(sel)?.hidden === true, card, { timeout: WAIT }).catch(() => {
    throw new Error('Hide did not hide the card in Startup');
  });
  // Health's compact card follows (same set of apps).
  await showView(page, 'optimize');
  await page.click('#view-optimize .op-tabs [data-tab="health"]');
  await page.waitForSelector('#op-pane-health .hl-item', { timeout: WAIT });
  if (await page.locator('#op-pane-health .sg:not([hidden])').count()) throw new Error('Health still shows the card after Hide');
  // It stays hidden after a reload.
  await page.goto(base, { waitUntil: 'load' });
  await showView(page, 'startup');
  await page.waitForFunction(() => document.querySelectorAll('#view-startup tbody tr.row').length > 0, null, { timeout: WAIT });
  await page.waitForTimeout(400);
  if (await page.locator(`${card}:not([hidden])`).count()) throw new Error('card came back after a reload with the same apps');
  // A changed set (one suggested app turned off elsewhere) brings it back, without the app that was turned off.
  const gone = await page.evaluate(async () => {
    const { call } = await import('/js/api.js');
    const list = await call('get_startup_suggestions');
    await call('set_startup_enabled', { id: list[0].id, enabled: false });
    return { id: list[0].id, count: list.length };
  });
  await page.click('#view-startup [data-refresh]');
  await page.waitForFunction((sel) => document.querySelector(sel)?.hidden === false, card, { timeout: WAIT }).catch(() => {
    throw new Error('card did not return after the suggested apps changed');
  });
  const ids = await page.locator(`${card} .sg-row [data-id]`).evaluateAll((els) => els.map((e) => e.dataset.id));
  if (!ids.length || ids.includes(gone.id)) throw new Error(`suggested after the change: ${ids.join(', ')}`);
  // Put the mock back.
  await page.evaluate(async (id) => {
    const { call } = await import('/js/api.js');
    await call('set_startup_enabled', { id, enabled: true });
    (await import('/js/store.js')).setSettings({ hiddenSuggestions: '' });
  }, gone.id);
}

// Startup (0.8): Store app startup tasks are listed read only with their state and an "Open
// Startup settings" button; the list is re-read when the tab is shown again and when the
// backend announces orbit://startup-changed (the mock sends it from a test hook).
async function startupSync(page) {
  const sid = (...parts) => parts.join(String.fromCharCode(92)); // backslash-joined ids
  const rows = '#view-startup tbody tr.row';
  const row = (name) => page.locator(rows, { hasText: name }).first();
  const api = (fn, arg) => page.evaluate(async ([f, a]) => {
    const { call } = await import('/js/api.js');
    return call(f, a);
  }, [fn, arg]);
  const switchOf = (name) => row(name).locator('.switch').getAttribute('aria-checked');
  const waitSwitch = (name, value, what) => page.waitForFunction(([n, v]) => {
    const tr = [...document.querySelectorAll('#view-startup tbody tr.row')].find((r) => r.textContent.includes(n));
    return tr?.querySelector('.switch')?.getAttribute('aria-checked') === v;
  }, [name, value], { timeout: WAIT }).catch(() => {
    throw new Error(what);
  });

  await showView(page, 'startup');
  await page.waitForFunction(() => document.querySelectorAll('#view-startup tbody tr.row').length > 3, null, { timeout: WAIT });

  // Packaged rows: read only, labelled "Store app", state in words, Settings button.
  const packaged = page.locator('#view-startup tbody tr.row--packaged');
  if (await packaged.count() !== 3) throw new Error('expected 3 Store app rows, got ' + await packaged.count());
  const text = (await packaged.allInnerTexts()).join(' | ').replace(/\s+/g, ' ');
  for (const need of ['WhatsApp', 'Claude', 'Microsoft 365 Copilot', 'Store app', 'Disabled by you', 'Open Startup settings']) {
    if (!text.toLowerCase().includes(need.toLowerCase())) throw new Error('Store app rows miss "' + need + '": ' + text);
  }
  if (await page.locator('#view-startup tr.row--packaged .switch[data-act="toggle"]').count()) throw new Error('a Store app row has a working switch');
  const locked = page.locator('#view-startup tr.row--packaged .switch');
  for (let i = 0; i < 3; i++) {
    if (await locked.nth(i).getAttribute('aria-disabled') !== 'true') throw new Error('a Store app switch is not marked disabled');
  }
  const settingsPage = [];
  const onConsole = (m) => { if (m.type() === 'info' && /ms-settings:startupapps/.test(m.text())) settingsPage.push(m.text()); };
  page.on('console', onConsole);
  await row('WhatsApp').locator('[data-act="settings"]').click();
  await page.waitForFunction(() => true);
  await page.waitForTimeout(300);
  page.off('console', onConsole);
  if (!settingsPage.length) throw new Error('"Open Startup settings" did not open ms-settings:startupapps');
  // Clicking the locked switch explains instead of changing anything.
  await row('Claude').locator('.switch').click({ force: true }); // aria-disabled: Playwright would wait forever
  await page.waitForFunction(() => /Startup settings/.test(document.getElementById('toasts')?.innerText || ''), null, { timeout: WAIT }).catch(() => {
    throw new Error('the locked switch did not explain itself');
  });
  if (await switchOf('Claude') !== 'true') throw new Error('the locked switch changed');
  // Right-click: only Settings and Search online.
  // The last row sits at the bottom of the table: scroll it into view and let the scroll events
  // finish first (a late scroll event closes an open menu); click its name cell, because the toast
  // from the click above can cover the middle of the row.
  await row('WhatsApp').scrollIntoViewIfNeeded();
  await page.waitForTimeout(400);
  await row('WhatsApp').locator('.c-name').click({ button: 'right' });
  const labels = await menuLabels(page);
  if (labels.join('|') !== 'Open Startup settings|Search online') throw new Error('Store app menu: ' + labels.join(', '));
  await page.keyboard.press('Escape');
  // The backend refuses a change too.
  const refused = await page.evaluate(async (id) => {
    const { call } = await import('/js/api.js');
    try {
      await call('set_startup_enabled', { id, enabled: false });
      return '';
    } catch (e) {
      return String(e);
    }
  }, sid('Packaged', 'Claude_pzs8sxrjxfjjc', 'ClaudeStartup'));
  if (!/Startup settings/.test(refused)) throw new Error('set_startup_enabled did not refuse a Store app: ' + refused);

  // Re-read on show: a change made while the tab was hidden is there when it is opened.
  await showView(page, 'processes');
  await api('set_startup_enabled', { id: sid('HKCU', 'Run', 'Steam'), enabled: true });
  await showView(page, 'startup');
  await waitSwitch('Steam', 'true', 'Startup did not re-read when the tab was shown again');

  // Live: the event reloads the visible list, packaged rows included.
  await api('mock_startup_external_change', { id: sid('HKCU', 'Run', 'Steam'), enabled: false });
  await waitSwitch('Steam', 'false', 'Startup did not update on orbit://startup-changed');
  await api('mock_startup_external_change', { id: sid('Packaged', '5319275A.WhatsAppDesktop_cv1g1gvanyjgm', '2defd21c-0b9e-4e4e-873a-2a68c47d7da5'), enabled: true, packagedState: 'enabled' });
  await waitSwitch('WhatsApp', 'true', 'a Store app row did not update on orbit://startup-changed');
  const status = await row('WhatsApp').locator('.lv-status').innerText();
  if (status.trim() !== 'Enabled') throw new Error('WhatsApp status: ' + status);
  await api('mock_startup_external_change', { id: sid('Packaged', '5319275A.WhatsAppDesktop_cv1g1gvanyjgm', '2defd21c-0b9e-4e4e-873a-2a68c47d7da5'), enabled: false, packagedState: 'disabledByPolicy' });
  await page.waitForFunction(() => [...document.querySelectorAll('#view-startup tbody tr.row')].find((r) => r.textContent.includes('WhatsApp'))?.textContent.includes('Disabled by policy'), null, { timeout: WAIT }).catch(() => {
    throw new Error('"Disabled by policy" did not show');
  });
  await api('mock_startup_external_change', { id: sid('Packaged', '5319275A.WhatsAppDesktop_cv1g1gvanyjgm', '2defd21c-0b9e-4e4e-873a-2a68c47d7da5'), enabled: false, packagedState: 'disabled' });

  // Health's startup count and the suggestions never include Store app rows.
  await showView(page, 'optimize');
  await page.click('#view-optimize .op-tabs [data-tab="health"]');
  await page.waitForSelector('#op-pane-health .hl-item', { timeout: WAIT });
  const ids = await page.evaluate(async () => {
    const { call } = await import('/js/api.js');
    return (await call('get_startup_suggestions')).map((x) => x.id);
  });
  if (ids.some((id) => id.startsWith('Packaged'))) throw new Error('a Store app was suggested: ' + ids.join(', '));
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
      btnTransition: getComputedStyle(btn).transitionDuration,
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
  && s.entering === 0 && s.counting === '32 GB';

async function motion(page) {
  const sw = '#view-settings [data-switch="reducedMotion"]';
  await showView(page, 'settings');
  if (await page.getAttribute(sw, 'aria-checked') !== 'true') throw new Error('Animations switch is not on by default');
  if (await page.evaluate(() => document.documentElement.classList.contains('reduce-motion'))) throw new Error('reduce-motion is set by default');

  // On: buttons and cards transition, the view's blocks rise in, numbers count from zero.
  let s = await motionState(page, 'rig');
  if (!/0\.25s/.test(s.btnTransition) || !/0\.25s/.test(s.cardTransition)) throw new Error(`no hover transition: button ${s.btnTransition}, card ${s.cardTransition}`);
  if (!s.entering || !/^orbit-enter 0\.45s/.test(s.enterAnim)) throw new Error(`view switch did not animate: ${s.entering} blocks, "${s.enterAnim}"`);
  if (s.counting === '32 GB') throw new Error('countUp did not start from zero');
  const hidden = await settledOpacity(page);
  if (hidden) throw new Error(`${hidden} block(s) not at opacity 1 after the entrance`);

  // Settings switch off: every transition, entrance and count-up stops.
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
    // A mock tick already in flight can land between setting a load and reading the rate,
    // so set it again until the rate settles (at most about a second).
    const at = async (usage) => {
      for (let i = 0; i < 20; i += 1) {
        logoTick({ cpu: { usage } });
        await anim.ready;
        if (anim.playbackRate === pulseRate(usage)) break;
        await new Promise((r) => setTimeout(r, 50));
      }
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

// Watchlist (1.1): a process pinned from its own row stays pinned when it ends and starts again
// with a new pid, and Settings shows it as not running, then running.
async function watchRestart(page) {
  const notepads = () => page.evaluate(async () => {
    const { call } = await import('/js/api.js');
    return (await call('get_snapshot')).processes.filter((r) => r.name === 'notepad.exe').map((r) => ({ pid: r.pid }));
  });
  const endAll = () => page.evaluate(async () => {
    const { call } = await import('/js/api.js');
    for (const r of (await call('get_snapshot')).processes.filter((x) => x.name === 'notepad.exe')) {
      await call('end_task', { pid: r.pid, startTime: r.startTime });
    }
  });
  const start = () => page.evaluate(async () => {
    const { call } = await import('/js/api.js');
    await call('run_task', { command: 'notepad', asAdmin: false });
  });
  const watchItem = () => page.evaluate(() => {
    const li = [...document.querySelectorAll('#view-settings [data-watch] .lv-watch-item')].find((el) => el.textContent.includes('notepad.exe'));
    return li ? li.textContent.replace(/\s+/g, ' ').trim() : '';
  });

  await endAll();
  await start();
  await showView(page, 'processes');
  await page.fill('#search', 'notepad.exe');
  await page.waitForFunction(() => {
    // Hidden rows are the list's spare pool, not results.
    const rows = [...document.querySelectorAll('#view-processes .proc-canvas > .proc-row.row:not([hidden])')];
    return rows.length === 1 && rows[0].textContent.includes('notepad.exe');
  }, null, { timeout: WAIT }).catch(() => {
    throw new Error('Processes shows no single notepad.exe row');
  });
  await page.locator('#view-processes .proc-canvas > .proc-row.row:not([hidden])', { hasText: 'notepad.exe' }).click({ button: 'right' });
  const labels = await menuLabels(page);
  if (!labels.includes('Pin to watchlist')) throw new Error(`notepad.exe menu lacks "Pin to watchlist": ${labels.join(', ')}`);
  await page.locator('#context-menu .menu-item', { hasText: 'Pin to watchlist' }).click();
  await page.fill('#search', '');
  const saved = await page.evaluate(async () => (await import('/js/store.js')).store.settings);
  if (saved.watchlist.length || !saved.watchNames.includes('notepad.exe')) throw new Error(`Pin to watchlist did not save a name pin: ${JSON.stringify({ watchlist: saved.watchlist, watchNames: saved.watchNames })}`);

  const first = await notepads();
  await endAll();
  await showView(page, 'settings');
  await page.waitForFunction(() => [...document.querySelectorAll('#view-settings [data-watch] .lv-watch-item')]
    .some((el) => el.textContent.includes('notepad.exe') && el.textContent.includes('Not running')), null, { timeout: WAIT }).catch(async () => {
    throw new Error(`Settings watchlist after notepad.exe ended: "${await watchItem()}"`);
  });

  await start();
  await page.waitForFunction(() => [...document.querySelectorAll('#view-settings [data-watch] .lv-watch-item')]
    .some((el) => el.textContent.includes('notepad.exe') && el.textContent.includes('Running') && !el.textContent.includes('Not running')), null, { timeout: WAIT }).catch(async () => {
    throw new Error(`Settings watchlist after notepad.exe started again: "${await watchItem()}"`);
  });
  const second = await notepads();
  if (!second.length || second[0].pid === first[0].pid) throw new Error('the restarted notepad.exe kept its old pid, so the test proves nothing');
  const pinned = await page.evaluate(async (pid) => {
    const { isPinned } = await import('/js/views/processes.js');
    const { store } = await import('/js/store.js');
    const row = (store.snapshot?.processes || []).find((r) => r.pid === pid);
    return !!row && isPinned(row);
  }, second[0].pid);
  if (!pinned) throw new Error('the restarted notepad.exe is not on the watchlist');

  // Leave the watchlist as it was.
  await page.locator('#view-settings [data-remove-key="name:notepad.exe"]').click();
  await endAll();

  // A pid pin saved by an older version becomes a name pin while its process runs.
  await page.evaluate(async () => {
    const { store, setSettings } = await import('/js/store.js');
    const ex = store.snapshot.processes.find((r) => r.name === 'explorer.exe');
    setSettings({ watchlist: [{ pid: ex.pid, start: ex.startTime }] });
  });
  await showView(page, 'processes');
  await page.waitForFunction(async () => {
    const { store } = await import('/js/store.js');
    const s = store.settings;
    return s.watchlist.length === 0 && s.watchNames.includes('explorer.exe');
  }, null, { timeout: WAIT }).catch(async () => {
    const s = await page.evaluate(async () => (await import('/js/store.js')).store.settings);
    throw new Error(`old explorer.exe pid pin was not moved to a name pin: ${JSON.stringify({ watchlist: s.watchlist, watchNames: s.watchNames })}`);
  });
  await page.evaluate(async () => {
    const { store, setSettings } = await import('/js/store.js');
    setSettings({ watchNames: store.settings.watchNames.filter((n) => n !== 'explorer.exe') });
  });
}

// Services on svchost rows (1.2, 14a): a process that hosts services names them under its row,
// the drawer lists them, and "Go to service" opens Services with that service marked.
async function svchostServices(page) {
  const host = await page.evaluate(async () => {
    const { call } = await import('/js/api.js');
    const map = await call('process_services');
    const [pid, list] = Object.entries(map).sort((a, b) => b[1].length - a[1].length)[0] || [];
    return pid ? { pid: Number(pid), count: list.length, first: list[0] } : null;
  });
  if (!host || host.count < 2) throw new Error('the mock process_services lists no host with two services');

  await showView(page, 'processes');
  await page.fill('#search', 'svchost.exe');
  await page.waitForFunction((name) => [...document.querySelectorAll('#view-processes .proc-canvas > .proc-row.row:not([hidden]) .proc-meta')]
    .some((el) => el.textContent.includes(name)), host.first.displayName, { timeout: WAIT }).catch(() => {
    throw new Error(`no svchost.exe row names "${host.first.displayName}" under it`);
  });
  await page.fill('#search', '');

  await page.evaluate(async (pid) => (await import('/js/store.js')).selectProcess(pid), host.pid);
  await page.waitForSelector('#details.is-open .dr-svc', { timeout: WAIT }).catch(() => {
    throw new Error('the details drawer lists no services for the host');
  });
  const shown = await page.locator('#details .dr-svc').count();
  if (shown !== host.count) throw new Error(`drawer lists ${shown} services, expected ${host.count}`);

  await page.locator(`#details [data-svc="${host.first.name}"]`).click();
  await page.waitForSelector('#view-services.is-active', { timeout: WAIT });
  await page.waitForSelector(`#view-services tr.is-target[data-name="${host.first.name}"]`, { timeout: WAIT }).catch(() => {
    throw new Error(`Go to service did not mark ${host.first.name} in Services`);
  });
}

// Cleanup > Developer leftovers (14b, 14c): projects listed with nothing ticked, Move to hold
// sends ids and the folder shows as held, Restore brings it back, Add folder adds a picked one.
async function devLeftovers(page) {
  await showView(page, 'optimize');
  await page.click('#view-optimize .op-tabs [data-tab="cleanup"]');
  const pane = '#op-pane-cleanup [data-dl]';
  await page.waitForSelector(`${pane} [data-dl-pick]`, { timeout: WAIT }).catch(() => {
    throw new Error('Developer leftovers lists no projects');
  });
  if (await page.locator(`${pane} [data-dl-pick]:checked`).count()) throw new Error('a project is ticked by default');
  const heldBefore = await page.locator(`${pane} [data-dl-restore]`).count();
  await page.locator(`${pane} [data-dl-pick]`).first().check();
  await page.click(`${pane} [data-dl-hold]`);
  await page.click('#dialog [data-act="ok"]');
  await page.waitForFunction(([sel, n]) => document.querySelectorAll(`${sel} [data-dl-restore]`).length === n + 1, [pane, heldBefore], { timeout: WAIT }).catch(() => {
    throw new Error('Move to hold does not list the folder as held');
  });
  await page.locator(`${pane} [data-dl-restore]`).last().click();
  await page.waitForFunction(([sel, n]) => document.querySelectorAll(`${sel} [data-dl-restore]`).length === n, [pane, heldBefore], { timeout: WAIT }).catch(() => {
    throw new Error('Restore does not take the folder off the held list');
  });
  await page.click(`${pane} [data-dl-add]`);
  await page.waitForFunction((sel) => /D:\\Projects/.test(document.querySelector(sel)?.innerText || ''), pane, { timeout: WAIT }).catch(() => {
    throw new Error('Add folder does not show the picked folder');
  });
}

// Biggest folders > Map: measure C:\, switch to Map, boxes render, a click drills in, Backspace
// goes back up, "Other folders" is not clickable, the choice is kept after List and back.
async function diskMap(page) {
  await showView(page, 'optimize');
  await page.click('#view-optimize .op-tabs [data-tab="cleanup"]');
  const pane = '#op-pane-cleanup';
  await page.click(`${pane} [data-cl-go="C:\\\\"]`);
  await page.waitForSelector(`${pane} [data-cl-view="map"]`, { timeout: WAIT });
  await page.click(`${pane} [data-cl-view="map"]`);
  await page.waitForSelector(`${pane} .cl-tm-box`, { timeout: WAIT });
  const n = await page.locator(`${pane} .cl-tm-box`).count();
  if (n < 3) throw new Error(`disk map shows ${n} boxes`);
  const sized = await page.evaluate(() => [...document.querySelectorAll('#op-pane-cleanup .cl-tm-box')].every((b) => b.offsetWidth > 0 && b.offsetHeight > 0 && !!b.title));
  if (!sized) throw new Error('a disk map box has no size or tooltip');
  const first = page.locator(`${pane} .cl-tm-box--folder`).first();
  const name = await first.getAttribute('data-name');
  await first.click();
  await page.waitForFunction((nm) => document.querySelector('#op-pane-cleanup .cl-crumb.is-here')?.textContent === nm && document.querySelector('#op-pane-cleanup .cl-tm-box'), name, { timeout: WAIT }).catch(() => {
    throw new Error('clicking a disk map box does not open that folder');
  });
  await page.locator(`${pane} .cl-tm-box`).first().focus();
  await page.keyboard.press('Backspace');
  await page.waitForFunction(() => document.querySelector('#op-pane-cleanup .cl-crumb.is-here')?.textContent === 'C:', null, { timeout: WAIT }).catch(() => {
    throw new Error('Backspace does not go up one folder in the disk map');
  });
  await page.click(`${pane} [data-cl-view="list"]`);
  await page.waitForSelector(`${pane} .cl-rows`, { timeout: WAIT });
  await page.click(`${pane} [data-cl-view="map"]`);
  await page.waitForSelector(`${pane} .cl-tm-box`, { timeout: WAIT });
}

// Processes: right-click an app group (mock chrome.exe), Set priority > Below normal sets
// every member and says so in a toast.
async function groupPriority(page) {
  await showView(page, 'processes');
  await page.fill('#search', 'chrome.exe');
  // Wait for the filter: until it applies, rows re-sort and the first group can be another app.
  await page.waitForFunction(() => {
    const groups = [...document.querySelectorAll('#view-processes .proc-canvas > .proc-row.is-group:not([hidden])')];
    return groups.length > 0 && groups.every((el) => el.textContent.includes('chrome.exe'));
  }, null, { timeout: WAIT }).catch(() => {
    throw new Error('Processes shows no chrome.exe group row');
  });
  const grp = page.locator('#view-processes .proc-canvas > .proc-row.is-group:not([hidden])', { hasText: 'chrome.exe' }).first();
  await grp.click({ button: 'right' });
  const labels = await menuLabels(page);
  if (!labels.includes('Set priority')) throw new Error(`group menu lacks "Set priority": ${labels.join(', ')}`);
  await page.locator('#context-menu .menu-item', { hasText: 'Set priority' }).click();
  await page.waitForFunction(() => /Below normal/.test(document.getElementById('context-menu')?.innerText || ''), null, { timeout: WAIT });
  await page.locator('#context-menu .menu-item', { hasText: 'Below normal' }).click();
  await page.waitForFunction(() => /Set \d+ chrome\.exe processes to Below normal\./.test(document.getElementById('toasts')?.innerText || ''), null, { timeout: WAIT }).catch(() => {
    throw new Error('no toast for the group priority change');
  });
  const prios = await page.evaluate(async () => {
    const { call } = await import('/js/api.js');
    const snap = await call('get_snapshot');
    const out = [];
    for (const r of snap.processes.filter((x) => x.name === 'chrome.exe')) out.push(await call('get_priority', { pid: r.pid }));
    return out;
  });
  if (prios.length < 2 || prios.some((x) => x !== 'belowNormal')) throw new Error(`chrome.exe priorities after Set priority: ${prios.join(', ')}`);
  await page.fill('#search', '');
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

// File search (1.4): file results in the palette, Enter opens and Ctrl+Enter shows in File
// Explorer (by result id), text typed on the wrong keyboard layout still matches, the
// Windows Search line when it is off, and the admin index's progress line.
async function fileSearch(page) {
  const mock = (cmd, args = {}) => page.evaluate(async ([c, a]) => {
    const { call } = await import('/js/api.js');
    return call(c, a);
  }, [cmd, args]);
  const fileItem = (name) => page.locator('#palette .palette-item', { has: page.locator('.palette-item__label', { hasText: new RegExp(`^${name.replace(/[.]/g, '\.')}$`) }) }).first();
  const typeInPalette = async (text) => {
    await page.keyboard.press('Control+K');
    await page.waitForSelector('#palette.is-open .palette-input', { timeout: WAIT });
    await page.fill('#palette .palette-input', '');
    await page.keyboard.type(text);
  };
  const closed = () => page.waitForSelector('#palette:not(.is-open)', { state: 'attached', timeout: WAIT });

  // Pure layout conversion with a mocked map.
  const conv = await page.evaluate(async () => {
    const { convertLayout, layoutVariants } = await import('/js/layouts.js');
    const map = { 'ؤ': 'c', 'ا': 'h', 'ق': 'r', 'خ': 'o', 'ة': 'm', 'ث': 'e' };
    return {
      one: convertLayout('ؤاقخةث', map),
      kept: convertLayout('ؤ1', map),
      variants: layoutVariants('ؤاقخةث', [{ from: 'a', to: 'b', map }]),
      partial: layoutVariants('ؤxyz', [{ from: 'a', to: 'b', map }]),
    };
  });
  if (conv.one !== 'chrome' || conv.kept !== 'c1' || conv.variants.join() !== 'chrome' || conv.partial.length) {
    throw new Error(`layout conversion is wrong: ${JSON.stringify(conv)}`);
  }

  // Earlier checks may have turned the mock's admin on: start on the Windows Search route.
  await mock('mock_search_set', { admin: false });
  const st = await mock('file_search_status');
  if (st.route !== 'windowsSearch') throw new Error(`expected the Windows Search route before admin, got ${st.route}`);

  // Results show with their folder; Enter opens, Ctrl+Enter shows in File Explorer.
  await typeInPalette('notes');
  await fileItem('notes.txt').waitFor({ timeout: WAIT });
  const hint = await fileItem('notes.txt').locator('.palette-item__hint').textContent();
  if (!/Desktop/.test(hint || '')) throw new Error(`file result shows no folder: ${hint}`);
  await fileItem('notes.txt').hover();
  const before = (await mock('mock_search_opened')).length;
  await page.keyboard.press('Enter');
  await closed();
  await typeInPalette('notes');
  await fileItem('notes.txt').waitFor({ timeout: WAIT });
  await fileItem('notes.txt').hover();
  await page.keyboard.press('Control+Enter');
  await closed();
  // The palette closes before the (mock) backend records the call, which takes 25-95 ms.
  let opened = await mock('mock_search_opened');
  for (let i = 0; i < 40 && opened.length < before + 2; i += 1) {
    await page.waitForTimeout(50);
    opened = await mock('mock_search_opened');
  }
  const last2 = opened.slice(-2);
  if (last2.length !== 2 || !last2.every((o) => /notes\.txt$/.test(o.path)) || last2[0].reveal || !last2[1].reveal) {
    throw new Error(`Enter / Ctrl+Enter called the wrong command: ${JSON.stringify(last2)}`);
  }

  // Typed on the Arabic layout: still finds chrome.
  await typeInPalette('ؤاقخةث');
  await fileItem('chrome.exe').waitFor({ timeout: WAIT }).catch(() => {
    throw new Error('wrong-layout text found no chrome.exe');
  });
  await page.keyboard.press('Escape');
  await closed();

  // Windows Search off: one line offering Restart as administrator.
  await mock('mock_search_set', { windowsSearch: false });
  await typeInPalette('report');
  await page.locator('#palette .palette-item', { hasText: 'File search needs Windows Search, or Restart as administrator' }).waitFor({ timeout: WAIT });
  await page.keyboard.press('Escape');
  await closed();
  await mock('mock_search_set', { windowsSearch: true });

  // As administrator: an "Indexing" line, then results; Settings > About shows the index.
  await mock('mock_search_set', { admin: true });
  await typeInPalette('report');
  await page.waitForSelector('#palette [data-palette-indexing]', { timeout: WAIT });
  await fileItem('report-2026.docx').waitFor({ timeout: WAIT });
  if (await page.locator('#palette [data-palette-indexing]').count()) throw new Error('indexing line stays after results');
  await page.keyboard.press('Escape');
  await closed();
  await showView(page, 'processes');
  await showView(page, 'settings');
  await page.waitForFunction(() => /MB/.test(document.querySelector('#view-settings [data-file-index]')?.textContent || ''), null, { timeout: WAIT }).catch(async () => {
    const t = await page.evaluate(() => document.querySelector('#view-settings [data-file-index]')?.textContent);
    throw new Error(`Settings > About shows no file index size: ${t}`);
  });
  await mock('mock_search_set', { admin: null });
}

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
    await check(`${theme}: network usage`, () => netUsageTheme(page));
  }
  await check('network usage', () => netUsage(page, base, watch));
  await check('gpu adapters', () => gpuAdapters(page));
  await check('temperatures', () => sensors(page, base, watch));
  await check('history menu', () => historyMenu(page));
  await check('defender', () => defender(page, base, watch));
  await check('group priority', () => groupPriority(page));
  await check('watchlist restart', () => watchRestart(page));
  await check('svchost services', () => svchostServices(page));
  await check('developer leftovers', () => devLeftovers(page));
  await check('disk map', () => diskMap(page));
  await check('rules', () => rules(page));
  await check('rules 11b', () => rulesMore(page, base, watch));
  await check('suggestions hide', () => suggestionsHide(page, base));
  await check('startup sync', () => startupSync(page));
  await check('debloat', () => debloat(page));
  await check('my rig', () => myRig(page));
  await check('motion', () => motion(page));
  await check('logo', () => logo(page));
  await check('file search', () => fileSearch(page));
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
console.log(`\nPassed ${passed} checks: ${THEMES.length} themes x ${VIEWS.length} views, ${OPTIMIZE_TABS.length} Optimize tabs, palette and details drawer, temperatures, GPU adapters, Network usage (admin, empty, error, Windows record and its fallback), History menu, per-app rules (master switch, in front, reset, Boost link, backup, efficiency mode), Defender scans, group Set priority, watchlist after a restart, services on svchost rows, Debloat, My Rig, motion on and off, sidebar logo, suggestions Hide, Startup sync with Store app rows, file search (open, show in folder, wrong keyboard layout, Windows Search off, indexing), no console errors.`);
