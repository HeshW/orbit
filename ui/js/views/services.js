// 04 / Services: every Windows service with start, stop and restart.
import { call } from '../api.js';
import { store, on, setQuery, selectProcess } from '../store.js';
import { escapeHtml, fuzzy } from '../format.js';
import { toast, showMenu, navigate } from '../app.js';

const REFRESH_MS = 5000;
const PENDING_RECHECK_MS = 1500;
const VIRTUAL_THRESHOLD = 200; // render every row up to this many
const OVERSCAN = 12;
const FIRST_CHUNK = 60;
const COLS = 7;

const FILTERS = [
  { id: 'all', label: 'All', test: () => true },
  { id: 'running', label: 'Running', test: (s) => s.status === 'running' },
  { id: 'stopped', label: 'Stopped', test: (s) => s.status === 'stopped' },
  { id: 'auto', label: 'Auto start', test: (s) => s.startType === 'auto' || s.startType === 'autoDelayed' },
  { id: 'disabled', label: 'Disabled', test: (s) => s.startType === 'disabled' },
];

const STATUS_LABEL = {
  running: 'Running',
  stopped: 'Stopped',
  startPending: 'Starting',
  stopPending: 'Stopping',
  paused: 'Paused',
  pausePending: 'Pausing',
  continuePending: 'Resuming',
  unknown: 'Unknown',
};
const STATUS_ORDER = {
  running: 0, startPending: 1, continuePending: 1, stopPending: 1, pausePending: 1, paused: 2, stopped: 3, unknown: 4,
};
const START_LABEL = {
  auto: 'Automatic',
  autoDelayed: 'Automatic (delayed)',
  manual: 'Manual',
  disabled: 'Disabled',
  boot: 'Boot',
  system: 'System',
  unknown: 'Unknown',
};
const START_ORDER = { boot: 0, system: 1, auto: 2, autoDelayed: 3, manual: 4, disabled: 5, unknown: 6 };
const PENDING_LABEL = { start: 'Starting', stop: 'Stopping', restart: 'Restarting' };
const DONE_LABEL = { start: 'started', stop: 'stopped', restart: 'restarted' };

const collator = new Intl.Collator(undefined, { sensitivity: 'base', numeric: true });
const byDisplay = (a, b) => collator.compare(a.displayName || a.name, b.displayName || b.name);

const SORTERS = {
  name: byDisplay,
  status: (a, b) => ((STATUS_ORDER[a.status] ?? 9) - (STATUS_ORDER[b.status] ?? 9)) || byDisplay(a, b),
  startType: (a, b) => ((START_ORDER[a.startType] ?? 9) - (START_ORDER[b.startType] ?? 9)) || byDisplay(a, b),
  pid: (a, b) => ((a.pid || 0) - (b.pid || 0)) || byDisplay(a, b),
  description: (a, b) => collator.compare(a.description || '', b.description || '') || byDisplay(a, b),
};

let root = null;
const els = {};
let visible = false;
let dirty = false;
let loaded = false;
let loading = false;
let reloadQueued = false;
let loadError = null;
let services = [];
let byName = new Map();
let viewRows = [];
const pending = new Map(); // service name -> 'start' | 'stop' | 'restart'
const rowCache = new Map(); // service name -> { tr, sig }
let filter = 'all';
let sort = { key: 'name', dir: 1 };
let rowH = 0;
let remeasure = false;
let lastCompact = false;
let timer = 0;
let followUp = 0;
let rafId = 0;
let spacerTop = null;
let spacerBottom = null;

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

function isPendingStatus(status) {
  return status === 'startPending' || status === 'stopPending' || status === 'pausePending' || status === 'continuePending';
}

function canStart(s) {
  return !pending.has(s.name) && s.status === 'stopped' && s.startType !== 'disabled';
}
function canStop(s) {
  return !pending.has(s.name) && (s.status === 'running' || s.status === 'paused');
}
function canRestart(s) {
  return !pending.has(s.name) && s.status === 'running';
}
function hasPid(s) {
  return typeof s.pid === 'number' && s.pid > 0;
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

async function load({ silent = false } = {}) {
  if (loading) {
    reloadQueued = true;
    return;
  }
  loading = true;
  if (!silent) setRefreshBusy(true);
  try {
    const list = await call('list_services');
    services = Array.isArray(list) ? list : [];
    byName = new Map(services.map((s) => [s.name, s]));
    for (const name of rowCache.keys()) {
      if (!byName.has(name)) rowCache.delete(name);
    }
    loadError = null;
  } catch (err) {
    loadError = errMsg(err);
    if (!silent && loaded) toast(loadError, 'error');
  } finally {
    loaded = true;
    loading = false;
    setRefreshBusy(false);
    if (visible) render(); else dirty = true;
  }
  if (reloadQueued) {
    reloadQueued = false;
    load({ silent: true });
  } else if (visible && !followUp && services.some((s) => isPendingStatus(s.status))) {
    // A service is mid-transition: check back soon instead of waiting for the 5 s tick.
    followUp = setTimeout(() => {
      followUp = 0;
      if (visible) load({ silent: true });
    }, PENDING_RECHECK_MS);
  }
}

function computeRows() {
  const q = (store.query || '').trim().toLowerCase();
  const f = FILTERS.find((x) => x.id === filter) || FILTERS[0];
  const counts = { all: 0, running: 0, stopped: 0, auto: 0, disabled: 0 };
  const out = [];
  for (const s of services) {
    if (q && !(
      matches(s.displayName, q) || matches(s.name, q) || includes(s.description, q)
      || (hasPid(s) && String(s.pid).includes(q))
    )) continue;
    counts.all++;
    if (s.status === 'running') counts.running++;
    else if (s.status === 'stopped') counts.stopped++;
    if (s.startType === 'auto' || s.startType === 'autoDelayed') counts.auto++;
    else if (s.startType === 'disabled') counts.disabled++;
    if (f.test(s)) out.push(s);
  }
  const cmp = SORTERS[sort.key] || SORTERS.name;
  out.sort((a, b) => cmp(a, b) * sort.dir);
  viewRows = out;
  return counts;
}

/* ---------- actions ---------- */

async function control(svc, action) {
  if (!svc || pending.has(svc.name)) return;
  const ok = action === 'start' ? canStart(svc) : action === 'stop' ? canStop(svc) : canRestart(svc);
  if (!ok) return;
  const label = svc.displayName || svc.name;
  pending.set(svc.name, action);
  refreshRows();
  try {
    await call('control_service', { name: svc.name, action });
    toast(`${label} ${DONE_LABEL[action]}.`, 'success');
  } catch (err) {
    toast(errMsg(err), 'error');
  } finally {
    pending.delete(svc.name);
    refreshRows();
    load({ silent: true });
  }
}

function goToProcess(svc) {
  if (!hasPid(svc)) return;
  // The search is global and matches service names, not process names: clear it so
  // the process row is not filtered out of the Processes table.
  clearSearch();
  navigate('processes');
  selectProcess(svc.pid);
}

async function searchOnline(svc) {
  try {
    await call('search_online', { query: `${svc.displayName || svc.name} (${svc.name}) Windows service` });
  } catch (err) {
    toast(errMsg(err), 'error');
  }
}

async function copyName(svc) {
  const ok = await copyText(svc.name);
  toast(ok ? `Copied ${svc.name}.` : 'Could not copy the name.', ok ? 'success' : 'error');
}

function menuItems(svc) {
  return [
    { label: 'Start', icon: 'i-play', action: () => control(svc, 'start'), disabled: !canStart(svc) },
    { label: 'Stop', icon: 'i-stop', action: () => control(svc, 'stop'), disabled: !canStop(svc) },
    { label: 'Restart', icon: 'i-refresh', action: () => control(svc, 'restart'), disabled: !canRestart(svc) },
    { separator: true },
    { label: 'Go to process', icon: 'i-arrow-right', action: () => goToProcess(svc), disabled: !hasPid(svc) },
    { label: 'Copy name', icon: 'i-copy', action: () => copyName(svc) },
    { label: 'Search online', icon: 'i-globe', action: () => searchOnline(svc) },
  ];
}

/* ---------- render ---------- */

function setRefreshBusy(on) {
  if (!els.refresh) return;
  els.refresh.classList.toggle('is-busy', on);
  els.refresh.setAttribute('aria-busy', on ? 'true' : 'false');
}

function statusChip(s, act) {
  if (act) {
    return `<span class="chip chip--warn lv-chip">${PENDING_LABEL[act]}</span>`;
  }
  const label = STATUS_LABEL[s.status] || 'Unknown';
  if (s.status === 'running') {
    return `<span class="chip chip--live lv-chip"><span class="signal" aria-hidden="true"></span>${label}</span>`;
  }
  if (isPendingStatus(s.status)) return `<span class="chip chip--warn lv-chip">${label}</span>`;
  return `<span class="chip lv-chip lv-chip--muted">${label}</span>`;
}

function actionBtn(act, label, iconId, enabled, name) {
  return `<button type="button" class="btn btn--ghost btn--sm lv-act" data-act="${act}"
    aria-label="${label} ${attr(name)}" title="${label}"${enabled ? '' : ' disabled'}>${icon(iconId)}<span class="lv-btn-label">${label}</span></button>`;
}

function rowHtml(s, act) {
  const name = s.displayName || s.name;
  const pid = hasPid(s)
    ? `<button type="button" class="lv-link lv-num" data-act="pid" title="Show process ${s.pid}" aria-label="Show process ${s.pid}">${s.pid}</button>`
    : '<span class="lv-faint" aria-label="No process">-</span>';
  const desc = s.description || '';
  return `<td class="c-name"><div class="lv-namecell"><span class="lv-primary" title="${attr(name)}">${text(name)}</span><span class="lv-secondary" title="${attr(s.name)}">${text(s.name)}</span></div></td>
    <td class="c-status">${statusChip(s, act)}</td>
    <td class="c-start"><span class="lv-start${s.startType === 'disabled' ? ' lv-start--off' : ''}">${START_LABEL[s.startType] || 'Unknown'}</span></td>
    <td class="c-pid cell-num">${pid}</td>
    <td class="c-desc"><span class="lv-desc" title="${attr(desc)}">${text(desc)}</span></td>
    <td class="c-actions"><div class="lv-actions">${actionBtn('start', 'Start', 'i-play', canStart(s), name)}${actionBtn('stop', 'Stop', 'i-stop', canStop(s), name)}${actionBtn('restart', 'Restart', 'i-refresh', canRestart(s), name)}</div></td>
    <td class="c-more"><button type="button" class="icon-btn lv-more" data-act="menu" aria-label="More actions for ${attr(name)}" title="More actions">${icon('i-more')}</button></td>`;
}

function rowFor(s, index) {
  const act = pending.get(s.name) || '';
  const sig = `${s.displayName}\u0001${s.status}\u0001${s.pid}\u0001${s.startType}\u0001${s.description || ''}\u0001${act}`;
  let rec = rowCache.get(s.name);
  if (!rec) {
    const tr = document.createElement('tr');
    tr.className = 'row';
    tr.dataset.name = s.name;
    rec = { tr, sig: '' };
    rowCache.set(s.name, rec);
  }
  if (rec.sig !== sig) {
    rec.tr.innerHTML = rowHtml(s, act);
    rec.tr.classList.toggle('is-pending', Boolean(act));
    rec.tr.setAttribute('aria-busy', act ? 'true' : 'false');
    rec.sig = sig;
  }
  rec.tr.setAttribute('aria-rowindex', String(index + 2));
  return rec.tr;
}

function makeSpacer() {
  const tr = document.createElement('tr');
  tr.className = 'lv-spacer';
  tr.setAttribute('aria-hidden', 'true');
  const td = document.createElement('td');
  td.colSpan = COLS;
  tr.appendChild(td);
  return tr;
}

function visibleRange(n) {
  if (n <= VIRTUAL_THRESHOLD) return [0, n];
  if (!rowH) return [0, Math.min(n, FIRST_CHUNK)];
  // Works whichever ancestor scrolls: compare the tbody to the visible part of the scroller.
  const wrap = els.scroll.getBoundingClientRect();
  const body = els.tbody.getBoundingClientRect();
  const top = Math.max(wrap.top, 0);
  const bottom = Math.min(wrap.bottom, window.innerHeight || document.documentElement.clientHeight);
  const height = Math.max(bottom - top, 0) || 600;
  const offset = Math.max(top - body.top, 0);
  const perScreen = Math.ceil(height / rowH);
  // Clamp so a shrunken list (new filter while scrolled deep) still shows its last screen.
  const lastStart = Math.max(n - perScreen - OVERSCAN, 0);
  const start = Math.max(Math.min(Math.floor(offset / rowH) - OVERSCAN, lastStart), 0);
  const end = Math.min(Math.max(Math.ceil((offset + height) / rowH) + OVERSCAN, start + perScreen + OVERSCAN), n);
  return [start, Math.max(end, start)];
}

function renderWindow() {
  rafId = 0;
  const n = viewRows.length;
  if (!n) return;
  const [start, end] = visibleRange(n);

  const desired = [spacerTop];
  for (let i = start; i < end; i++) desired.push(rowFor(viewRows[i], i));
  desired.push(spacerBottom);

  const virtual = n > VIRTUAL_THRESHOLD;
  const topH = virtual && rowH ? start * rowH : 0;
  const bottomH = virtual && rowH ? (n - end) * rowH : 0;
  spacerTop.firstChild.style.height = `${topH}px`;
  spacerBottom.firstChild.style.height = `${bottomH}px`;
  spacerTop.hidden = topH === 0;
  spacerBottom.hidden = bottomH === 0;

  const tbody = els.tbody;
  let cursor = tbody.firstChild;
  for (const node of desired) {
    if (node === cursor) cursor = cursor.nextSibling;
    else tbody.insertBefore(node, cursor);
  }
  while (cursor) {
    const next = cursor.nextSibling;
    tbody.removeChild(cursor);
    cursor = next;
  }

  if ((!rowH || remeasure) && end > start) {
    const h = desired[1].offsetHeight;
    if (h > 0) {
      remeasure = false;
      if (h !== rowH) {
        rowH = h;
        if (virtual) scheduleWindow();
      }
    }
  }
}

function scheduleWindow() {
  if (rafId || !visible) return;
  rafId = requestAnimationFrame(renderWindow);
}

/** Re-render only the rows already on screen (after pending state changes). */
function refreshRows() {
  if (!visible || !viewRows.length) {
    dirty = true;
    return;
  }
  renderWindow();
}

function showEmpty(title, body, action) {
  els.tbody.textContent = '';
  els.table.classList.add('is-empty');
  els.empty.hidden = false;
  els.empty.innerHTML = `${icon('i-services')}
    <p class="empty-title lv-empty-title">${text(title)}</p>
    ${body ? `<p class="lv-empty-body">${text(body)}</p>` : ''}
    ${action ? `<button type="button" class="btn btn--ghost btn--sm" data-empty-act="${action.id}">${text(action.label)}</button>` : ''}`;
}

function hideEmpty() {
  if (els.empty.hidden) return;
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

  let running = 0;
  let stopped = 0;
  for (const s of services) {
    if (s.status === 'running') running++;
    else if (s.status === 'stopped') stopped++;
  }
  els.statRunning.textContent = loaded ? String(running) : '-';
  els.statStopped.textContent = loaded ? String(stopped) : '-';
  els.statTotal.textContent = loaded ? String(services.length) : '-';

  for (const btn of els.chips.querySelectorAll('[data-filter]')) {
    const id = btn.dataset.filter;
    const isOn = id === filter;
    btn.classList.toggle('chip--on', isOn);
    btn.setAttribute('aria-pressed', isOn ? 'true' : 'false');
    const c = btn.querySelector('.lv-chip-count');
    if (c) c.textContent = loaded ? String(counts[id]) : '';
  }
  els.count.textContent = loaded && !loadError ? `${viewRows.length} of ${services.length}` : '';
  els.table.setAttribute('aria-rowcount', String(viewRows.length + 1));
  updateSortHeaders();

  if (!loaded) {
    showEmpty('Reading services', 'Asking the Service Control Manager for the full list.');
    return;
  }
  if (loadError && !services.length) {
    showEmpty('Could not read services', loadError, { id: 'retry', label: 'Try again' });
    return;
  }
  if (!services.length) {
    showEmpty('No services found', 'Windows returned an empty service list.');
    return;
  }
  if (!viewRows.length) {
    const q = (store.query || '').trim();
    showEmpty(
      q ? `Nothing matches "${q}"` : 'Nothing in this filter',
      q ? 'Search looks at display names, service names, descriptions and PIDs.' : 'Try another filter.',
      { id: 'clear', label: 'Clear filters' },
    );
    return;
  }
  hideEmpty();
  if (rafId) {
    cancelAnimationFrame(rafId);
    rafId = 0;
  }
  renderWindow();
}

function serviceFromTarget(target) {
  const tr = target.closest('tr[data-name]');
  if (!tr || !els.tbody.contains(tr)) return null;
  return byName.get(tr.dataset.name) || null;
}

function startTimer() {
  stopTimer();
  timer = setInterval(() => {
    if (visible && !document.hidden) load({ silent: true });
  }, REFRESH_MS);
}

function stopTimer() {
  if (timer) clearInterval(timer);
  timer = 0;
  if (followUp) clearTimeout(followUp);
  followUp = 0;
}

/* ---------- mount ---------- */

export function mount(rootEl) {
  root = rootEl;
  // The table runs its own scroller inside the view (app.css .view--fill).
  root.classList.add('view--fill');
  root.innerHTML = `
  <div class="lv lv--services">
    <header class="view-head">
      <div class="lv-head-text">
        <p class="micro">04 / Services</p>
        <h1 class="view-title">Every service. <em>One switch.</em></h1>
      </div>
      <div class="lv-stats" aria-live="polite">
        <div class="stat lv-stat"><span class="micro stat-label">Running</span><span class="stat-value lv-stat-value" data-stat="running">-</span></div>
        <div class="stat lv-stat"><span class="micro stat-label">Stopped</span><span class="stat-value lv-stat-value" data-stat="stopped">-</span></div>
        <div class="stat lv-stat"><span class="micro stat-label">Total</span><span class="stat-value lv-stat-value" data-stat="total">-</span></div>
      </div>
    </header>
    <div class="toolbar lv-toolbar">
      <div class="lv-chips" role="group" aria-label="Filter services">
        ${FILTERS.map((f) => `<button type="button" class="chip" data-filter="${f.id}" aria-pressed="false">${f.label}<span class="lv-chip-count"></span></button>`).join('')}
      </div>
      <span class="lv-toolbar-spacer"></span>
      <span class="micro lv-auto"><span class="signal" aria-hidden="true"></span>Auto / 5 s</span>
      <span class="micro lv-count" aria-live="polite"></span>
      <button type="button" class="icon-btn lv-refresh" data-refresh aria-label="Refresh services" title="Refresh">${icon('i-refresh')}</button>
    </div>
    <div class="panel lv-table-panel">
      <div class="lv-scroll">
        <table class="table lv-table lv-table--services">
          <thead><tr>
            <th class="c-name" scope="col" aria-sort="none"><button type="button" class="lv-sort" data-sort="name">Name</button></th>
            <th class="c-status" scope="col" aria-sort="none"><button type="button" class="lv-sort" data-sort="status">Status</button></th>
            <th class="c-start" scope="col" aria-sort="none"><button type="button" class="lv-sort" data-sort="startType">Start type</button></th>
            <th class="c-pid cell-num" scope="col" aria-sort="none"><button type="button" class="lv-sort" data-sort="pid">PID</button></th>
            <th class="c-desc" scope="col" aria-sort="none"><button type="button" class="lv-sort" data-sort="description">Description</button></th>
            <th class="c-actions" scope="col"><span class="lv-sr">Controls</span></th>
            <th class="c-more" scope="col"><span class="lv-sr">More</span></th>
          </tr></thead>
          <tbody></tbody>
        </table>
        <div class="empty lv-empty" hidden></div>
      </div>
    </div>
  </div>`;

  els.statRunning = root.querySelector('[data-stat="running"]');
  els.statStopped = root.querySelector('[data-stat="stopped"]');
  els.statTotal = root.querySelector('[data-stat="total"]');
  els.chips = root.querySelector('.lv-chips');
  els.count = root.querySelector('.lv-count');
  els.refresh = root.querySelector('[data-refresh]');
  els.scroll = root.querySelector('.lv-scroll');
  els.table = root.querySelector('table');
  els.thead = els.table.tHead;
  els.tbody = els.table.tBodies[0];
  els.empty = root.querySelector('.lv-empty');
  spacerTop = makeSpacer();
  spacerBottom = makeSpacer();

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
    if (!btn || btn.disabled) return;
    const svc = serviceFromTarget(btn);
    if (!svc) return;
    const act = btn.dataset.act;
    if (act === 'start' || act === 'stop' || act === 'restart') control(svc, act);
    else if (act === 'pid') goToProcess(svc);
    else if (act === 'menu') {
      const r = btn.getBoundingClientRect();
      showMenu(r.right, r.bottom + 4, menuItems(svc));
    }
  });

  els.tbody.addEventListener('contextmenu', (ev) => {
    const svc = serviceFromTarget(ev.target);
    if (!svc) return;
    ev.preventDefault();
    let { clientX: x, clientY: y } = ev;
    if (!x && !y) {
      const r = ev.target.getBoundingClientRect();
      x = r.left + 12;
      y = r.bottom;
    }
    showMenu(x, y, menuItems(svc));
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

  // Capture phase sees scrolling of whichever ancestor owns the scrollbar.
  document.addEventListener('scroll', () => {
    if (visible && viewRows.length > VIRTUAL_THRESHOLD) scheduleWindow();
  }, { capture: true, passive: true });
  window.addEventListener('resize', () => {
    if (visible && viewRows.length > VIRTUAL_THRESHOLD) scheduleWindow();
  }, { passive: true });

  on('query', () => {
    if (visible) render(); else dirty = true;
  });
  lastCompact = Boolean(store.settings && store.settings.compact);
  on('settings', () => {
    // Compact rows change the row height used by the virtual window.
    const compact = Boolean(store.settings && store.settings.compact);
    if (compact === lastCompact) return;
    lastCompact = compact;
    remeasure = true;
    if (visible) render(); else dirty = true;
  });

  render();
}

export function onShow() {
  visible = true;
  if (!loaded && !loading) load();
  else {
    if (dirty) render();
    load({ silent: true });
  }
  startTimer();
}

export function onHide() {
  visible = false;
  stopTimer();
  if (rafId) {
    cancelAnimationFrame(rafId);
    rafId = 0;
  }
}
