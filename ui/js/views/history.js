// 03 / History: what each app used over past days, like Task Manager's App history.
//
// Orbit records it itself while it runs (history.rs): per app, CPU time, active time,
// average and peak memory, I/O and, when Orbit runs as administrator, network bytes. The
// data stays on this PC. `get_history(days)` returns the totals for the range; the view
// reloads on show, on range change and every 30 s while visible (the backend includes what
// it has not written yet, so today is always current). "Record app history" is
// settings.historyEnabled, sent to the backend at boot and on every change.

import { call } from '../api.js';
import { store, on, setSettings } from '../store.js';
import { bytes, time, escapeHtml, fuzzy, IO_TITLE } from '../format.js';
import { toast, navigate, confirmDialog } from '../app.js';
import { registerCommands } from '../palette.js';

const REFRESH_MS = 30000;
const RANGES = [
  { id: 'today', label: 'Today', days: 1 },
  { id: '7', label: '7 days', days: 7 },
  { id: '30', label: '30 days', days: 30 },
  { id: 'custom', label: 'Custom', days: 0 },
];
const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
const CPU_TITLE = 'Processor time, counted in one core: two cores busy for a minute add 2 minutes.';
const ACTIVE_TITLE = 'Time the app had a window or used more than 0.5% CPU.';
const MEM_TITLE = 'Working set of all its processes together.';
const NET_TITLE = 'Downloaded plus uploaded. Per-app network needs Orbit to run as administrator.';
const NEEDS_ADMIN_TITLE = 'Orbit records per-app network only while it runs as administrator.';

let root = null;
const els = {};
let visible = false;
let rows = [];
let loaded = false;
let loading = false;
let loadError = null;
let reloadAgain = false;
let timer = 0;
let sort = { key: 'cpu', dir: -1 };
let lastSent = null; // last historyEnabled value sent to the backend
const icons = new Map(); // exe -> data URL, '' while asked or when Windows has none
let unregister = null;

const icon = (id) => `<svg class="icon" aria-hidden="true"><use href="#${id}"/></svg>`;
const text = (s) => escapeHtml(s == null ? '' : String(s));

function errText(err) {
  if (typeof err === 'string' && err) return err;
  if (err && typeof err.message === 'string' && err.message) return err.message;
  return 'Something went wrong. Try again.';
}

function pad2(n) {
  return n < 10 ? `0${n}` : String(n);
}

/**
 * "1h 02m", "12m 30s", "45s", "0s"; with full = true "1h 02m 05s". Hours keep counting
 * past a day, so long ranges stay comparable.
 */
export function hms(secs, full = false) {
  const t = Math.max(0, Math.round(Number(secs) || 0));
  const h = Math.floor(t / 3600);
  const m = Math.floor((t % 3600) / 60);
  const s = t % 60;
  if (h > 0) return full ? `${h}h ${pad2(m)}m ${pad2(s)}s` : `${h}h ${pad2(m)}m`;
  if (m > 0) return `${m}m ${pad2(s)}s`;
  return `${s}s`;
}

function dayLabel(epochSecs) {
  const d = new Date(epochSecs * 1000);
  return `${d.getDate()} ${MONTHS[d.getMonth()]}`;
}

function lastSeenText(epochSecs) {
  if (!epochSecs) return '-';
  const ago = Math.floor(Date.now() / 1000) - epochSecs;
  if (ago < 120) return 'Just now';
  if (ago < 3600) return `${Math.floor(ago / 60)} min ago`;
  const d = new Date(epochSecs * 1000);
  const today = new Date();
  if (d.toDateString() === today.toDateString()) return `Today ${pad2(d.getHours())}:${pad2(d.getMinutes())}`;
  const y = new Date(today);
  y.setDate(today.getDate() - 1);
  if (d.toDateString() === y.toDateString()) return `Yesterday ${pad2(d.getHours())}:${pad2(d.getMinutes())}`;
  return `${dayLabel(epochSecs)}`;
}

// ------------------------------------------------------------------ range

function maxDays() {
  return Math.min(90, Math.max(1, Number(store.settings.historyDays) || 30));
}

function rangeMode() {
  const m = store.settings.historyRange;
  return RANGES.some((r) => r.id === m) ? m : 'today';
}

function customDays() {
  const n = Math.round(Number(store.settings.historyCustomDays) || 14);
  return Math.min(maxDays(), Math.max(1, n));
}

/** Days the current range asks for, never more than settings.historyDays. */
function rangeDays() {
  const mode = rangeMode();
  if (mode === 'custom') return customDays();
  const r = RANGES.find((x) => x.id === mode);
  return Math.min(maxDays(), r ? r.days : 1);
}

function rangeText() {
  const d = rangeDays();
  return d === 1 ? 'today' : `in the last ${d} days`;
}

function setRange(mode) {
  if (!RANGES.some((r) => r.id === mode)) return;
  setSettings({ historyRange: mode });
}

// ------------------------------------------------------------------ data

async function load() {
  if (loading) {
    reloadAgain = true;
    return;
  }
  loading = true;
  if (els.refresh) els.refresh.classList.add('is-busy');
  const days = rangeDays();
  try {
    const res = await call('get_history', { days });
    rows = Array.isArray(res) ? res : [];
    loaded = true;
    loadError = null;
    loadIcons(rows.map((r) => r.exe));
  } catch (err) {
    loadError = errText(err);
    if (loaded && isVisible()) toast(`Could not refresh app history. ${loadError}`, 'error');
  }
  loading = false;
  if (els.refresh) els.refresh.classList.remove('is-busy');
  // The range changed while this one was loading: load the new one.
  if (reloadAgain || days !== rangeDays()) {
    reloadAgain = false;
    load();
    return;
  }
  if (isVisible()) render();
}

function startTimer() {
  stopTimer();
  timer = setInterval(() => { if (isVisible() && !document.hidden) load(); }, REFRESH_MS);
}

function stopTimer() {
  if (timer) clearInterval(timer);
  timer = 0;
}

async function loadIcons(paths) {
  const want = [...new Set(paths.filter((p) => p && !icons.has(p)))];
  if (!want.length) return;
  for (const p of want) icons.set(p, '');
  try {
    const map = (await call('get_icons', { paths: want })) || {};
    let got = false;
    for (const p of want) {
      if (typeof map[p] === 'string' && map[p].startsWith('data:image/')) {
        icons.set(p, map[p]);
        got = true;
      }
    }
    if (got && isVisible()) render();
  } catch (_) { /* keep the blank tiles */ }
}

function sendEnabled() {
  const on = store.settings.historyEnabled !== false;
  if (on === lastSent) return;
  lastSent = on;
  call('set_history_enabled', { enabled: on }).catch((err) => {
    lastSent = null;
    console.warn('[orbit] set_history_enabled failed', err);
  });
}

// ------------------------------------------------------------------ filter and sort

function netBytes(r) {
  return (Number(r.netRxBytes) || 0) + (Number(r.netTxBytes) || 0);
}

function perAppNet() {
  const s = store.snapshot;
  return !!(s && s.network && s.network.perApp && s.network.perApp.available);
}

const SORTS = {
  name: (r) => r.name.toLowerCase(),
  cpu: (r) => r.cpuSeconds,
  active: (r) => r.activeSeconds,
  avg: (r) => r.avgMemory,
  max: (r) => r.maxMemory,
  read: (r) => r.diskReadBytes,
  write: (r) => r.diskWriteBytes,
  net: netBytes,
  last: (r) => r.lastSeen,
};

function visibleRows() {
  const q = (store.query || '').trim();
  const list = q ? rows.filter((r) => fuzzy(`${r.name} ${r.exe || ''}`, q) >= 1000) : [...rows];
  const get = SORTS[sort.key] || SORTS.cpu;
  const d = sort.dir;
  list.sort((a, b) => {
    const x = get(a);
    const y = get(b);
    const c = typeof x === 'string' ? x.localeCompare(y) : (x || 0) - (y || 0);
    return (c || b.cpuSeconds - a.cpuSeconds || a.key.localeCompare(b.key)) * (c ? d : 1);
  });
  return list;
}

// ------------------------------------------------------------------ render

function iconCell(exe) {
  const src = exe ? icons.get(exe) : '';
  if (src) return `<img class="lv-ico" src="${src}" alt="" width="18" height="18" decoding="async">`;
  return `<span class="lv-ico lv-ico--blank" aria-hidden="true">${icon('i-grid')}</span>`;
}

function netCell(r, admin) {
  const n = netBytes(r);
  if (n > 0) {
    return `<span class="lv-num" title="Down ${text(bytes(r.netRxBytes))}, up ${text(bytes(r.netTxBytes))}">${text(bytes(n))}</span>`;
  }
  if (admin) return '<span class="lv-num">0 B</span>';
  return `<span class="lv-faint hist-na" title="${text(NEEDS_ADMIN_TITLE)}">Needs admin</span>`;
}

function rowHtml(r, maxCpu, admin) {
  const share = maxCpu > 0 ? Math.max(0, Math.min(1, r.cpuSeconds / maxCpu)) : 0;
  const w = Math.round(share * 1000) / 10;
  return `<tr class="row" data-key="${text(r.key)}">
    <td class="c-name"><div class="lv-namecell">
      ${iconCell(r.exe)}
      <span class="lv-primary" title="${text(r.exe || r.name)}">${text(r.name)}</span>
    </div></td>
    <td class="c-cpu cell-num"><div class="hist-cpu">
      <span class="hist-bar" aria-hidden="true"><span style="width:${w}%"></span></span>
      <span class="lv-num hist-cpu-val" title="${text(hms(r.cpuSeconds, true))}">${text(hms(r.cpuSeconds))}</span>
    </div></td>
    <td class="c-active cell-num"><span class="lv-num" title="${text(hms(r.activeSeconds, true))}">${text(hms(r.activeSeconds))}</span></td>
    <td class="c-avg cell-num"><span class="lv-num">${text(bytes(r.avgMemory))}</span></td>
    <td class="c-max cell-num"><span class="lv-num">${text(bytes(r.maxMemory))}</span></td>
    <td class="c-read cell-num"><span class="lv-num">${text(bytes(r.diskReadBytes))}</span></td>
    <td class="c-write cell-num"><span class="lv-num">${text(bytes(r.diskWriteBytes))}</span></td>
    <td class="c-net cell-num">${netCell(r, admin)}</td>
    <td class="c-last"><span class="lv-num" title="${text(time(r.lastSeen))}">${text(lastSeenText(r.lastSeen))}</span></td>
  </tr>`;
}

function showEmpty(title, body, action) {
  els.table.classList.add('is-empty');
  els.empty.hidden = false;
  els.empty.innerHTML = `${icon('i-history')}
    <p class="empty-title lv-empty-title">${text(title)}</p>
    ${body ? `<p class="lv-empty-body">${text(body)}</p>` : ''}
    ${action ? `<button type="button" class="btn btn--ghost btn--sm" data-empty-act="${action.id}">${text(action.label)}</button>` : ''}`;
}

function hideEmpty() {
  els.table.classList.remove('is-empty');
  if (els.empty.hidden) return;
  els.empty.hidden = true;
  els.empty.innerHTML = '';
}

function renderControls() {
  const mode = rangeMode();
  const max = maxDays();
  for (const btn of els.ranges.querySelectorAll('[data-range]')) {
    const r = RANGES.find((x) => x.id === btn.dataset.range);
    // A preset longer than historyDays is hidden; Custom covers the rest.
    btn.hidden = r.days > 1 && r.days > max;
    const on = r.id === mode;
    btn.classList.toggle('chip--on', on);
    btn.setAttribute('aria-pressed', on ? 'true' : 'false');
  }
  els.custom.hidden = mode !== 'custom';
  els.days.max = String(max);
  if (document.activeElement !== els.days) els.days.value = String(customDays());
  els.daysMax.textContent = `of ${max}`;

  const enabled = store.settings.historyEnabled !== false;
  els.recSwitch.setAttribute('aria-checked', enabled ? 'true' : 'false');
  els.noteText.textContent = enabled
    ? 'History stays on this PC. Orbit counts what each app uses while Orbit is running; time it was closed is missing.'
    : 'History stays on this PC. Recording is off: Orbit adds nothing new until you turn on Record app history.';
  els.note.classList.toggle('is-off', !enabled);
}

function renderStats(list) {
  if (!loaded) {
    for (const el of [els.statApps, els.statCpu, els.statIo, els.statSince]) el.textContent = '-';
    return;
  }
  let cpu = 0;
  let io = 0;
  let since = 0;
  for (const r of list) {
    cpu += r.cpuSeconds || 0;
    io += (r.diskReadBytes || 0) + (r.diskWriteBytes || 0);
    if (r.firstSeen && (!since || r.firstSeen < since)) since = r.firstSeen;
  }
  els.statApps.textContent = String(list.length);
  els.statCpu.textContent = hms(cpu);
  els.statCpu.title = hms(cpu, true);
  els.statIo.textContent = bytes(io);
  els.statSince.textContent = since ? dayLabel(since) : '-';
  els.statSince.title = since ? time(since) : '';
}

function renderTable() {
  for (const th of els.table.querySelectorAll('th[data-col]')) {
    const on = th.dataset.col === sort.key;
    th.classList.toggle('is-sorted-asc', on && sort.dir === 1);
    th.classList.toggle('is-sorted-desc', on && sort.dir === -1);
    th.setAttribute('aria-sort', on ? (sort.dir === 1 ? 'ascending' : 'descending') : 'none');
  }
  if (!loaded) {
    els.tbody.innerHTML = '';
    els.count.textContent = '';
    renderStats([]);
    if (loadError) {
      const body = /^Could not read app history\.?$/.test(loadError) ? 'Try again in a moment.' : loadError;
      showEmpty('Could not read app history.', body, { id: 'retry', label: 'Try again' });
    }
    else showEmpty('Reading app history.', 'Adding up what each app used.');
    return;
  }
  const list = visibleRows();
  renderStats(list);
  els.count.textContent = `${list.length} of ${rows.length} apps`;
  if (!list.length) {
    els.tbody.innerHTML = '';
    const q = (store.query || '').trim();
    if (rows.length) showEmpty(`No apps match ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬Ãƒâ€¦Ã¢â‚¬Å“${q}ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬Ãƒâ€šÃ‚Â.`, 'Clear the search to see every app.');
    else if (store.settings.historyEnabled === false) {
      showEmpty('Recording is off.', `Nothing was recorded ${rangeText()}. Turn on Record app history to start.`, { id: 'enable', label: 'Turn on recording' });
    } else showEmpty(`Nothing recorded ${rangeText()} yet.`, 'Orbit adds up what each app uses while it runs. Leave it open for a while, then look again.');
    return;
  }
  hideEmpty();
  let maxCpu = 0;
  for (const r of list) if (r.cpuSeconds > maxCpu) maxCpu = r.cpuSeconds;
  const admin = perAppNet();
  const scroll = els.scroll.scrollTop;
  els.tbody.innerHTML = list.map((r) => rowHtml(r, maxCpu, admin)).join('');
  els.scroll.scrollTop = scroll;
}

function render() {
  if (!root) return;
  renderControls();
  renderTable();
}

// ------------------------------------------------------------------ actions

function setRecording(on) {
  setSettings({ historyEnabled: !!on });
  toast(on ? 'Recording app history.' : 'App history recording is off.', 'info');
}

async function deleteHistory() {
  const ok = await confirmDialog({
    title: 'Delete app history?',
    body: 'Orbit deletes every day it recorded on this PC. This cannot be undone. Recording carries on if it is turned on.',
    confirmLabel: 'Delete history',
    danger: true,
  });
  if (!ok) return;
  try {
    await call('clear_history');
    rows = [];
    toast('App history deleted.', 'success');
  } catch (err) {
    toast(`Could not delete app history. ${errText(err)}`, 'error');
  }
  load();
}

function applyCustomDays() {
  const n = Math.round(Number(els.days.value));
  const v = Number.isFinite(n) && n >= 1 ? Math.min(maxDays(), n) : customDays();
  els.days.value = String(v);
  setSettings({ historyCustomDays: v });
}

// ------------------------------------------------------------------ palette

function paletteItems() {
  const enabled = store.settings.historyEnabled !== false;
  return [
    {
      label: 'App history',
      search: 'app history usage past days cpu time memory disk network apps used',
      icon: 'i-history',
      hint: 'History',
      run: () => navigate('history'),
    },
    {
      label: enabled ? 'Turn off app history recording' : 'Turn on app history recording',
      search: 'record app history recording pause stop start privacy',
      icon: enabled ? 'i-pause' : 'i-play',
      hint: 'History',
      run: () => setRecording(!enabled),
    },
    {
      label: 'Delete app history',
      search: 'delete clear erase app history privacy',
      icon: 'i-x',
      hint: 'History',
      danger: true,
      run: () => deleteHistory(),
    },
  ];
}

// ------------------------------------------------------------------ view API

export function mount(rootEl) {
  root = rootEl;
  root.classList.add('view--fill');
  const th = (col, label, title, cls = 'cell-num') => `<th class="c-${col} ${cls}" scope="col" data-col="${col}" aria-sort="none"${title ? ` title="${text(title)}"` : ''}><button type="button" class="lv-sort" data-sort="${col}">${label}</button></th>`;
  root.innerHTML = `
  <div class="lv hist">
    <header class="view-head">
      <div class="lv-head-text">
        <p class="micro">03 / History</p>
        <h1 class="view-title">What ran. <em>What it cost.</em></h1>
      </div>
      <div class="lv-stats" aria-live="polite">
        <div class="stat lv-stat"><span class="micro stat-label">Apps</span><span class="stat-value lv-stat-value" data-stat="apps">-</span></div>
        <div class="stat lv-stat" title="${text(CPU_TITLE)}"><span class="micro stat-label">CPU time</span><span class="stat-value lv-stat-value" data-stat="cpu">-</span></div>
        <div class="stat lv-stat"><span class="micro stat-label">I/O</span><span class="stat-value lv-stat-value" data-stat="io">-</span></div>
        <div class="stat lv-stat"><span class="micro stat-label">Since</span><span class="stat-value lv-stat-value" data-stat="since">-</span></div>
      </div>
    </header>
    <div class="toolbar lv-toolbar">
      <div class="lv-chips" role="group" aria-label="Date range" data-ranges>
        ${RANGES.map((r) => `<button type="button" class="chip" data-range="${r.id}" aria-pressed="false">${r.label}</button>`).join('')}
      </div>
      <label class="hist-custom" hidden>
        <span>Last</span>
        <input class="input hist-days" type="number" min="1" max="30" step="1" inputmode="numeric" aria-label="Number of days">
        <span>days</span>
        <span class="micro hist-days-max"></span>
      </label>
      <span class="lv-toolbar-spacer"></span>
      <span class="micro lv-count" data-count aria-live="polite"></span>
      <div class="hist-rec">
        <span id="hist-rec-l" class="hist-rec-label">Record app history</span>
        <button type="button" class="switch" role="switch" aria-checked="true" aria-labelledby="hist-rec-l" data-rec><span class="switch-thumb" aria-hidden="true"></span></button>
      </div>
      <button type="button" class="btn btn--ghost btn--sm" data-clear>Delete history</button>
      <button type="button" class="icon-btn lv-refresh" data-refresh aria-label="Refresh app history" title="Refresh (updates every 30 seconds)">${icon('i-refresh')}</button>
    </div>
    <p class="hist-note">${icon('i-lock')}<span data-note-text></span></p>
    <div class="panel lv-table-panel">
      <div class="lv-scroll">
        <table class="table lv-table hist-table">
          <thead><tr>
            ${th('name', 'Name', '', '')}
            ${th('cpu', 'CPU time', CPU_TITLE)}
            ${th('active', 'Active', ACTIVE_TITLE)}
            ${th('avg', 'Avg mem', `Average memory. ${MEM_TITLE}`)}
            ${th('max', 'Max mem', `Peak memory. ${MEM_TITLE}`)}
            ${th('read', 'I/O read', IO_TITLE)}
            ${th('write', 'I/O write', IO_TITLE)}
            ${th('net', 'Network', NET_TITLE)}
            ${th('last', 'Last seen', '', '')}
          </tr></thead>
          <tbody></tbody>
        </table>
        <div class="empty lv-empty" hidden></div>
      </div>
    </div>
  </div>`;

  els.table = root.querySelector('.hist-table');
  els.tbody = els.table.querySelector('tbody');
  els.scroll = root.querySelector('.lv-scroll');
  els.empty = root.querySelector('.lv-empty');
  els.count = root.querySelector('[data-count]');
  els.refresh = root.querySelector('[data-refresh]');
  els.ranges = root.querySelector('[data-ranges]');
  els.custom = root.querySelector('.hist-custom');
  els.days = root.querySelector('.hist-days');
  els.daysMax = root.querySelector('.hist-days-max');
  els.recSwitch = root.querySelector('[data-rec]');
  els.note = root.querySelector('.hist-note');
  els.noteText = root.querySelector('[data-note-text]');
  els.statApps = root.querySelector('[data-stat="apps"]');
  els.statCpu = root.querySelector('[data-stat="cpu"]');
  els.statIo = root.querySelector('[data-stat="io"]');
  els.statSince = root.querySelector('[data-stat="since"]');

  els.ranges.addEventListener('click', (e) => {
    const btn = e.target instanceof Element ? e.target.closest('[data-range]') : null;
    if (!btn) return;
    setRange(btn.dataset.range);
    if (btn.dataset.range === 'custom') els.days.focus();
  });
  els.days.addEventListener('change', applyCustomDays);
  els.days.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') {
      e.preventDefault();
      applyCustomDays();
    }
  });
  els.recSwitch.addEventListener('click', () => setRecording(store.settings.historyEnabled === false));
  root.querySelector('[data-clear]').addEventListener('click', deleteHistory);
  els.refresh.addEventListener('click', () => load());
  els.empty.addEventListener('click', (e) => {
    const btn = e.target instanceof Element ? e.target.closest('[data-empty-act]') : null;
    if (!btn) return;
    if (btn.dataset.emptyAct === 'retry') load();
    if (btn.dataset.emptyAct === 'enable') setRecording(true);
  });
  els.table.querySelector('thead').addEventListener('click', (e) => {
    const btn = e.target instanceof Element ? e.target.closest('[data-sort]') : null;
    if (!btn) return;
    const key = btn.dataset.sort;
    sort = sort.key === key ? { key, dir: -sort.dir } : { key, dir: key === 'name' ? 1 : -1 };
    render();
  });

  let lastDays = rangeDays();
  on('settings', () => {
    sendEnabled();
    const d = rangeDays();
    if (d !== lastDays) {
      lastDays = d;
      if (isVisible()) load();
      else loaded = false;
    }
    if (isVisible()) render();
  });
  on('query', () => { if (isVisible()) render(); });
  document.addEventListener('visibilitychange', () => {
    if (isVisible() && !document.hidden) load();
  });
  if (!unregister) unregister = registerCommands(paletteItems);
  // Tell the backend whether to record, before anything else.
  sendEnabled();
  render();
}

export function onShow() {
  visible = true;
  render();
  load();
  startTimer();
}

export function onHide() {
  visible = false;
  stopTimer();
}

export function isVisible() {
  return visible && !!root;
}
