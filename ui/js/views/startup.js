// 04 / Startup: every Run key and Startup folder entry, with on/off switches.
import { call } from '../api.js';
import { store, on, setQuery } from '../store.js';
import { escapeHtml, fuzzy, time } from '../format.js';
import { toast, showMenu } from '../app.js';

const FILTERS = [
  { id: 'all', label: 'All', test: () => true },
  { id: 'enabled', label: 'Enabled', test: (e) => e.enabled },
  { id: 'disabled', label: 'Disabled', test: (e) => !e.enabled },
  { id: 'admin', label: 'Needs admin', test: (e) => e.needsAdmin },
];

const collator = new Intl.Collator(undefined, { sensitivity: 'base', numeric: true });

// Startup impact, measured by Orbit at a sign-in (impact.rs). null = not measured.
const IMPACT = {
  high: { label: 'High', chip: 'chip--danger', rank: 3 },
  medium: { label: 'Medium', chip: 'chip--warn', rank: 2 },
  low: { label: 'Low', chip: 'chip--ok', rank: 1 },
};
const impactRank = (e) => (IMPACT[e.impact] ? IMPACT[e.impact].rank : 0);

const SORTERS = {
  name: (a, b) => collator.compare(a.name, b.name),
  command: (a, b) => collator.compare(a.command, b.command),
  location: (a, b) => collator.compare(a.location, b.location) || collator.compare(a.name, b.name),
  // Ascending shows High first, then Medium, Low and Not measured.
  impact: (a, b) => (impactRank(b) - impactRank(a)) || collator.compare(a.name, b.name),
  status: (a, b) => (Number(b.enabled) - Number(a.enabled)) || collator.compare(a.name, b.name),
};

let root = null;
const els = {};
let visible = false;
let dirty = false;
let loaded = false;
let loading = false;
let loadError = null;
let entries = [];
let viewRows = [];
let viewRowsPrev = []; // rows as currently rendered (data-i indexes into this)
const icons = new Map(); // exe path -> data URL
const busy = new Set(); // entry ids with a toggle in flight
let filter = 'all';
let sort = { key: 'name', dir: 1 };
let measureScheduled = false; // a one-shot RunOnce measurement waits for the next sign-in
let measureBusy = false;

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

function matches(haystack, needle) {
  if (!haystack) return false;
  const r = fuzzy(String(haystack), needle);
  return typeof r === 'number' ? r >= 0 : Boolean(r);
}

function includes(haystack, needle) {
  return Boolean(haystack) && String(haystack).toLowerCase().includes(needle);
}

function icon(id) {
  return `<svg class="icon" aria-hidden="true"><use href="#${id}"/></svg>`;
}

async function copyText(value) {
  try {
    await navigator.clipboard.writeText(value);
    return true;
  } catch {
    const ta = document.createElement('textarea');
    ta.value = value;
    ta.setAttribute('readonly', '');
    ta.style.position = 'fixed';
    ta.style.opacity = '0';
    document.body.appendChild(ta);
    ta.select();
    let ok = false;
    try { ok = document.execCommand('copy'); } catch { ok = false; }
    ta.remove();
    return ok;
  }
}

function clearSearch() {
  const input = document.getElementById('search');
  if (input && input.value) {
    input.value = '';
    // Let the titlebar search update its own state (clear button, hint).
    input.dispatchEvent(new Event('input', { bubbles: true }));
  }
  if ((store.query || '') !== '') setQuery('');
}

/* ---------- data ---------- */

async function load() {
  if (loading) return;
  loading = true;
  setRefreshBusy(true);
  if (!loaded) render();
  try {
    const list = await call('list_startup');
    entries = Array.isArray(list) ? list : [];
    loadError = null;
  } catch (err) {
    loadError = errMsg(err);
    if (loaded) toast(loadError, 'error');
  }
  try {
    measureScheduled = (await call('get_startup_measure')) === true;
  } catch {
    measureScheduled = false;
  } finally {
    loaded = true;
    loading = false;
    setRefreshBusy(false);
    if (visible) render(); else dirty = true;
  }
  loadIcons();
}

async function loadIcons() {
  const wanted = [];
  for (const e of entries) {
    if (e.path && !icons.has(e.path) && !wanted.includes(e.path)) wanted.push(e.path);
  }
  if (!wanted.length) return;
  try {
    const result = await call('get_icons', { paths: wanted });
    if (!result || typeof result !== 'object') return;
    let changed = false;
    for (const path of wanted) {
      const url = result[path];
      if (typeof url === 'string' && url.startsWith('data:image/')) {
        icons.set(path, url);
        changed = true;
      }
    }
    if (changed) {
      if (visible) render(); else dirty = true;
    }
  } catch {
    // Icons are decoration; the fallback glyph stays.
  }
}

function computeRows() {
  const q = (store.query || '').trim().toLowerCase();
  const f = FILTERS.find((x) => x.id === filter) || FILTERS[0];
  const counts = { all: 0, enabled: 0, disabled: 0, admin: 0 };
  const out = [];
  for (const e of entries) {
    if (q && !(matches(e.name, q) || includes(e.command, q) || includes(e.location, q))) continue;
    counts.all++;
    if (e.enabled) counts.enabled++; else counts.disabled++;
    if (e.needsAdmin) counts.admin++;
    if (f.test(e)) out.push(e);
  }
  const cmp = SORTERS[sort.key] || SORTERS.name;
  out.sort((a, b) => cmp(a, b) * sort.dir);
  viewRows = out;
  return counts;
}

/* ---------- actions ---------- */

async function toggle(entry) {
  if (!entry || busy.has(entry.id)) return;
  const next = !entry.enabled;
  entry.enabled = next;
  busy.add(entry.id);
  render();
  try {
    await call('set_startup_enabled', { id: entry.id, enabled: next });
    toast(next ? `${entry.name} will start with Windows.` : `${entry.name} will no longer start with Windows.`, 'success');
  } catch (err) {
    entry.enabled = !next;
    toast(errMsg(err), 'error');
  } finally {
    busy.delete(entry.id);
    if (visible) render(); else dirty = true;
  }
}

async function openLocation(entry) {
  if (!entry.path) return;
  try {
    await call('open_location', { path: entry.path });
  } catch (err) {
    toast(errMsg(err), 'error');
  }
}

async function searchOnline(entry) {
  try {
    await call('search_online', { query: `${entry.name} startup program` });
  } catch (err) {
    toast(errMsg(err), 'error');
  }
}

async function setMeasure(on) {
  if (measureBusy) return;
  measureBusy = true;
  renderMeasure();
  try {
    await call(on ? 'schedule_startup_measure' : 'cancel_startup_measure');
    measureScheduled = on;
    toast(on ? 'Measurement scheduled.' : 'Measurement cancelled.', on ? 'success' : 'info');
  } catch (err) {
    toast(errMsg(err), 'error');
  } finally {
    const from = on ? els.measure : els.noteCancel;
    const hadFocus = document.activeElement === from;
    measureBusy = false;
    renderMeasure();
    // The button that was clicked is now hidden: keep focus on its counterpart.
    if (hadFocus && from.closest('[hidden]')) (on ? els.noteCancel : els.measure).focus({ preventScroll: true });
  }
}

async function copyCommand(entry) {
  const ok = await copyText(entry.command || '');
  toast(ok ? 'Command copied.' : 'Could not copy the command.', ok ? 'success' : 'error');
}

function menuItems(entry) {
  return [
    {
      label: entry.enabled ? 'Disable' : 'Enable',
      icon: entry.enabled ? 'i-x' : 'i-check',
      action: () => toggle(entry),
      disabled: busy.has(entry.id),
    },
    { separator: true },
    { label: 'Open file location', icon: 'i-folder', action: () => openLocation(entry), disabled: !entry.path },
    { label: 'Search online', icon: 'i-globe', action: () => searchOnline(entry) },
    { label: 'Copy command', icon: 'i-copy', action: () => copyCommand(entry), disabled: !entry.command },
  ];
}

function openMenuFor(entry, x, y) {
  showMenu(x, y, menuItems(entry));
}

/* ---------- render ---------- */

function setRefreshBusy(on) {
  if (!els.refresh) return;
  els.refresh.classList.toggle('is-busy', on);
  els.refresh.setAttribute('aria-busy', on ? 'true' : 'false');
}

function impactHtml(e) {
  const k = IMPACT[e.impact];
  if (!k) return '<span class="lv-impact-none">Not measured</span>';
  const when = e.impactMeasuredAt ? ` title="Measured ${attr(time(e.impactMeasuredAt))}"` : '';
  return `<span class="chip lv-chip ${k.chip}"${when}>${k.label}</span>`;
}

/** Latest measurement time over all entries (seconds), or null. */
function measuredAt() {
  let t = null;
  for (const e of entries) if (e.impactMeasuredAt && (t === null || e.impactMeasuredAt > t)) t = e.impactMeasuredAt;
  return t;
}

function renderMeasure() {
  if (!els.measure) return;
  els.measure.hidden = measureScheduled;
  els.measure.disabled = measureBusy;
  els.note.hidden = !measureScheduled;
  els.noteCancel.disabled = measureBusy;
  const t = loaded ? measuredAt() : null;
  els.when.textContent = t ? `Impact / ${time(t)}` : loaded ? 'Impact / Not measured' : '';
  els.when.title = t ? `Startup impact was measured at the sign-in on ${time(t)}.` : 'Orbit has not measured startup impact yet.';
}

function rowHtml(e, i) {
  const pending = busy.has(e.id);
  const iconUrl = e.path ? icons.get(e.path) : null;
  const ico = iconUrl
    ? `<img class="lv-ico" src="${attr(iconUrl)}" alt="" width="18" height="18" decoding="async">`
    : `<span class="lv-ico lv-ico--blank" aria-hidden="true">${icon('i-rocket')}</span>`;
  const admin = e.needsAdmin
    ? `<span class="lv-admin" title="Changing this entry needs administrator rights">${icon('i-shield')}<span class="lv-sr">Needs admin</span></span>`
    : '';
  return `<tr class="row${pending ? ' is-pending' : ''}" data-i="${i}" aria-busy="${pending}">
    <td class="c-toggle"><button type="button" class="switch" role="switch" data-act="toggle"
      aria-checked="${e.enabled}" aria-label="Start ${attr(e.name)} with Windows"${pending ? ' aria-disabled="true"' : ''}><span class="switch-thumb" aria-hidden="true"></span></button></td>
    <td class="c-name"><div class="lv-namecell">${ico}<span class="lv-primary" title="${attr(e.name)}">${text(e.name)}</span></div></td>
    <td class="c-cmd"><span class="lv-mono" title="${attr(e.command)}">${text(e.command)}</span></td>
    <td class="c-loc"><div class="lv-loccell"><span class="chip lv-chip">${text(e.location)}</span>${admin}</div></td>
    <td class="c-impact">${impactHtml(e)}</td>
    <td class="c-status"><span class="lv-status ${e.enabled ? 'lv-status--on' : 'lv-status--off'}">${e.enabled ? 'Enabled' : 'Disabled'}</span></td>
    <td class="c-more"><button type="button" class="icon-btn lv-more" data-act="menu" aria-label="More actions for ${attr(e.name)}" title="More actions">${icon('i-more')}</button></td>
  </tr>`;
}

function showEmpty(title, body, action) {
  els.tbody.innerHTML = '';
  els.table.classList.add('is-empty');
  els.empty.hidden = false;
  els.empty.innerHTML = `${icon('i-rocket')}
    <p class="empty-title lv-empty-title">${text(title)}</p>
    ${body ? `<p class="lv-empty-body">${text(body)}</p>` : ''}
    ${action ? `<button type="button" class="btn btn--ghost btn--sm" data-empty-act="${action.id}">${text(action.label)}</button>` : ''}`;
}

function hideEmpty() {
  els.empty.hidden = true;
  els.empty.innerHTML = '';
  els.table.classList.remove('is-empty');
}

function updateSortHeaders() {
  for (const btn of els.thead.querySelectorAll('button[data-sort]')) {
    const th = btn.closest('th');
    const active = btn.dataset.sort === sort.key;
    th.classList.toggle('is-sorted-asc', active && sort.dir === 1);
    th.classList.toggle('is-sorted-desc', active && sort.dir === -1);
    th.setAttribute('aria-sort', active ? (sort.dir === 1 ? 'ascending' : 'descending') : 'none');
  }
}

function render() {
  if (!root) return;
  dirty = false;
  const counts = computeRows();

  let enabled = 0;
  for (const e of entries) if (e.enabled) enabled++;
  els.statEnabled.textContent = loaded ? String(enabled) : '-';
  els.statDisabled.textContent = loaded ? String(entries.length - enabled) : '-';

  for (const btn of els.chips.querySelectorAll('[data-filter]')) {
    const id = btn.dataset.filter;
    const on = id === filter;
    btn.classList.toggle('chip--on', on);
    btn.setAttribute('aria-pressed', on ? 'true' : 'false');
    const c = btn.querySelector('.lv-chip-count');
    if (c) c.textContent = loaded ? String(counts[id]) : '';
  }
  els.count.textContent = loaded && !loadError ? `${viewRows.length} of ${entries.length}` : '';
  updateSortHeaders();
  renderMeasure();

  if (!loaded) {
    showEmpty('Reading startup entries', 'Checking the Run keys and both Startup folders.');
    return;
  }
  if (loadError && !entries.length) {
    showEmpty('Could not read startup entries', loadError, { id: 'retry', label: 'Try again' });
    return;
  }
  if (!entries.length) {
    showEmpty('No startup entries', 'Nothing is set to start with Windows from the Run keys or Startup folders.');
    return;
  }
  if (!viewRows.length) {
    const q = (store.query || '').trim();
    showEmpty(
      q ? `Nothing matches "${q}"` : 'Nothing in this filter',
      q ? 'Search looks at names, commands and locations.' : 'Try another filter.',
      { id: 'clear', label: 'Clear filters' },
    );
    return;
  }
  hideEmpty();

  // Keep keyboard focus on the same control across the rebuild.
  let focusId = null;
  let focusAct = null;
  const active = document.activeElement;
  if (active && els.tbody.contains(active)) {
    const tr = active.closest('tr[data-i]');
    const prev = tr ? viewRowsPrev[Number(tr.dataset.i)] : null;
    focusId = prev ? prev.id : null;
    focusAct = active.dataset.act || null;
  }

  const parts = new Array(viewRows.length);
  for (let i = 0; i < viewRows.length; i++) parts[i] = rowHtml(viewRows[i], i);
  els.tbody.innerHTML = parts.join('');
  els.table.setAttribute('aria-rowcount', String(viewRows.length + 1));
  viewRowsPrev = viewRows;

  if (focusId) {
    const idx = viewRows.findIndex((e) => e.id === focusId);
    if (idx >= 0) {
      const tr = els.tbody.children[idx];
      const target = (focusAct && tr.querySelector(`[data-act="${focusAct}"]`)) || tr.querySelector('[data-act]');
      if (target) target.focus({ preventScroll: true });
    }
  }
}

function entryFromEvent(target) {
  const tr = target.closest('tr[data-i]');
  if (!tr || !els.tbody.contains(tr)) return null;
  return viewRowsPrev[Number(tr.dataset.i)] || null;
}

/* ---------- mount ---------- */

export function mount(rootEl) {
  root = rootEl;
  // The table runs its own scroller inside the view (app.css .view--fill).
  root.classList.add('view--fill');
  root.innerHTML = `
  <div class="lv lv--startup">
    <header class="view-head">
      <div class="lv-head-text">
        <p class="micro">04 / Startup</p>
        <h1 class="view-title">Boot lean. <em>Start fast.</em></h1>
      </div>
      <div class="lv-stats" aria-live="polite">
        <div class="stat lv-stat"><span class="micro stat-label">Enabled</span><span class="stat-value lv-stat-value" data-stat="enabled">-</span></div>
        <div class="stat lv-stat"><span class="micro stat-label">Disabled</span><span class="stat-value lv-stat-value" data-stat="disabled">-</span></div>
      </div>
    </header>
    <div class="toolbar lv-toolbar">
      <div class="lv-chips" role="group" aria-label="Filter startup entries">
        ${FILTERS.map((f) => `<button type="button" class="chip" data-filter="${f.id}" aria-pressed="false">${f.label}<span class="lv-chip-count"></span></button>`).join('')}
      </div>
      <span class="lv-toolbar-spacer"></span>
      <span class="micro lv-impact-when" data-when></span>
      <span class="micro lv-count" aria-live="polite"></span>
      <button type="button" class="btn btn--ghost btn--sm" data-measure title="Orbit measures how much each startup app slows your sign-in, once, the next time you sign in"><span>Measure at next sign-in</span>${icon('i-clock')}</button>
      <button type="button" class="icon-btn lv-refresh" data-refresh aria-label="Refresh startup entries" title="Refresh">${icon('i-refresh')}</button>
    </div>
    <div class="lv-note" data-note role="status" hidden>
      <span class="signal" aria-hidden="true"></span>
      <span class="lv-note-text">Orbit will measure your startup apps the next time you sign in.</span>
      <button type="button" class="btn btn--ghost btn--sm" data-note-cancel><span>Cancel</span>${icon('i-close')}</button>
    </div>
    <div class="panel lv-table-panel">
      <div class="lv-scroll">
        <table class="table lv-table lv-table--startup">
          <thead><tr>
            <th class="c-toggle" scope="col"><span class="lv-sr">Starts with Windows</span></th>
            <th class="c-name" scope="col" aria-sort="none"><button type="button" class="lv-sort" data-sort="name">Name</button></th>
            <th class="c-cmd" scope="col" aria-sort="none"><button type="button" class="lv-sort" data-sort="command">Command</button></th>
            <th class="c-loc" scope="col" aria-sort="none"><button type="button" class="lv-sort" data-sort="location">Location</button></th>
            <th class="c-impact" scope="col" aria-sort="none"><button type="button" class="lv-sort" data-sort="impact" title="CPU time and disk reads in the app's first minute after sign-in, measured by Orbit">Impact</button></th>
            <th class="c-status" scope="col" aria-sort="none"><button type="button" class="lv-sort" data-sort="status">Status</button></th>
            <th class="c-more" scope="col"><span class="lv-sr">Actions</span></th>
          </tr></thead>
          <tbody></tbody>
        </table>
        <div class="empty lv-empty" hidden></div>
      </div>
    </div>
  </div>`;

  els.statEnabled = root.querySelector('[data-stat="enabled"]');
  els.statDisabled = root.querySelector('[data-stat="disabled"]');
  els.chips = root.querySelector('.lv-chips');
  els.count = root.querySelector('.lv-count');
  els.refresh = root.querySelector('[data-refresh]');
  els.table = root.querySelector('table');
  els.thead = els.table.tHead;
  els.tbody = els.table.tBodies[0];
  els.empty = root.querySelector('.lv-empty');
  els.measure = root.querySelector('[data-measure]');
  els.note = root.querySelector('[data-note]');
  els.noteCancel = root.querySelector('[data-note-cancel]');
  els.when = root.querySelector('[data-when]');

  els.measure.addEventListener('click', () => setMeasure(true));
  els.noteCancel.addEventListener('click', () => setMeasure(false));

  els.chips.addEventListener('click', (ev) => {
    const btn = ev.target.closest('[data-filter]');
    if (!btn) return;
    filter = btn.dataset.filter;
    render();
  });

  els.refresh.addEventListener('click', () => load());

  // Sort headers are buttons, so Enter and Space work natively.
  els.thead.addEventListener('click', (ev) => {
    const btn = ev.target.closest('button[data-sort]');
    if (!btn) return;
    const key = btn.dataset.sort;
    if (sort.key === key) sort.dir = -sort.dir;
    else sort = { key, dir: 1 };
    render();
  });

  els.tbody.addEventListener('click', (ev) => {
    const btn = ev.target.closest('[data-act]');
    if (!btn) return;
    const entry = entryFromEvent(btn);
    if (!entry) return;
    if (btn.dataset.act === 'toggle') {
      if (btn.getAttribute('aria-disabled') === 'true') return;
      toggle(entry);
    } else if (btn.dataset.act === 'menu') {
      const r = btn.getBoundingClientRect();
      openMenuFor(entry, r.right, r.bottom + 4);
    }
  });

  els.tbody.addEventListener('contextmenu', (ev) => {
    const entry = entryFromEvent(ev.target);
    if (!entry) return;
    ev.preventDefault();
    let { clientX: x, clientY: y } = ev;
    if (!x && !y) {
      const r = ev.target.getBoundingClientRect();
      x = r.left + 12;
      y = r.bottom;
    }
    openMenuFor(entry, x, y);
  });

  els.empty.addEventListener('click', (ev) => {
    const btn = ev.target.closest('[data-empty-act]');
    if (!btn) return;
    if (btn.dataset.emptyAct === 'retry') load();
    else if (btn.dataset.emptyAct === 'clear') {
      filter = 'all';
      if ((store.query || '').trim()) clearSearch();
      render();
    }
  });

  on('query', () => {
    if (visible) render(); else dirty = true;
  });

  render();
}

export function onShow() {
  visible = true;
  if (!loaded && !loading) load();
  else if (dirty) render();
}

export function onHide() {
  visible = false;
}
