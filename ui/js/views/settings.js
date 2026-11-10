// 09 / Settings: themes, update speed, display, behavior, watchlist, permissions, shortcuts,
// backup, about. The Behavior and Backup panels come from js/panels/ (docs/V04-CONTRACT.md).
import { call, appVersion } from '../api.js';
import { store, on, setSettings, pinStartMatches } from '../store.js';
import { bytes, escapeHtml } from '../format.js';
import { toast, confirmDialog, restartAsAdmin as relaunchElevated, checkForUpdates, installUpdate } from '../app.js';
import { mountPanel as mountBehaviorPanel } from '../panels/behavior.js';
import { mountPanel as mountBackupPanel } from '../panels/backup.js';

const THEMES = [
  { id: 'night', name: 'Night', note: 'The design system as-is' },
  { id: 'void', name: 'Void', note: 'Pure black for OLED' },
  { id: 'signal', name: 'Signal', note: 'High contrast' },
  { id: 'daylight', name: 'Daylight', note: 'Light, from the logo kit' },
];

const INTERVALS = [500, 1000, 2000, 5000];

const GROUPINGS = [
  { id: 'grouped', label: 'Grouped' },
  { id: 'tree', label: 'Tree' },
  { id: 'flat', label: 'Flat list' },
];

const SHORTCUTS = [
  { keys: ['/'], what: 'Focus search' },
  { keys: ['Ctrl', 'K'], what: 'Open the command palette' },
  { keys: ['Ctrl', 'N'], what: 'Run a new task' },
  { keys: ['1', '9'], range: true, what: 'Switch section (0 for Settings)' },
  { keys: ['Space'], what: 'Pause or resume updates' },
  { keys: ['Del'], what: 'End the selected task' },
  { keys: ['Enter'], what: 'Open details for the selected process' },
  { keys: ['Esc'], what: 'Close the drawer, palette or menu' },
  { keys: ['Ctrl', 'C'], what: 'Copy the selected process details' },
];

const WATCH_NOTE = 'Orbit warns when a pinned process stays above 50% CPU for 10 seconds.';

let root = null;
const els = {};
let visible = false;
let dirty = false;
let watchSig = null;
let factsSig = null;

/* ---------- helpers ---------- */

function text(s) {
  return escapeHtml(String(s ?? ''));
}

function attr(s) {
  return text(s).replace(/"/g, '&quot;').replace(/'/g, '&#39;');
}

function errMsg(err) {
  if (typeof err === 'string') return err;
  if (err && typeof err.message === 'string') return err.message;
  return 'Something went wrong.';
}

function icon(id) {
  return `<svg class="icon" aria-hidden="true"><use href="#${id}"/></svg>`;
}

function settings() {
  return store.settings || {};
}

function secs(ms) {
  return `${(ms / 1000).toFixed(1)} s`;
}

function switchHtml(key, labelId) {
  return `<button type="button" class="switch" role="switch" data-switch="${key}" aria-checked="false" aria-labelledby="${labelId}"><span class="switch-thumb" aria-hidden="true"></span></button>`;
}

function kbd(keys, range) {
  const parts = keys.map((k) => `<kbd class="kbd">${text(k)}</kbd>`);
  return parts.join(range ? '<span class="lv-keys-sep">to</span>' : '<span class="lv-keys-sep">+</span>');
}

/* ---------- markup ---------- */

function themeCard(t) {
  return `<button type="button" class="lv-theme" role="radio" data-theme="${t.id}" data-pick="${t.id}" aria-checked="false" tabindex="-1" aria-label="${t.name} theme. ${t.note}.">
    <span class="lv-theme-preview" aria-hidden="true">
      <span class="tp-bar">
        <span class="tp-mark"><i class="tp-m1"></i><i class="tp-m2"></i><i class="tp-m3"></i></span>
        <span class="tp-search"></span>
        <span class="tp-signal"></span>
      </span>
      <span class="tp-body">
        <span class="tp-side"><i class="is-on"></i><i></i><i></i><i></i><i></i></span>
        <span class="tp-main">
          <span class="tp-micro"></span>
          <span class="tp-title"><i></i><i class="tp-em"></i></span>
          <span class="tp-panel">
            <svg class="tp-chart" viewBox="0 0 100 32" preserveAspectRatio="none">
              <path class="tp-area" d="M0 26 L10 22 L20 24 L30 15 L40 18 L50 10 L60 14 L70 8 L80 12 L90 6 L100 9 L100 32 L0 32 Z"/>
              <path class="tp-line" d="M0 26 L10 22 L20 24 L30 15 L40 18 L50 10 L60 14 L70 8 L80 12 L90 6 L100 9"/>
            </svg>
          </span>
          <span class="tp-rows">
            <span class="tp-row"><i class="tp-name"></i><i class="tp-heat" style="--h:.9"></i><i class="tp-heat" style="--h:.5"></i></span>
            <span class="tp-row"><i class="tp-name"></i><i class="tp-heat" style="--h:.45"></i><i class="tp-heat" style="--h:.8"></i></span>
            <span class="tp-row"><i class="tp-name"></i><i class="tp-heat" style="--h:.15"></i><i class="tp-heat" style="--h:.25"></i></span>
          </span>
        </span>
      </span>
    </span>
    <span class="lv-theme-meta">
      <span class="lv-theme-text">
        <span class="lv-theme-name">${t.name}</span>
        <span class="lv-theme-note">${t.note}</span>
      </span>
      <span class="lv-theme-check">${icon('i-check')}</span>
    </span>
  </button>`;
}

function markSvg() {
  // Orbit mark: 5x5 pixel ring (ink), blue core, cyan satellite. Same geometry as assets/logo.
  return `<svg class="lv-mark" viewBox="0 0 110 110" aria-hidden="true">
    <path class="lv-mark-ink" d="M26 5h16v16h-16z M47 5h16v16h-16z M68 5h16v16h-16z M5 26h16v16h-16z M5 47h16v16h-16z M5 68h16v16h-16z M89 26h16v16h-16z M89 47h16v16h-16z M89 68h16v16h-16z M26 89h16v16h-16z M47 89h16v16h-16z M68 89h16v16h-16z"/>
    <path class="lv-mark-blue" d="M47 47h16v16h-16z"/>
    <path class="lv-mark-cyan" d="M89 5h16v16h-16z"/>
  </svg>`;
}

function template() {
  return `
  <div class="lv lv--settings">
    <header class="view-head">
      <div class="lv-head-text">
        <p class="micro">09 / Settings</p>
        <h1 class="view-title">Your console. <em>Your rules.</em></h1>
      </div>
    </header>

    <div class="lv-settings-grid">
      <section class="panel lv-panel lv-panel--wide" aria-labelledby="set-theme-h">
        <div class="lv-panel-head">
          <h2 class="lv-panel-title" id="set-theme-h">Theme</h2>
          <span class="micro">4 palettes / Saved on this PC</span>
        </div>
        <div class="lv-themes" role="radiogroup" aria-labelledby="set-theme-h">
          ${THEMES.map(themeCard).join('')}
        </div>
      </section>

      <section class="panel lv-panel" aria-labelledby="set-updates-h">
        <div class="lv-panel-head">
          <h2 class="lv-panel-title" id="set-updates-h">Updates</h2>
          <span class="micro lv-live" data-live><span class="signal" aria-hidden="true"></span><span data-live-text>Live / 1.0 s</span></span>
        </div>
        <div class="lv-field">
          <div class="lv-field-text">
            <span class="lv-label" id="set-interval-l">Update interval</span>
            <span class="lv-help">How often Orbit samples CPU, memory, disk and processes.</span>
          </div>
          <div class="lv-seg" role="radiogroup" aria-labelledby="set-interval-l" data-seg="interval">
            ${INTERVALS.map((ms) => `<button type="button" role="radio" data-interval="${ms}" aria-checked="false" tabindex="-1">${secs(ms)}</button>`).join('')}
          </div>
        </div>
        <div class="lv-field">
          <div class="lv-field-text">
            <span class="lv-label" id="set-paused-l">Pause updates</span>
            <span class="lv-help">Freeze every number and graph. <kbd class="kbd">Space</kbd> does the same anywhere.</span>
          </div>
          ${switchHtml('paused', 'set-paused-l')}
        </div>
      </section>

      <section class="panel lv-panel" aria-labelledby="set-display-h">
        <div class="lv-panel-head">
          <h2 class="lv-panel-title" id="set-display-h">Display</h2>
          <span class="micro">Layout / Motion</span>
        </div>
        <div class="lv-field">
          <div class="lv-field-text">
            <span class="lv-label" id="set-compact-l">Compact rows</span>
            <span class="lv-help">Fit more rows in every table.</span>
          </div>
          ${switchHtml('compact', 'set-compact-l')}
        </div>
        <div class="lv-field">
          <div class="lv-field-text">
            <span class="lv-label" id="set-motion-l">Reduced motion</span>
            <span class="lv-help">Stop hover lifts, transitions and the pulsing signal.</span>
          </div>
          ${switchHtml('reducedMotion', 'set-motion-l')}
        </div>
        <div class="lv-field">
          <div class="lv-field-text">
            <span class="lv-label" id="set-top-l">Always on top</span>
            <span class="lv-help">Keep Orbit above other windows. The pin in the title bar does the same.</span>
          </div>
          ${switchHtml('alwaysOnTop', 'set-top-l')}
        </div>
        <div class="lv-field">
          <div class="lv-field-text">
            <label class="lv-label" id="set-grouping-l" for="set-grouping">Default process view</label>
            <span class="lv-help">How the process list opens: grouped into Apps, Background and Windows, as a parent and child tree, or flat.</span>
          </div>
          <select class="select lv-select" id="set-grouping" data-grouping>
            ${GROUPINGS.map((g) => `<option value="${g.id}">${g.label}</option>`).join('')}
          </select>
        </div>
      </section>

      <section class="panel lv-panel" data-panel="behavior"></section>

      <section class="panel lv-panel" aria-labelledby="set-watch-h">
        <div class="lv-panel-head">
          <h2 class="lv-panel-title" id="set-watch-h">Watchlist</h2>
          <span class="micro" data-watch-count>0 pinned</span>
        </div>
        <p class="lv-help lv-watch-note">${WATCH_NOTE}</p>
        <ul class="lv-watch" data-watch aria-labelledby="set-watch-h" tabindex="-1"></ul>
      </section>

      <section class="panel lv-panel" aria-labelledby="set-perm-h">
        <div class="lv-panel-head">
          <h2 class="lv-panel-title" id="set-perm-h">Permissions</h2>
          <span class="micro">Access / Windows</span>
        </div>
        <div class="lv-perm">
          <span class="chip lv-perm-chip" data-perm-chip>${icon('i-shield')}<span data-perm-text>Checking</span></span>
          <p class="lv-help" data-perm-help>Some actions, like ending protected processes, changing machine-wide startup entries and controlling services, need administrator rights.</p>
          <button type="button" class="btn btn--primary" data-admin>Restart as administrator</button>
        </div>
      </section>

      <section class="panel lv-panel" aria-labelledby="set-keys-h">
        <div class="lv-panel-head">
          <h2 class="lv-panel-title" id="set-keys-h">Keyboard shortcuts</h2>
          <span class="micro">Everywhere</span>
        </div>
        <table class="lv-keys" aria-labelledby="set-keys-h">
          <thead><tr><th scope="col">Keys</th><th scope="col">Action</th></tr></thead>
          <tbody>
            ${SHORTCUTS.map((s) => `<tr><td class="lv-keys-k">${kbd(s.keys, s.range)}</td><td>${text(s.what)}</td></tr>`).join('')}
          </tbody>
        </table>
      </section>

      <section class="panel lv-panel" data-panel="backup"></section>

      <section class="panel lv-panel lv-about" aria-labelledby="set-about-h">
        <div class="lv-panel-head">
          <h2 class="lv-panel-title" id="set-about-h">About</h2>
          <span class="micro">Version / <span data-version>…</span></span>
        </div>
        <div class="lv-about-id">
          ${markSvg()}
          <div class="lv-about-text">
            <p class="lv-about-name">Orbit <span data-version>…</span></p>
            <p class="lv-about-by">Made by Legacy<span class="brand-accent">Tech</span></p>
          </div>
        </div>
        <div class="lv-field">
          <div class="lv-field-text">
            <span class="lv-label" id="set-autoupdate-l">Check for updates automatically</span>
            <span class="lv-help" data-update-status aria-live="polite">Orbit checks GitHub for a newer version when it starts.</span>
          </div>
          ${switchHtml('autoUpdate', 'set-autoupdate-l')}
        </div>
        <div class="lv-update-actions">
          <button type="button" class="btn btn--sm" data-update-check>Check for updates</button>
          <button type="button" class="btn btn--sm btn--primary" data-update-install hidden>Update now</button>
        </div>
        <dl class="lv-facts" data-facts></dl>
        <a class="lv-textlink" href="https://legacytech.me" data-site>legacytech.me${icon('i-arrow-right')}</a>
      </section>
    </div>
  </div>`;
}

/* ---------- sync ---------- */

function syncRadios(container, attrName, value) {
  const items = container.querySelectorAll(`[${attrName}]`);
  let any = false;
  for (const el of items) {
    const on = String(el.getAttribute(attrName)) === String(value);
    el.setAttribute('aria-checked', on ? 'true' : 'false');
    el.tabIndex = on ? 0 : -1;
    if (on) any = true;
  }
  if (!any && items[0]) items[0].tabIndex = 0;
}

function syncControls() {
  const s = settings();
  syncRadios(els.themes, 'data-pick', s.theme || 'night');
  syncRadios(els.seg, 'data-interval', s.intervalMs || 1000);
  for (const sw of root.querySelectorAll('[data-switch]')) {
    sw.setAttribute('aria-checked', s[sw.dataset.switch] ? 'true' : 'false');
  }
  const grouping = s.grouping || 'grouped';
  if (els.grouping.value !== grouping) els.grouping.value = grouping;
  els.liveText.textContent = s.paused ? `Paused / ${secs(s.intervalMs || 1000)}` : `Live / ${secs(s.intervalMs || 1000)}`;
  els.live.classList.toggle('is-paused', Boolean(s.paused));
}

// The running process a pid pin belongs to: same pid and the same start time.
function processForPin(e) {
  const procs = store.snapshot && store.snapshot.processes;
  if (!procs) return null;
  for (let i = 0; i < procs.length; i++) {
    if (procs[i].pid === e.pid) return pinStartMatches(e.start, procs[i]) ? procs[i] : null;
  }
  return null;
}

function renderWatch() {
  const s = settings();
  const pids = Array.isArray(s.watchlist) ? s.watchlist : [];
  const names = Array.isArray(s.watchNames) ? s.watchNames : [];
  const live = pids.map((e) => processForPin(e));
  const sig = `${pids.map((e, i) => `${e.pid}:${live[i] ? live[i].name : ''}`).join(',')}|${names.join(',')}`;
  if (sig === watchSig) return;

  const focusedKey = document.activeElement && els.watch.contains(document.activeElement)
    ? document.activeElement.dataset.removeKey || null
    : null;
  const focusedIndex = focusedKey
    ? [...els.watch.querySelectorAll('[data-remove-key]')].findIndex((b) => b.dataset.removeKey === focusedKey)
    : -1;
  watchSig = sig;

  const total = pids.length + names.length;
  els.watchCount.textContent = `${total} pinned`;

  if (!total) {
    els.watch.innerHTML = `<li class="lv-watch-empty">Nothing pinned yet. Pin a process from its menu or the details drawer.</li>`;
  } else {
    const items = [];
    pids.forEach(({ pid }, i) => {
      const p = live[i];
      const name = p ? p.name : 'Process ended';
      items.push(`<li class="lv-watch-item">
        <span class="lv-watch-ico">${icon('i-pin')}</span>
        <span class="lv-watch-text">
          <span class="lv-watch-name">${text(name)}</span>
          <span class="lv-watch-meta">PID ${pid}</span>
        </span>
        ${p
          ? '<span class="chip chip--live lv-chip"><span class="signal" aria-hidden="true"></span>Running</span>'
          : '<span class="chip lv-chip lv-chip--muted">Not running</span>'}
        <button type="button" class="icon-btn lv-more" data-remove-key="pid:${pid}" aria-label="Remove ${attr(name)} (PID ${pid}) from the watchlist" title="Remove">${icon('i-close')}</button>
      </li>`);
    });
    for (const n of names) {
      items.push(`<li class="lv-watch-item">
        <span class="lv-watch-ico">${icon('i-pin')}</span>
        <span class="lv-watch-text">
          <span class="lv-watch-name">${text(n)}</span>
          <span class="lv-watch-meta">Every process with this name</span>
        </span>
        <span class="chip lv-chip lv-chip--muted">By name</span>
        <button type="button" class="icon-btn lv-more" data-remove-key="name:${attr(n)}" aria-label="Remove ${attr(n)} from the watchlist" title="Remove">${icon('i-close')}</button>
      </li>`);
    }
    els.watch.innerHTML = items.join('');
  }

  if (focusedIndex >= 0) {
    const buttons = els.watch.querySelectorAll('[data-remove-key]');
    const next = buttons[Math.min(focusedIndex, buttons.length - 1)];
    if (next) next.focus({ preventScroll: true });
    else els.watch.focus({ preventScroll: true });
  }
}

function renderPermissions() {
  const info = store.staticInfo;
  const known = Boolean(info);
  const admin = known && Boolean(info.isAdmin);
  els.permText.textContent = !known ? 'Checking' : admin ? 'Administrator' : 'Standard user';
  els.permChip.classList.toggle('chip--on', admin);
  els.admin.disabled = !known || admin;
  els.admin.textContent = admin ? 'Running as administrator' : 'Restart as administrator';
  els.permHelp.textContent = admin
    ? 'Orbit has administrator rights. Protected system processes can still refuse some actions.'
    : 'Some actions, like ending protected processes, changing machine-wide startup entries and controlling services, need administrator rights.';
}

function renderFacts() {
  const info = store.staticInfo;
  const sig = info ? `${info.hostName}|${info.osName}|${info.osVersion}|${info.cpuBrand}|${info.logicalCores}|${info.physicalCores}|${info.totalMemory}` : '';
  if (sig === factsSig) return;
  factsSig = sig;
  if (!info) {
    els.facts.innerHTML = '<div class="lv-fact"><dt>System</dt><dd>Reading system facts</dd></div>';
    return;
  }
  const os = [info.osName, info.osVersion].filter(Boolean).join(' ') || 'Unknown';
  const cores = info.physicalCores
    ? `${info.physicalCores} physical / ${info.logicalCores} logical`
    : `${info.logicalCores} logical`;
  const rows = [
    ['Host', info.hostName || 'Unknown'],
    ['OS', os],
    ['CPU', (info.cpuBrand || 'Unknown').trim()],
    ['Cores', cores],
    ['RAM', bytes(info.totalMemory)],
  ];
  els.facts.innerHTML = rows.map(([k, v]) => `<div class="lv-fact"><dt>${k}</dt><dd title="${attr(v)}">${text(v)}</dd></div>`).join('');
}

function renderAll() {
  dirty = false;
  syncControls();
  renderWatch();
  renderPermissions();
  renderFacts();
}

/* ---------- actions ---------- */

/** app.js pushes every interval/pause change to the backend (set_sampling) on 'settings'. */
function applySampling(patch) {
  setSettings(patch);
}

function pickTheme(id) {
  if (settings().theme === id) return;
  setSettings({ theme: id });
}

function onSwitch(key) {
  const next = !settings()[key];
  if (key === 'paused') applySampling({ paused: next });
  else setSettings({ [key]: next });
}

function removeWatch(key) {
  const s = settings();
  if (key.startsWith('pid:')) {
    const pid = Number(key.slice(4));
    setSettings({ watchlist: (s.watchlist || []).filter((e) => e.pid !== pid) });
  } else if (key.startsWith('name:')) {
    const name = key.slice(5);
    setSettings({ watchNames: (s.watchNames || []).filter((n) => n !== name) });
  }
}

async function restartAsAdmin() {
  const ok = await confirmDialog({
    title: 'Restart as administrator?',
    body: 'Orbit closes and opens again with administrator rights. Windows asks you to allow it first.',
    confirmLabel: 'Restart',
    danger: false,
  });
  if (!ok) return;
  // Shared with the titlebar badge: calls restart_as_admin and reports errors.
  await relaunchElevated();
  renderPermissions();
}

async function openSite(ev) {
  ev.preventDefault();
  try {
    await call('open_url', { url: 'https://legacytech.me' });
  } catch (err) {
    toast(errMsg(err), 'error');
  }
}

/** Radio-group keyboard pattern: arrows move and select, Home/End jump. */
function radioKeys(container, attrName, select) {
  container.addEventListener('keydown', (ev) => {
    const keys = ['ArrowRight', 'ArrowDown', 'ArrowLeft', 'ArrowUp', 'Home', 'End'];
    if (!keys.includes(ev.key)) return;
    const items = [...container.querySelectorAll(`[${attrName}]`)];
    const idx = items.indexOf(ev.target.closest(`[${attrName}]`));
    if (idx < 0) return;
    ev.preventDefault();
    let next = idx;
    if (ev.key === 'ArrowRight' || ev.key === 'ArrowDown') next = (idx + 1) % items.length;
    else if (ev.key === 'ArrowLeft' || ev.key === 'ArrowUp') next = (idx - 1 + items.length) % items.length;
    else if (ev.key === 'Home') next = 0;
    else if (ev.key === 'End') next = items.length - 1;
    select(items[next].getAttribute(attrName));
    items[next].focus();
  });
}

/* ---------- mount ---------- */

export function mount(rootEl) {
  root = rootEl;
  root.innerHTML = template();

  els.themes = root.querySelector('.lv-themes');
  els.seg = root.querySelector('[data-seg="interval"]');
  els.grouping = root.querySelector('[data-grouping]');
  els.live = root.querySelector('[data-live]');
  els.liveText = root.querySelector('[data-live-text]');
  els.watch = root.querySelector('[data-watch]');
  els.watchCount = root.querySelector('[data-watch-count]');
  els.permChip = root.querySelector('[data-perm-chip]');
  els.permText = root.querySelector('[data-perm-text]');
  els.permHelp = root.querySelector('[data-perm-help]');
  els.admin = root.querySelector('[data-admin]');
  els.facts = root.querySelector('[data-facts]');

  // Panels owned by other modules; one that fails is hidden, the rest of Settings still works.
  for (const [name, mountFn] of [['behavior', mountBehaviorPanel], ['backup', mountBackupPanel]]) {
    const el = root.querySelector(`[data-panel="${name}"]`);
    try {
      mountFn(el);
    } catch (err) {
      console.error(`[orbit] ${name} panel failed`, err);
      el.hidden = true;
    }
  }

  els.themes.addEventListener('click', (ev) => {
    const card = ev.target.closest('[data-pick]');
    if (card) pickTheme(card.dataset.pick);
  });
  radioKeys(els.themes, 'data-pick', pickTheme);

  els.seg.addEventListener('click', (ev) => {
    const b = ev.target.closest('[data-interval]');
    if (!b) return;
    const ms = Number(b.dataset.interval);
    if (ms !== settings().intervalMs) applySampling({ intervalMs: ms });
  });
  radioKeys(els.seg, 'data-interval', (v) => {
    const ms = Number(v);
    if (ms !== settings().intervalMs) applySampling({ intervalMs: ms });
  });

  root.addEventListener('click', (ev) => {
    const sw = ev.target.closest('[data-switch]');
    if (sw && root.contains(sw)) onSwitch(sw.dataset.switch);
  });

  els.grouping.addEventListener('change', () => {
    setSettings({ grouping: els.grouping.value });
  });

  els.watch.addEventListener('click', (ev) => {
    const b = ev.target.closest('[data-remove-key]');
    if (b) removeWatch(b.dataset.removeKey);
  });

  els.admin.addEventListener('click', restartAsAdmin);
  root.querySelector('[data-site]').addEventListener('click', openSite);

  // Updates: real version from the app, check button, and the status line.
  els.updateStatus = root.querySelector('[data-update-status]');
  els.updateCheck = root.querySelector('[data-update-check]');
  els.updateInstall = root.querySelector('[data-update-install]');
  appVersion().then((v) => {
    for (const el of root.querySelectorAll('[data-version]')) el.textContent = v;
  }).catch(() => {});
  els.updateCheck.addEventListener('click', () => checkForUpdates({ manual: true }));
  els.updateInstall.addEventListener('click', installUpdate);
  on('update', renderUpdate);
  if (store.update) renderUpdate(store.update);

  on('settings', () => {
    if (visible) {
      syncControls();
      renderWatch();
    } else {
      dirty = true;
    }
  });

  on('snapshot', () => {
    if (!visible || document.hidden) return;
    // Pinned processes start and end; static info may arrive after boot.
    renderWatch();
    if (!factsSig) {
      renderPermissions();
      renderFacts();
    }
  });

  // Minimised: snapshots skip the watchlist render above; catch up when shown again.
  document.addEventListener('visibilitychange', () => { if (visible && !document.hidden) renderWatch(); });

  renderAll();
}

const UPDATE_IDLE = 'Orbit checks GitHub for a newer version when it starts.';

function renderUpdate(u) {
  if (!els.updateStatus || !u) return;
  const busy = u.state === 'checking' || u.state === 'installing';
  els.updateCheck.disabled = busy;
  els.updateInstall.hidden = u.state !== 'available';
  els.updateInstall.disabled = busy;
  els.updateStatus.textContent =
    u.state === 'checking' ? 'Checking for updates…'
    : u.state === 'installing' ? 'Downloading the update. Orbit will restart when it is installed.'
    : u.state === 'available' ? `Version ${u.info.version} is available.`
    : u.state === 'current' ? 'Orbit is up to date.'
    : u.state === 'error' ? u.message || 'Couldn’t check for updates.'
    : UPDATE_IDLE;
}

export function onShow() {
  visible = true;
  renderAll();
}

export function onHide() {
  visible = false;
}
