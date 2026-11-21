// Network > Usage: how much this PC downloaded and uploaded over a period, and which apps
// used it. Loads `get_network_usage({ period })` on show, on period change and every 30 s
// while visible. The machine total needs no admin; per-app bytes exist only for time
// Orbit ran as administrator. Recording follows App history's switch.
// Mounted by views/network.js into its Usage tab: mount(paneEl), onShow(), onHide().

import { call } from '../api.js';
import { store, on, setSettings } from '../store.js';
import { escapeHtml, fuzzy, bytes, time } from '../format.js';
import { restartAsAdmin, toast } from '../app.js';

const REFRESH_MS = 30000;
const PERIOD_KEY = 'orbit.netUsagePeriod';
const PERIODS = [
  { id: 'hour', label: 'Last hour', text: 'in the last hour' },
  { id: 'day', label: '24 hours', text: 'in the last 24 hours' },
  { id: 'week', label: '7 days', text: 'in the last 7 days' },
  { id: 'month', label: '30 days', text: 'in the last 30 days' },
];

let root = null;
const els = {};
let visible = false;
let period = readPeriod();
let data = null;
let loading = false;
let loadError = null;
let again = false;
let timer = 0;
let sort = { key: 'total', dir: -1 };
const icons = new Map();

const icon = (id) => `<svg class="icon" aria-hidden="true"><use href="#${id}"/></svg>`;
const text = (s) => escapeHtml(s == null ? '' : String(s));

function readPeriod() {
  try {
    const v = localStorage.getItem(PERIOD_KEY);
    if (PERIODS.some((p) => p.id === v)) return v;
  } catch (_) { /* private mode */ }
  return 'day';
}

function errText(err) {
  if (typeof err === 'string' && err) return err;
  if (err && typeof err.message === 'string' && err.message) return err.message;
  return 'Could not read network usage. Try again.';
}

function isAdmin() {
  return !!(store.staticInfo && store.staticInfo.isAdmin);
}

function perAppOn() {
  const s = store.snapshot;
  return !!(s && s.network && s.network.perApp && s.network.perApp.available);
}

function periodInfo() {
  return PERIODS.find((p) => p.id === period) || PERIODS[1];
}

// ------------------------------------------------------------------ data

async function load() {
  if (loading) {
    again = true;
    return;
  }
  loading = true;
  const want = period;
  try {
    const r = await call('get_network_usage', { period: want });
    if (want === period) {
      data = r && typeof r === 'object' ? r : null;
      loadError = null;
    }
  } catch (err) {
    if (want === period) loadError = errText(err);
  } finally {
    loading = false;
  }
  if (again || want !== period) {
    again = false;
    load();
    return;
  }
  if (data && Array.isArray(data.apps)) loadIcons(data.apps.map((a) => a.exe));
  if (isVisible()) render();
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
    if (got && isVisible()) renderTable();
  } catch (_) { /* keep the blank tiles */ }
}

function startTimer() {
  stopTimer();
  timer = setInterval(() => {
    if (isVisible() && !document.hidden) load();
  }, REFRESH_MS);
}

function stopTimer() {
  if (timer) clearInterval(timer);
  timer = 0;
}

// ------------------------------------------------------------------ render

function apps() {
  const list = data && Array.isArray(data.apps) ? data.apps : [];
  const q = (store.query || '').trim();
  const shown = q ? list.filter((a) => fuzzy(`${a.name} ${a.exe || ''}`, q) >= 1000) : [...list];
  const get = {
    name: (a) => a.name.toLowerCase(),
    rx: (a) => a.rxBytes,
    tx: (a) => a.txBytes,
    total: (a) => a.rxBytes + a.txBytes,
  }[sort.key];
  shown.sort((a, b) => {
    const x = get(a);
    const y = get(b);
    const c = typeof x === 'string' ? x.localeCompare(y) : x - y;
    return c * sort.dir || (b.rxBytes + b.txBytes) - (a.rxBytes + a.txBytes) || a.key.localeCompare(b.key);
  });
  return shown;
}

function sinceText(secs) {
  if (!secs) return '-';
  const d = new Date(secs * 1000);
  const now = new Date();
  const hm = `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
  if (d.toDateString() === now.toDateString()) return `Today ${hm}`;
  const y = new Date(now);
  y.setDate(now.getDate() - 1);
  if (d.toDateString() === y.toDateString()) return `Yesterday ${hm}`;
  return time(secs).split(', ')[1] || time(secs);
}

function renderControls() {
  for (const btn of els.periods.querySelectorAll('[data-period]')) {
    const on = btn.dataset.period === period;
    btn.classList.toggle('chip--on', on);
    btn.setAttribute('aria-pressed', on ? 'true' : 'false');
  }
  for (const th of els.table.querySelectorAll('th[data-col]')) {
    const active = th.dataset.col === sort.key;
    th.classList.toggle('is-sorted-asc', active && sort.dir === 1);
    th.classList.toggle('is-sorted-desc', active && sort.dir === -1);
    th.setAttribute('aria-sort', active ? (sort.dir === 1 ? 'ascending' : 'descending') : 'none');
  }
}

function renderStats() {
  const has = !!data && !loadError;
  els.down.textContent = has ? bytes(data.rxBytes) : '-';
  els.up.textContent = has ? bytes(data.txBytes) : '-';
  els.apps.textContent = has ? String(data.apps.length) : '-';
  els.since.textContent = has ? sinceText(data.since) : '-';
  els.since.title = has && data.since ? time(data.since) : '';
}

/** One line under the totals: what is missing and what the user can do about it. */
function renderNote() {
  let html = '';
  if (data && !loadError) {
    const none = !data.apps.length;
    if (data.recording === false) {
      html = `${icon('i-history')}<p>Recording is off, so Orbit is not counting network use.</p>
        <button type="button" class="btn btn--ghost btn--sm" data-nu-act="record">Turn on recording</button>`;
    } else if (!perAppOn() && isAdmin()) {
      html = `${icon('i-shield')}<p>Per-app numbers are not available right now; the totals still count.</p>`;
    } else if (!perAppOn() && none) {
      html = `${icon('i-shield')}<p>Download and upload per app needs administrator rights.</p>
        ${isAdmin() ? '' : '<button type="button" class="btn btn--ghost btn--sm" data-nu-act="admin">Run as admin</button>'}`;
    } else if (!perAppOn()) {
      html = `${icon('i-shield')}<p>Per-app numbers cover only the time Orbit ran as administrator.</p>
        ${isAdmin() ? '' : '<button type="button" class="btn btn--ghost btn--sm" data-nu-act="admin">Run as admin</button>'}`;
    } else {
      html = `${icon('i-lock')}<p>Orbit counts network use only while it runs, and it stays on this PC.</p>`;
    }
  }
  if (els.note.innerHTML !== html) els.note.innerHTML = html;
  els.note.hidden = !html;
}

function iconCell(exe) {
  const src = exe ? icons.get(exe) : '';
  if (src) return `<img class="lv-ico" src="${src}" alt="" width="18" height="18" decoding="async">`;
  return `<span class="lv-ico lv-ico--blank" aria-hidden="true">${icon('i-grid')}</span>`;
}

function showEmpty(title, body, action) {
  els.table.classList.add('is-empty');
  els.empty.hidden = false;
  els.empty.innerHTML = `${icon('i-plug')}
    <p class="empty-title lv-empty-title">${text(title)}</p>
    ${body ? `<p class="lv-empty-body">${text(body)}</p>` : ''}
    ${action ? `<button type="button" class="btn btn--ghost btn--sm" data-nu-act="${action.id}">${text(action.label)}</button>` : ''}`;
}

function renderTable() {
  const list = apps();
  const all = data && Array.isArray(data.apps) ? data.apps : [];
  const sum = all.reduce((s, a) => s + a.rxBytes + a.txBytes, 0);
  const max = Math.max(1, ...list.map((a) => a.rxBytes + a.txBytes));
  els.count.textContent = data && !loadError && all.length ? (list.length === all.length ? `${all.length} apps` : `${list.length} of ${all.length} apps`) : '';
  if (!list.length) {
    els.tbody.innerHTML = '';
    const p = periodInfo();
    if (loadError) showEmpty('Could not read network usage.', loadError, { id: 'retry', label: 'Try again' });
    else if (!data) showEmpty('Reading network usage.', '');
    else if (all.length) showEmpty('No app matches the search.', '', { id: 'clearsearch', label: 'Clear search' });
    else if (!perAppOn()) showEmpty(`No per-app numbers ${p.text}.`, 'Orbit counts each app only while it runs as administrator.');
    else showEmpty(`No app used the network ${p.text}.`, 'Orbit adds up what each app downloads and uploads while it runs.');
    return;
  }
  els.table.classList.remove('is-empty');
  els.empty.hidden = true;
  els.empty.innerHTML = '';
  els.tbody.innerHTML = list.map((a) => {
    const total = a.rxBytes + a.txBytes;
    const w = Math.max(2, Math.round((total / max) * 100));
    const share = sum > 0 ? (total / sum) * 100 : 0;
    return `<tr class="row" data-key="${text(a.key)}">
      <td class="c-name"><div class="lv-namecell">
        ${iconCell(a.exe)}
        <span class="lv-primary" title="${text(a.exe || a.name)}">${text(a.name)}</span>
      </div></td>
      <td class="c-rx cell-num"><span class="lv-num">${text(bytes(a.rxBytes))}</span></td>
      <td class="c-tx cell-num"><span class="lv-num">${text(bytes(a.txBytes))}</span></td>
      <td class="c-total cell-num"><div class="nu-total">
        <span class="nu-bar" aria-hidden="true"><span style="width:${w}%"></span></span>
        <span class="lv-num nu-total-val">${text(bytes(total))}</span>
      </div></td>
      <td class="c-share cell-num"><span class="lv-num">${share >= 0.1 ? `${share.toFixed(1)}%` : '<0.1%'}</span></td>
    </tr>`;
  }).join('');
}

function render() {
  if (!root) return;
  renderControls();
  renderStats();
  renderNote();
  renderTable();
}

// ------------------------------------------------------------------ view API

function setPeriod(id) {
  if (id === period || !PERIODS.some((p) => p.id === id)) return;
  period = id;
  try { localStorage.setItem(PERIOD_KEY, id); } catch (_) { /* private mode */ }
  data = null;
  loadError = null;
  render();
  load();
}

function onAct(e) {
  const btn = e.target instanceof Element ? e.target.closest('[data-nu-act]') : null;
  if (!btn) return;
  const act = btn.dataset.nuAct;
  if (act === 'admin') restartAsAdmin();
  else if (act === 'retry') load();
  else if (act === 'record') {
    setSettings({ historyEnabled: true });
    toast('Recording app history and network use.', 'info');
    setTimeout(() => { if (isVisible()) load(); }, 300);
  } else if (act === 'clearsearch') {
    const input = document.getElementById('search');
    if (input) {
      input.value = '';
      input.dispatchEvent(new Event('input', { bubbles: true }));
    }
  }
}

export function mount(paneEl) {
  root = paneEl;
  root.innerHTML = `
    <div class="toolbar lv-toolbar nu-toolbar">
      <div class="lv-chips" role="group" aria-label="Period" data-periods>
        ${PERIODS.map((p) => `<button type="button" class="chip" data-period="${p.id}" aria-pressed="false">${p.label}</button>`).join('')}
      </div>
      <span class="lv-toolbar-spacer"></span>
      <span class="micro lv-count" data-count aria-live="polite"></span>
      <button type="button" class="icon-btn lv-refresh" data-refresh aria-label="Refresh usage" title="Refresh (updates every 30 seconds)">${icon('i-refresh')}</button>
    </div>
    <div class="nu-sum">
      <div class="lv-stats" aria-live="polite">
        <div class="stat lv-stat"><span class="micro stat-label">Download</span><span class="stat-value lv-stat-value" data-nu="down">-</span></div>
        <div class="stat lv-stat"><span class="micro stat-label">Upload</span><span class="stat-value lv-stat-value" data-nu="up">-</span></div>
        <div class="stat lv-stat"><span class="micro stat-label">Apps</span><span class="stat-value lv-stat-value" data-nu="apps">-</span></div>
        <div class="stat lv-stat" title="When Orbit started counting in this period"><span class="micro stat-label">Since</span><span class="stat-value lv-stat-value" data-nu="since">-</span></div>
      </div>
      <div class="nu-note" data-nu-note hidden></div>
    </div>
    <div class="panel lv-table-panel">
      <div class="lv-scroll">
        <table class="table lv-table nu-table">
          <thead><tr>
            <th class="c-name" scope="col" data-col="name" aria-sort="none"><button type="button" class="lv-sort" data-sort="name">App</button></th>
            <th class="c-rx cell-num" scope="col" data-col="rx" aria-sort="none"><button type="button" class="lv-sort" data-sort="rx">Download</button></th>
            <th class="c-tx cell-num" scope="col" data-col="tx" aria-sort="none"><button type="button" class="lv-sort" data-sort="tx">Upload</button></th>
            <th class="c-total cell-num" scope="col" data-col="total" aria-sort="none"><button type="button" class="lv-sort" data-sort="total">Total</button></th>
            <th class="c-share cell-num" scope="col"><span title="Share of every app's total in this period">Share</span></th>
          </tr></thead>
          <tbody></tbody>
        </table>
        <div class="empty lv-empty" hidden></div>
      </div>
    </div>`;
  els.periods = root.querySelector('[data-periods]');
  els.count = root.querySelector('[data-count]');
  els.table = root.querySelector('.nu-table');
  els.tbody = els.table.querySelector('tbody');
  els.empty = root.querySelector('.lv-empty');
  els.note = root.querySelector('[data-nu-note]');
  els.down = root.querySelector('[data-nu="down"]');
  els.up = root.querySelector('[data-nu="up"]');
  els.apps = root.querySelector('[data-nu="apps"]');
  els.since = root.querySelector('[data-nu="since"]');

  els.periods.addEventListener('click', (e) => {
    const btn = e.target instanceof Element ? e.target.closest('[data-period]') : null;
    if (btn) setPeriod(btn.dataset.period);
  });
  root.querySelector('[data-refresh]').addEventListener('click', () => load());
  els.table.querySelector('thead').addEventListener('click', (e) => {
    const btn = e.target instanceof Element ? e.target.closest('[data-sort]') : null;
    if (!btn) return;
    const key = btn.dataset.sort;
    sort = sort.key === key ? { key, dir: -sort.dir } : { key, dir: key === 'name' ? 1 : -1 };
    render();
  });
  els.note.addEventListener('click', onAct);
  els.empty.addEventListener('click', onAct);
  on('query', () => { if (isVisible()) renderTable(); });
  // Per-app rates start a moment after Orbit runs as administrator: redo the note then.
  let perApp = perAppOn();
  on('snapshot', () => {
    if (perApp === perAppOn()) return;
    perApp = perAppOn();
    if (isVisible()) render();
  });
  let recording = store.settings.historyEnabled !== false;
  on('settings', () => {
    const now = store.settings.historyEnabled !== false;
    if (now === recording) return;
    recording = now;
    if (isVisible()) load();
  });
  document.addEventListener('visibilitychange', () => {
    if (isVisible() && !document.hidden) load();
  });
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

function isVisible() {
  return visible && !!root;
}
