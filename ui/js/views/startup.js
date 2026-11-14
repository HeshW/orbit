// 04 / Startup: every Run key and Startup folder entry, with on/off switches ("Apps"),
// plus read-only autoruns tabs: scheduled tasks, services, logon keys and image hijacks.
import { call, onEvent } from '../api.js';
import { store, on, setQuery } from '../store.js';
import { escapeHtml, fuzzy, time } from '../format.js';
import { toast, showMenu } from '../app.js';
import { checkOnVirusTotal } from './security.js';
import { createSuggestions } from './optimize/suggestions.js';

const FILTERS = [
  { id: 'all', label: 'All', test: () => true },
  { id: 'enabled', label: 'Enabled', test: (e) => e.enabled },
  { id: 'disabled', label: 'Disabled', test: (e) => !e.enabled },
  { id: 'admin', label: 'Needs admin', test: (e) => e.needsAdmin },
];

// Store (packaged) startup tasks are read only: Windows lets only the app, or the user in
// Settings, change one (startup/packaged.rs). packagedState is set only on those rows.
const PACKAGED_STATES = {
  enabled: 'Enabled',
  disabled: 'Disabled',
  disabledByUser: 'Disabled by you',
  disabledByPolicy: 'Disabled by policy',
  enabledByPolicy: 'Enabled by policy',
};
const STARTUP_SETTINGS = 'ms-settings:startupapps';
const LOCKED_HINT = "Windows doesn't let other apps change a Store app's startup. Use Startup settings.";

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
let appsCount = 0; // Apps entries matching the search (tab count)
let suggest = null; // "Turn them off?" card (0.5, 7b)

// Autoruns tabs (read only, list_autoruns).
const TABS = [
  { id: 'apps', label: 'Apps' },
  { id: 'tasks', label: 'Scheduled tasks' },
  { id: 'services', label: 'Services' },
  { id: 'logon', label: 'Logon' },
  { id: 'hijacks', label: 'Image hijacks' },
];
const START_TYPES = {
  auto: 'Automatic', autoDelayed: 'Automatic (delayed)', boot: 'Boot', system: 'System',
  manual: 'Manual', disabled: 'Disabled', unknown: 'Unknown',
};
const AR_SR_ACTIONS = '<span class="lv-sr">Actions</span>';
const AR_COLS = {
  tasks: [['c-name', 'Name'], ['c-cmd', 'Command'], ['c-trig', 'Triggers'], ['c-pub', 'Publisher'], ['c-status', 'Status'], ['c-acts', AR_SR_ACTIONS]],
  services: [['c-name', 'Name'], ['c-cmd', 'Path'], ['c-start', 'Start type'], ['c-pub', 'Publisher'], ['c-acts', AR_SR_ACTIONS]],
  hijacks: [['c-name', 'Image'], ['c-cmd', 'Runs instead'], ['c-pub', 'Publisher'], ['c-acts', AR_SR_ACTIONS]],
};
const VT_TITLE = 'Check on VirusTotal. Orbit sends only the file’s SHA-256 hash, never the file.';
const DEFAULT_SHELL = 'explorer.exe';
const DEFAULT_USERINIT = 'C:\\Windows\\system32\\userinit.exe,';

let tab = 'apps';
let ar = null; // { tasks, services, logon, imageHijacks }
let arLoaded = false;
let arLoading = false;
let arError = null;
let arHeadFor = '';
let arRows = []; // rows as rendered in the autoruns table (data-i indexes into this)
const hijackSigs = new Map(); // debugger exe -> SignatureInfo

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
  suggest?.load();
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

async function openStartupSettings() {
  try {
    await call('open_settings_page', { page: STARTUP_SETTINGS });
  } catch (err) {
    toast(errMsg(err), 'error');
  }
}

async function toggle(entry) {
  if (!entry || busy.has(entry.id)) return;
  if (entry.packagedState) {
    toast(LOCKED_HINT, 'info');
    return;
  }
  const next = !entry.enabled;
  entry.enabled = next;
  busy.add(entry.id);
  render();
  try {
    await call('set_startup_enabled', { id: entry.id, enabled: next });
    toast(next ? `${entry.name} will start with Windows.` : `${entry.name} will no longer start with Windows.`, 'success');
    suggest?.load();
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
  if (entry.packagedState) {
    return [
      { label: 'Open Startup settings', icon: 'i-gear', action: () => openStartupSettings() },
      { label: 'Search online', icon: 'i-globe', action: () => searchOnline(entry) },
    ];
  }
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

function packagedRowHtml(e, i) {
  const label = PACKAGED_STATES[e.packagedState] || 'Disabled';
  return `<tr class="row row--packaged" data-i="${i}">
    <td class="c-toggle"><button type="button" class="switch is-locked" role="switch" data-act="locked"
      aria-checked="${e.enabled}" aria-disabled="true" aria-label="${attr(e.name)} starts with Windows: ${attr(label)}. Read only." title="${attr(LOCKED_HINT)}"><span class="switch-thumb" aria-hidden="true"></span></button></td>
    <td class="c-name"><div class="lv-namecell"><span class="lv-ico lv-ico--blank" aria-hidden="true">${icon('i-rocket')}</span><span class="lv-primary" title="${attr(e.name)}">${text(e.name)}</span></div></td>
    <td class="c-cmd"><button type="button" class="btn btn--ghost btn--sm" data-act="settings" aria-label="Open Startup settings to change ${attr(e.name)}"><span>Open Startup settings</span></button></td>
    <td class="c-loc"><div class="lv-loccell"><span class="chip lv-chip">${text(e.location)}</span></div></td>
    <td class="c-impact"><span class="lv-impact-none">Not measured</span></td>
    <td class="c-status"><span class="lv-status ${e.enabled ? 'lv-status--on' : 'lv-status--off'}">${text(label)}</span></td>
    <td class="c-more"><button type="button" class="icon-btn lv-more" data-act="menu" aria-label="More actions for ${attr(e.name)}" title="More actions">${icon('i-more')}</button></td>
  </tr>`;
}

function rowHtml(e, i) {
  if (e.packagedState) return packagedRowHtml(e, i);
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
  renderApps();
  renderTabs();
  renderAutoruns();
}

function renderApps() {
  const counts = computeRows();
  appsCount = counts.all;

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

/* ---------- autoruns (read only) ---------- */

function query() {
  return (store.query || '').trim().toLowerCase();
}

function leafName(taskPath) {
  const s = String(taskPath || '');
  const i = s.lastIndexOf('\\');
  return i >= 0 && i < s.length - 1 ? s.slice(i + 1) : s;
}

function folderOf(taskPath) {
  const s = String(taskPath || '');
  const i = s.lastIndexOf('\\');
  return i > 0 ? s.slice(0, i) : '';
}

/** The program a command line starts: the quoted part, a path up to ".exe", or the first word. */
function exeFromCommand(cmd) {
  const s = String(cmd || '').trim();
  if (!s) return '';
  if (s.startsWith('"')) return s.slice(1).split('"')[0].trim();
  const first = s.split(/\s+/)[0];
  if (!/[\\/]/.test(first)) return first;
  const m = /^(.*?\.(?:exe|com|bat|cmd))(?=\s|$)/i.exec(s);
  return m ? m[1] : first;
}

function normalizeAutoruns(d) {
  const o = d && typeof d === 'object' ? d : {};
  const list = (x) => (Array.isArray(x) ? x : []);
  const l = o.logon && typeof o.logon === 'object' ? o.logon : {};
  return {
    tasks: list(o.tasks),
    services: list(o.services),
    logon: {
      shell: String(l.shell ?? ''), userinit: String(l.userinit ?? ''),
      shellOk: l.shellOk === true, userinitOk: l.userinitOk === true,
    },
    imageHijacks: list(o.imageHijacks),
  };
}

function setArRefreshBusy(on) {
  if (!els.arRefresh) return;
  els.arRefresh.classList.toggle('is-busy', on);
  els.arRefresh.setAttribute('aria-busy', on ? 'true' : 'false');
}

async function loadAutoruns() {
  if (arLoading) return;
  arLoading = true;
  setArRefreshBusy(true);
  if (visible) render();
  try {
    ar = normalizeAutoruns(await call('list_autoruns'));
    arError = null;
  } catch (err) {
    arError = errMsg(err);
    if (arLoaded) toast(arError, 'error');
  } finally {
    arLoaded = true;
    arLoading = false;
    setArRefreshBusy(false);
    if (visible) render(); else dirty = true;
  }
  loadHijackSigs();
}

/** Signatures of the programs image hijacks start (the list has no publisher of its own). */
async function loadHijackSigs() {
  if (!ar) return;
  const wanted = [];
  for (const h of ar.imageHijacks) {
    const p = exeFromCommand(h.debugger);
    if (p && /[\\/]/.test(p) && !hijackSigs.has(p) && !wanted.includes(p)) wanted.push(p);
  }
  if (!wanted.length) return;
  try {
    const result = await call('get_signatures', { paths: wanted });
    for (const p of wanted) if (result && result[p]) hijackSigs.set(p, result[p]);
    if (visible) render(); else dirty = true;
  } catch {
    // The publisher column stays "Can't check".
  }
}

function arList(kind, q) {
  if (!ar) return [];
  if (kind === 'tasks') {
    return ar.tasks.filter((t) => !q || matches(leafName(t.name), q) || includes(t.name, q) || includes(t.command, q)
      || includes(t.author, q) || includes(t.publisher, q) || (t.triggers || []).some((x) => includes(x, q)));
  }
  if (kind === 'services') {
    return ar.services.filter((s) => !q || matches(s.displayName, q) || includes(s.name, q) || includes(s.path, q)
      || includes(s.publisher, q) || includes(START_TYPES[s.startType], q));
  }
  if (kind === 'hijacks') {
    return ar.imageHijacks.filter((h) => !q || matches(h.image, q) || includes(h.debugger, q));
  }
  return [];
}

function logonWarnings() {
  if (!ar) return 0;
  const l = ar.logon;
  if (!l.shell && !l.userinit) return 0;
  return (l.shellOk ? 0 : 1) + (l.userinitOk ? 0 : 1);
}

function renderTabs() {
  const q = query();
  for (const btn of els.tabs.querySelectorAll('[data-tab]')) {
    const id = btn.dataset.tab;
    const on = id === tab;
    btn.setAttribute('aria-selected', on ? 'true' : 'false');
    btn.tabIndex = on ? 0 : -1;
    const c = btn.querySelector('.ar-tab-count');
    let n = '';
    let warn = false;
    if (id === 'apps') n = loaded ? String(appsCount) : '';
    else if (id === 'logon') {
      const w = ar ? logonWarnings() : 0;
      n = w ? String(w) : '';
      warn = w > 0;
    } else if (ar) n = String(arList(id, q).length);
    c.textContent = n;
    c.classList.toggle('ar-tab-count--warn', warn);
    c.title = warn ? `${n} value${n === '1' ? ' is' : 's are'} not the Windows default` : '';
  }
  els.appsPane.hidden = tab !== 'apps';
  els.arPane.hidden = tab === 'apps';
  if (tab !== 'apps') els.arPane.setAttribute('aria-labelledby', `ar-tab-${tab}`);
  els.stats.hidden = tab !== 'apps';
}

function pubHtml(status, publisher) {
  if (status === 'unsigned') return '<span class="chip lv-chip chip--warn">Unsigned</span>';
  if (status === 'invalid') {
    return `<span class="chip lv-chip chip--danger" title="${attr(publisher ? `Signed as ${publisher}, but the signature does not check out` : 'The signature does not check out')}">Invalid signature</span>`;
  }
  if (status === 'signed') return `<span class="ar-pub" title="${attr(publisher || 'Signed')}">${text(publisher || 'Signed')}</span>`;
  if (status === 'unknown') return '<span class="ar-pub ar-pub--none" title="Orbit could not open this file to check its signature. It may be missing.">Can’t check</span>';
  return '<span class="ar-pub ar-pub--none" title="This entry does not start a program file">No file</span>';
}

function actsHtml(r) {
  const label = attr(r.label);
  const noFile = r.path ? '' : ' disabled';
  return `<div class="ar-acts">
    <button type="button" class="icon-btn lv-more" data-ar="open" aria-label="Open file location of ${label}" title="Open file location"${noFile}>${icon('i-folder')}</button>
    <button type="button" class="icon-btn lv-more" data-ar="search" aria-label="Search online for ${label}" title="Search online">${icon('i-globe')}</button>
    <button type="button" class="icon-btn lv-more" data-ar="vt" aria-label="Check ${label} on VirusTotal" title="${attr(VT_TITLE)}"${noFile}>${icon('i-shield')}</button>
  </div>`;
}

function taskRow(t, i) {
  const leaf = leafName(t.name);
  const folder = folderOf(t.name);
  const triggers = (t.triggers || []).join(', ');
  return `<tr class="row" data-i="${i}">
    <td class="c-name"><div class="lv-namecell"><span class="lv-primary" title="${attr(t.name)}">${text(leaf)}</span>${folder ? `<span class="lv-secondary" title="${attr(folder)}">${text(folder)}</span>` : ''}</div></td>
    <td class="c-cmd"><span class="lv-mono" title="${attr(t.command)}">${text(t.command)}</span></td>
    <td class="c-trig"><span class="ar-trig" title="${attr(triggers)}">${text(triggers)}</span></td>
    <td class="c-pub">${pubHtml(t.signatureStatus, t.publisher)}</td>
    <td class="c-status"><span class="lv-status ${t.enabled ? 'lv-status--on' : 'lv-status--off'}">${t.enabled ? 'Enabled' : 'Disabled'}</span></td>
    <td class="c-acts">${actsHtml(arRows[i])}</td>
  </tr>`;
}

function serviceRow(s, i) {
  const showName = s.name && s.name !== s.displayName;
  return `<tr class="row" data-i="${i}">
    <td class="c-name"><div class="lv-namecell"><span class="lv-primary" title="${attr(s.displayName)}">${text(s.displayName)}</span>${showName ? `<span class="lv-secondary" title="${attr(s.name)}">${text(s.name)}</span>` : ''}</div></td>
    <td class="c-cmd"><span class="lv-mono" title="${attr(s.path)}">${text(s.path)}</span></td>
    <td class="c-start"><span class="lv-start">${text(START_TYPES[s.startType] || s.startType)}</span></td>
    <td class="c-pub">${pubHtml(s.signatureStatus, s.publisher)}</td>
    <td class="c-acts">${actsHtml(arRows[i])}</td>
  </tr>`;
}

function hijackRow(h, i) {
  const sig = hijackSigs.get(arRows[i].path);
  return `<tr class="row" data-i="${i}">
    <td class="c-name"><div class="lv-namecell"><span class="lv-primary" title="${attr(h.image)}">${text(h.image)}</span></div></td>
    <td class="c-cmd"><span class="lv-mono" title="${attr(h.debugger)}">${text(h.debugger)}</span></td>
    <td class="c-pub">${sig ? pubHtml(sig.status, sig.publisher) : pubHtml(arRows[i].path ? 'unknown' : null, null)}</td>
    <td class="c-acts">${actsHtml(arRows[i])}</td>
  </tr>`;
}

function arShowEmpty(title, body, action) {
  els.arTbody.innerHTML = '';
  arRows = [];
  els.arTable.classList.add('is-empty');
  els.arEmpty.hidden = false;
  els.arEmpty.innerHTML = `${icon('i-rocket')}
    <p class="empty-title lv-empty-title">${text(title)}</p>
    ${body ? `<p class="lv-empty-body">${text(body)}</p>` : ''}
    ${action ? `<button type="button" class="btn btn--ghost btn--sm" data-ar-empty="${action.id}">${text(action.label)}</button>` : ''}`;
}

function arHelpText() {
  if (tab === 'tasks') {
    const admin = store.staticInfo && store.staticInfo.isAdmin;
    return 'Scheduled tasks that run when you sign in or when Windows starts. Read only.'
      + (admin ? '' : ' Tasks of other accounts show after Restart as administrator.');
  }
  if (tab === 'services') return 'Services and drivers that start on their own and are not signed by Microsoft. Read only.';
  if (tab === 'logon') return 'The programs Windows runs at every sign-in. Read only.';
  return 'Programs set to open in place of another program (Image File Execution Options). Read only.';
}

const AR_EMPTY = {
  tasks: ['No scheduled tasks start at sign-in', 'No task has a log on or system start trigger.'],
  services: ['No third-party services start on their own', 'Every automatic service and driver is signed by Microsoft.'],
  hijacks: ['No image hijacks', 'No program is set to open in place of another.'],
};

function renderAutoruns() {
  if (tab === 'apps') return;
  const isLogon = tab === 'logon';
  els.arTablePanel.hidden = isLogon;
  els.logon.hidden = !isLogon;
  els.arHelp.textContent = arHelpText();
  if (isLogon) {
    els.arCount.textContent = '';
    renderLogon();
    return;
  }

  if (arHeadFor !== tab) {
    arHeadFor = tab;
    els.arTable.className = `table lv-table ar-table ar-table--${tab}`;
    els.arThead.innerHTML = `<tr>${AR_COLS[tab].map(([cls, label]) => `<th class="${cls}" scope="col">${label}</th>`).join('')}</tr>`;
    els.arScroll.scrollTop = 0;
  }

  const all = tab === 'tasks' ? ar?.tasks : tab === 'services' ? ar?.services : ar?.imageHijacks;
  const list = arList(tab, query());
  els.arCount.textContent = ar && !arError ? `${list.length} of ${all.length}` : '';

  if (!ar) {
    if (arError) arShowEmpty('Could not read the autoruns', arError, { id: 'retry', label: 'Try again' });
    else arShowEmpty('Reading scheduled tasks and services', 'Checking signatures can take a few seconds the first time.');
    return;
  }
  if (!all.length) {
    arShowEmpty(AR_EMPTY[tab][0], AR_EMPTY[tab][1]);
    return;
  }
  if (!list.length) {
    const q = (store.query || '').trim();
    arShowEmpty(`Nothing matches "${q}"`, 'Search looks at names, commands, paths and publishers.', { id: 'clear', label: 'Clear search' });
    return;
  }
  els.arEmpty.hidden = true;
  els.arEmpty.innerHTML = '';
  els.arTable.classList.remove('is-empty');

  // Keep keyboard focus on the same button across the rebuild.
  let focusKey = null;
  let focusAct = null;
  const active = document.activeElement;
  if (active && els.arTbody.contains(active)) {
    const tr = active.closest('tr[data-i]');
    const prev = tr ? arRows[Number(tr.dataset.i)] : null;
    focusKey = prev ? prev.key : null;
    focusAct = active.dataset.ar || null;
  }

  arRows = list.map((item) => {
    if (tab === 'tasks') return { kind: tab, item, key: item.name, path: item.path || '', label: leafName(item.name), search: `${leafName(item.name)} scheduled task` };
    if (tab === 'services') return { kind: tab, item, key: item.name, path: item.path || '', label: item.displayName || item.name, search: `${item.displayName || item.name} service` };
    const exe = exeFromCommand(item.debugger);
    return { kind: tab, item, key: `${item.image}|${item.debugger}`, path: /[\\/]/.test(exe) ? exe : '', label: item.image, search: `${item.image} Image File Execution Options debugger` };
  });
  const rowFn = tab === 'tasks' ? taskRow : tab === 'services' ? serviceRow : hijackRow;
  els.arTbody.innerHTML = arRows.map((r, i) => rowFn(r.item, i)).join('');

  if (focusKey) {
    const idx = arRows.findIndex((r) => r.key === focusKey);
    if (idx >= 0) {
      const target = els.arTbody.children[idx].querySelector(`[data-ar="${focusAct}"]:not(:disabled)`)
        || els.arTbody.children[idx].querySelector('[data-ar]:not(:disabled)');
      if (target) target.focus({ preventScroll: true });
    }
  }
}

function logonFact(label, value, ok, fallback) {
  const shown = value ? text(value) : '<span class="lv-faint">(empty)</span>';
  const chip = ok
    ? '<span class="chip lv-chip chip--ok">Default</span>'
    : '<span class="chip lv-chip chip--warn">Not the default</span>';
  return `<div class="ar-fact">
    <dt>${label}</dt>
    <dd>
      <div class="ar-value-row"><code class="ar-value">${shown}</code>${chip}</div>
      ${ok ? '' : `<p class="ar-default">Windows default: <code>${text(fallback)}</code></p>`}
    </dd>
  </div>`;
}

function verdictHtml(kind, iconId, title, body) {
  return `<div class="ar-verdict${kind ? ` ar-verdict--${kind}` : ''}" role="status">
    ${icon(iconId)}
    <div class="ar-verdict-text">
      <p class="ar-verdict-title">${text(title)}</p>
      ${body ? `<p class="ar-verdict-body">${text(body)}</p>` : ''}
    </div>
  </div>`;
}

function renderLogon() {
  if (!ar) {
    els.logon.innerHTML = arError
      ? `${verdictHtml('muted', 'i-shield', 'Could not read the logon values', arError)}
         <div><button type="button" class="btn btn--ghost btn--sm" data-ar-empty="retry">Try again</button></div>`
      : verdictHtml('muted', 'i-clock', 'Reading the logon values', '');
    return;
  }
  const l = ar.logon;
  if (!l.shell && !l.userinit) {
    els.logon.innerHTML = verdictHtml('muted', 'i-shield', 'Could not read the logon values', 'Try Restart as administrator.');
    return;
  }
  const warn = !l.shellOk || !l.userinitOk;
  const about = 'Windows starts these at sign-in. Anything after explorer.exe here also runs at every sign-in.';
  els.logon.innerHTML = `
    ${warn
      ? verdictHtml('warn', 'i-shield', 'A logon value is not the Windows default', `${about} Check what the extra part starts before you trust it.`)
      : verdictHtml('', 'i-check', 'Both logon values are the Windows defaults', about)}
    <dl class="ar-facts">
      ${logonFact('Shell', l.shell, l.shellOk, DEFAULT_SHELL)}
      ${logonFact('Userinit', l.userinit, l.userinitOk, DEFAULT_USERINIT)}
    </dl>
    <p class="lv-help">Read from HKLM\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Winlogon. Orbit does not change these values.</p>`;
}

async function arAction(act, r) {
  if (!r) return;
  if (act === 'open') {
    if (!r.path) return;
    try {
      await call('open_location', { path: r.path });
    } catch (err) {
      toast(errMsg(err), 'error');
    }
  } else if (act === 'search') {
    try {
      await call('search_online', { query: r.search });
    } catch (err) {
      toast(errMsg(err), 'error');
    }
  } else if (act === 'vt') {
    if (r.path) await checkOnVirusTotal(r.path, r.label);
  }
}

function selectTab(id, focus) {
  if (!TABS.some((t) => t.id === id)) return;
  tab = id;
  render();
  if (focus) {
    const btn = els.tabs.querySelector(`[data-tab="${id}"]`);
    if (btn) btn.focus();
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
    <div class="ar-tabs" role="tablist" aria-label="Startup lists">
      ${TABS.map((t) => `<button type="button" class="ar-tab" role="tab" id="ar-tab-${t.id}" data-tab="${t.id}" aria-selected="false" aria-controls="${t.id === 'apps' ? 'ar-pane-apps' : 'ar-pane-auto'}" tabindex="-1">${t.label}<span class="ar-tab-count"></span></button>`).join('')}
    </div>
    <div class="ar-pane" id="ar-pane-apps" role="tabpanel" aria-labelledby="ar-tab-apps" data-pane="apps">
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
    <div data-suggest></div>
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
    </div>
    <div class="ar-pane" id="ar-pane-auto" role="tabpanel" aria-labelledby="ar-tab-tasks" data-pane="auto" hidden>
      <div class="toolbar lv-toolbar">
        <p class="ar-help" data-ar-help></p>
        <span class="micro lv-count" data-ar-count aria-live="polite"></span>
        <button type="button" class="icon-btn lv-refresh" data-ar-refresh aria-label="Refresh scheduled tasks, services and logon values" title="Refresh">${icon('i-refresh')}</button>
      </div>
      <div class="panel lv-table-panel ar-table-panel" data-ar-panel>
        <div class="lv-scroll" data-ar-scroll>
          <table class="table lv-table ar-table" data-ar-table>
            <thead></thead>
            <tbody></tbody>
          </table>
          <div class="empty lv-empty" data-ar-empty-box hidden></div>
        </div>
      </div>
      <section class="panel ar-logon" data-ar-logon aria-label="Logon values" hidden></section>
    </div>
  </div>`;

  els.stats = root.querySelector('.lv-stats');
  els.tabs = root.querySelector('.ar-tabs');
  els.appsPane = root.querySelector('[data-pane="apps"]');
  els.arPane = root.querySelector('[data-pane="auto"]');
  els.statEnabled = root.querySelector('[data-stat="enabled"]');
  els.statDisabled = root.querySelector('[data-stat="disabled"]');
  els.chips = els.appsPane.querySelector('.lv-chips');
  els.count = els.appsPane.querySelector('.lv-count');
  els.refresh = els.appsPane.querySelector('[data-refresh]');
  els.table = els.appsPane.querySelector('table');
  els.thead = els.table.tHead;
  els.tbody = els.table.tBodies[0];
  els.empty = els.appsPane.querySelector('.lv-empty');
  els.measure = root.querySelector('[data-measure]');
  els.note = root.querySelector('[data-note]');
  els.noteCancel = root.querySelector('[data-note-cancel]');
  els.when = root.querySelector('[data-when]');
  els.arHelp = root.querySelector('[data-ar-help]');
  els.arCount = root.querySelector('[data-ar-count]');
  els.arRefresh = root.querySelector('[data-ar-refresh]');
  els.arTablePanel = root.querySelector('[data-ar-panel]');
  els.arScroll = root.querySelector('[data-ar-scroll]');
  els.arTable = root.querySelector('[data-ar-table]');
  els.arThead = els.arTable.tHead;
  els.arTbody = els.arTable.tBodies[0];
  els.arEmpty = root.querySelector('[data-ar-empty-box]');
  els.logon = root.querySelector('[data-ar-logon]');
  suggest = createSuggestions(root.querySelector('[data-suggest]'), { onChange: () => load(), compact: true });

  els.tabs.addEventListener('click', (ev) => {
    const btn = ev.target.closest('[data-tab]');
    if (btn) selectTab(btn.dataset.tab, false);
  });
  // Arrow keys move between tabs (one tab stop for the whole list).
  els.tabs.addEventListener('keydown', (ev) => {
    const i = TABS.findIndex((t) => t.id === tab);
    let next = -1;
    if (ev.key === 'ArrowRight') next = (i + 1) % TABS.length;
    else if (ev.key === 'ArrowLeft') next = (i - 1 + TABS.length) % TABS.length;
    else if (ev.key === 'Home') next = 0;
    else if (ev.key === 'End') next = TABS.length - 1;
    if (next < 0) return;
    ev.preventDefault();
    selectTab(TABS[next].id, true);
  });

  els.arRefresh.addEventListener('click', () => loadAutoruns());

  els.arTbody.addEventListener('click', (ev) => {
    const btn = ev.target.closest('[data-ar]');
    if (!btn || btn.disabled) return;
    const tr = btn.closest('tr[data-i]');
    if (!tr) return;
    arAction(btn.dataset.ar, arRows[Number(tr.dataset.i)]);
  });

  els.arPane.addEventListener('click', (ev) => {
    const btn = ev.target.closest('[data-ar-empty]');
    if (!btn) return;
    if (btn.dataset.arEmpty === 'retry') loadAutoruns();
    else if (btn.dataset.arEmpty === 'clear') {
      clearSearch();
      render();
    }
  });

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
    } else if (btn.dataset.act === 'locked') {
      toast(LOCKED_HINT, 'info');
    } else if (btn.dataset.act === 'settings') {
      openStartupSettings();
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

  // Another app (or Task Manager, or Settings) changed a startup entry: the backend's
  // watcher sends this once per change. Hidden: the next onShow re-reads anyway.
  onEvent('orbit://startup-changed', () => {
    if (visible && loaded) load();
  });

  render();
}

export function onShow() {
  visible = true;
  if (!arLoaded && !arLoading) loadAutoruns();
  if (!loaded && !loading) load();
  else {
    // Re-read each time the tab is shown (cheap); the old rows stay up until the new ones
    // arrive, so scroll position and focus are kept.
    if (dirty) render();
    if (loaded) load();
  }
}

export function onHide() {
  visible = false;
}
