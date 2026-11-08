// Orbit / 01 Processes
//
// Grouped (Apps / Background / Windows), tree and flat process table with
// search, filter chips, sorting, keyboard selection and the shared process
// actions used by the details drawer and the command palette.
//
// Rendering: rows are virtualised. A small pool of row elements is recycled by
// index (slot = index % pool size), positioned with transforms inside a canvas
// of fixed height. Each slot caches what it last wrote, so a 1 s snapshot only
// touches text, classes and heat values that actually changed.

import { call } from '../api.js';
import { store, on, setSettings, setQuery, selectProcess, pinStartMatches, pinEntry } from '../store.js';
import { bytes, rate, pct, heat, escapeHtml, fuzzy, IO_TITLE } from '../format.js';
import { toast, showMenu, confirmDialog, formDialog, openRunTask } from '../app.js';

const ROW_H = 32;
const ROW_H_COMPACT = 26;
const OVERSCAN = 8;
const MAX_INDENT = 12;
const HIGH_CPU = 10;
const HIGH_MEM = 500 * 1024 * 1024;
const ICON_BATCH = 48;
const ICON_TRIES = 3;
const ICON_RETRY_MS = 4000;
const SORT_STORAGE = 'orbit.sort';
const DASH = '–';
const EMPTY = [];

export const PRIORITIES = [
  { value: 'realtime', label: 'Realtime' },
  { value: 'high', label: 'High' },
  { value: 'aboveNormal', label: 'Above normal' },
  { value: 'normal', label: 'Normal' },
  { value: 'belowNormal', label: 'Below normal' },
  { value: 'idle', label: 'Low' },
];

const CRITICAL = new Set([
  'system', 'system idle process', 'registry', 'secure system', 'memory compression',
  'smss.exe', 'csrss.exe', 'wininit.exe', 'winlogon.exe', 'services.exe', 'lsass.exe',
  'lsaiso.exe', 'svchost.exe', 'dwm.exe', 'fontdrvhost.exe',
]);

// Ending one of these stops Windows itself (CRITICAL_PROCESS_DIED), so 'End all' is not offered.
const NO_END_ALL = new Set(['csrss.exe', 'wininit.exe', 'smss.exe', 'lsass.exe', 'services.exe', 'winlogon.exe']);

const SERVICE_USERS = new Set([
  'system', 'local service', 'network service',
  'nt authority\\system', 'nt authority\\local service', 'nt authority\\network service',
]);

const COLUMNS = [
  { key: 'name', label: 'Name', num: false },
  { key: 'pid', label: 'PID', num: true },
  { key: 'status', label: 'Status', num: false },
  { key: 'user', label: 'User', num: false },
  { key: 'cpu', label: 'CPU', num: true },
  { key: 'memory', label: 'Memory', num: true },
  { key: 'disk', label: 'I/O', num: true, title: IO_TITLE },
  { key: 'gpu', label: 'GPU', num: true },
];
const COL_KEYS = new Set(COLUMNS.map((c) => c.key));
const ASC_FIRST = new Set(['name', 'pid', 'status', 'user']);

const CHIPS = [
  { id: 'cpu', label: 'High CPU', title: 'CPU at 10% or more' },
  { id: 'mem', label: 'High memory', title: 'Memory at 500 MB or more' },
  { id: 'nr', label: 'Not responding', title: 'Apps whose window stopped responding' },
  { id: 'mine', label: 'Mine only', title: 'Processes run by you' },
  { id: 'watch', label: 'Watchlist', title: 'Pinned processes only' },
];

const GROUPINGS = [
  { id: 'grouped', label: 'Grouped', icon: 'i-list' },
  { id: 'tree', label: 'Tree', icon: 'i-tree' },
  { id: 'flat', label: 'Flat', icon: 'i-grid' },
];

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' });

const state = {
  root: null,
  els: null,
  visible: false,
  showKnown: false,
  sort: loadSort(),
  chips: new Set(),
  collapsedSections: new Set(),
  expandedGroups: new Set(),
  collapsedTree: new Set(),
  filterCollapsed: new Set(),
  selKey: null,
  selGroupPid: null,
  model: emptyModel(),
  byPid: new Map(),
  pins: { pids: new Set(), names: new Set() },
  terms: [],
  queryKey: '',
  mineUser: null,
  mineUserLc: null,
  grouping: 'grouped',
  rowH: ROW_H,
  headH: 46,
  viewH: 0,
  scrollTop: 0,
  pool: [],
  raf: 0,
  lastSnap: null,
  hc: {},
  iconMap: new Map(),
  iconReq: new Set(),
  iconTries: new Map(),
  lastPointer: null,
  pruneQueued: false,
  activeDesc: null,
};

function emptyModel() {
  return { items: [], keyIndex: new Map(), filtered: [], total: 0, filterActive: false };
}

/* ------------------------------------------------------------------ */
/* Small helpers                                                       */
/* ------------------------------------------------------------------ */

function lc(s) {
  return typeof s === 'string' ? s.toLowerCase() : '';
}

function errText(err) {
  if (typeof err === 'string' && err) return err;
  if (err && typeof err.message === 'string' && err.message) return err.message;
  return 'Something went wrong. Try again.';
}

function isFuzzy(hay, needle) {
  if (!hay) return false;
  const s = fuzzy(hay, needle);
  return typeof s === 'number' ? s >= 0 : !!s;
}

function clamp01(v) {
  return Number.isFinite(v) ? (v < 0 ? 0 : v > 1 ? 1 : v) : 0;
}

function heatOf(value, max) {
  if (!(value > 0) || !(max > 0)) return 0;
  return Math.round(clamp01(heat(value, max)) * 40) / 40;
}

function loadSort() {
  try {
    const v = JSON.parse(localStorage.getItem(SORT_STORAGE) || 'null');
    if (v && COL_KEYS.has(v.key) && (v.dir === 'asc' || v.dir === 'desc')) return { key: v.key, dir: v.dir };
  } catch (_) { /* storage unavailable */ }
  return { key: 'cpu', dir: 'desc' };
}

function saveSort() {
  try { localStorage.setItem(SORT_STORAGE, JSON.stringify(state.sort)); } catch (_) { /* ignore */ }
}

function currentRows() {
  const snap = store.snapshot;
  return snap && Array.isArray(snap.processes) ? snap.processes : EMPTY;
}

function rowFor(pid) {
  if (pid == null) return null;
  if (state.lastSnap === store.snapshot && state.byPid.size) return state.byPid.get(pid) || null;
  return currentRows().find((r) => r.pid === pid) || null;
}

// pids: Map pid -> pinned start time. A pin only matches the process that started then.
function pinSets() {
  const s = store.settings || {};
  const pids = new Map();
  for (const e of Array.isArray(s.watchlist) ? s.watchlist : EMPTY) pids.set(e.pid, e.start);
  const names = new Set((Array.isArray(s.watchNames) ? s.watchNames : EMPTY).map(lc));
  return { pids, names };
}

function isPinnedRow(r, pins) {
  if (pins.names.has(lc(r.name))) return true;
  const start = pins.pids.get(r.pid);
  return start !== undefined && pinStartMatches(start, r);
}

// Orbit's own process. The backend refuses to end, suspend or re-prioritise it.
function ownPid() {
  const p = store.staticInfo && store.staticInfo.pid;
  return Number.isInteger(p) ? p : null;
}

function isSelf(pid, name) {
  const own = ownPid();
  if (own != null) return pid === own;
  const n = lc(name);
  return n === 'orbit.exe' || n === 'orbit';
}

function refuseSelf() {
  toast('Close Orbit with its window close button.', 'info');
  return false;
}

function validParent(r, byPid) {
  const p = r.parentPid;
  if (p == null || p === r.pid) return null;
  const pr = byPid.get(p);
  if (!pr) return null;
  // A parent that started after its child is a reused PID, not the real parent.
  if (pr.startTime && r.startTime && pr.startTime > r.startTime + 1) return null;
  return pr;
}

function descendantsOf(pid) {
  const all = currentRows();
  const byPid = new Map();
  for (const r of all) byPid.set(r.pid, r);
  const kids = new Map();
  for (const r of all) {
    const p = validParent(r, byPid);
    if (!p) continue;
    let a = kids.get(p.pid);
    if (!a) kids.set(p.pid, (a = []));
    a.push(r);
  }
  const out = [];
  const seen = new Set([pid]);
  const stack = [pid];
  while (stack.length) {
    const p = stack.pop();
    for (const c of kids.get(p) || EMPTY) {
      if (seen.has(c.pid)) continue;
      seen.add(c.pid);
      out.push(c);
      stack.push(c.pid);
    }
  }
  return out;
}

export function statusLabel(r) {
  if (!r) return DASH;
  if (r.suspended) return 'Suspended';
  if (r.notResponding) return 'Not responding';
  const s = lc(r.status);
  if (!s || s === 'running' || s === 'run' || s === 'runnable' || s === 'sleeping' || s === 'sleep' || s === 'idle' || s === 'waiting') return 'Running';
  if (s === 'stopped' || s === 'stop') return 'Stopped';
  if (s === 'zombie' || s === 'dead') return 'Exiting';
  if (s === 'unknown') return 'Unknown';
  return s.charAt(0).toUpperCase() + s.slice(1);
}

export function isPinned(row) {
  return !!row && isPinnedRow(row, pinSets());
}

export async function copyText(text) {
  try {
    if (navigator.clipboard && navigator.clipboard.writeText) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch (_) { /* fall back below */ }
  const prev = document.activeElement;
  try {
    const ta = document.createElement('textarea');
    ta.value = text;
    ta.setAttribute('readonly', '');
    ta.style.position = 'fixed';
    ta.style.top = '-1000px';
    ta.style.opacity = '0';
    document.body.appendChild(ta);
    ta.select();
    const ok = document.execCommand('copy');
    ta.remove();
    return ok;
  } catch (_) {
    return false;
  } finally {
    if (prev && prev instanceof HTMLElement && prev.isConnected) prev.focus({ preventScroll: true });
  }
}

/* ------------------------------------------------------------------ */
/* Shared process actions (also used by details.js and palette.js)     */
/* ------------------------------------------------------------------ */

// startTime: the start time of the row the user acted on. The backend refuses with
// "That process has exited." when the pid now belongs to another process.
function startOf(startTime, row) {
  const t = Number(startTime);
  if (Number.isFinite(t) && t > 0) return t;
  return row && row.startTime ? row.startTime : undefined;
}

export async function endTask(pid, name, startTime) {
  const row = rowFor(pid);
  const label = name || (row && row.name) || `PID ${pid}`;
  const lower = lc(label);
  if (isSelf(pid, label)) return refuseSelf();
  let ok = true;
  if (lower === 'explorer.exe') {
    ok = await confirmDialog({
      title: 'End Explorer?',
      body: 'Explorer will restart the taskbar. The taskbar and desktop disappear for a moment, then Windows brings them back.',
      confirmLabel: 'End task',
      danger: false,
    });
  } else if ((row && row.kind === 'windows') || CRITICAL.has(lower)) {
    ok = await confirmDialog({
      title: `End ${label}?`,
      body: `${label} is part of Windows. Ending it can make Windows unstable, sign you out or restart the PC. Save your work first.`,
      confirmLabel: 'End task',
      danger: true,
    });
  }
  if (!ok) return false;
  try {
    await call('end_task', { pid, startTime: startOf(startTime, row) });
    toast(`Ended ${label} (${pid}).`, 'success');
    return true;
  } catch (err) {
    toast(errText(err), 'error');
    return false;
  }
}

export async function endTree(pid, name, startTime) {
  const row = rowFor(pid);
  const label = name || (row && row.name) || `PID ${pid}`;
  if (isSelf(pid, label)) return refuseSelf();
  const n = descendantsOf(pid).length;
  let body = n === 0
    ? `This ends ${label} (${pid}). It has no child processes.`
    : `This ends ${label} (${pid}) and its ${n} child process${n === 1 ? '' : 'es'}.`;
  if ((row && row.kind === 'windows') || CRITICAL.has(lc(label))) {
    body += ' It is part of Windows, so this can make Windows unstable or restart the PC.';
  }
  const ok = await confirmDialog({ title: 'End process tree?', body, confirmLabel: 'End tree', danger: true });
  if (!ok) return false;
  try {
    const count = await call('end_tree', { pid, startTime: startOf(startTime, row) });
    const c = Number.isFinite(count) ? count : n + 1;
    toast(`Ended ${c} process${c === 1 ? '' : 'es'}.`, 'success');
    return true;
  } catch (err) {
    toast(errText(err), 'error');
    return false;
  }
}

export async function toggleSuspend(row) {
  if (!row) return false;
  const live = rowFor(row.pid) || row;
  if (isSelf(live.pid, live.name)) return refuseSelf();
  const resume = !!live.suspended;
  if (!resume && (live.kind === 'windows' || CRITICAL.has(lc(live.name)))) {
    const ok = await confirmDialog({
      title: `Suspend ${live.name}?`,
      body: `${live.name} is part of Windows. Suspending it can freeze the desktop until you resume it or close Orbit, which resumes every process it suspended.`,
      confirmLabel: 'Suspend',
      danger: true,
    });
    if (!ok) return false;
  }
  try {
    await call(resume ? 'resume_process' : 'suspend_process', { pid: live.pid, startTime: startOf(row.startTime, live) });
    toast(`${resume ? 'Resumed' : 'Suspended'} ${live.name}.`, 'success');
    return true;
  } catch (err) {
    toast(errText(err), 'error');
    return false;
  }
}

export async function setPriority(pid, priority, startTime) {
  const p = PRIORITIES.find((x) => x.value === priority);
  const row = rowFor(pid);
  if (isSelf(pid, (row || {}).name)) return refuseSelf();
  if (!p) {
    toast('Unknown priority.', 'error');
    return false;
  }
  if (priority === 'realtime') {
    const ok = await confirmDialog({
      title: 'Set priority to Realtime?',
      body: 'A realtime process can starve Windows itself, including the mouse and keyboard. Use High unless you are sure.',
      confirmLabel: 'Set realtime',
      danger: true,
    });
    if (!ok) return false;
  }
  try {
    await call('set_priority', { pid, priority, startTime: startOf(startTime, row) });
    toast(`Priority set to ${p.label}.`, 'success');
    return true;
  } catch (err) {
    toast(errText(err), 'error');
    return false;
  }
}

function isExplorer(name) {
  return lc(name) === 'explorer.exe';
}

/** Resolves true once `pid` is in a snapshot (false after `ms`). */
export function whenListed(pid, ms = 4000) {
  return new Promise((resolve) => {
    if (currentRows().some((r) => r.pid === pid)) {
      resolve(true);
      return;
    }
    let timer = 0;
    const off = on('snapshot', () => {
      if (!currentRows().some((r) => r.pid === pid)) return;
      off();
      clearTimeout(timer);
      resolve(true);
    });
    timer = setTimeout(() => {
      off();
      resolve(false);
    }, ms);
  });
}

/**
 * End a process and start it again with the same command line and working folder.
 * Explorer restarts without asking (like Task Manager). Resolves the new pid, or null.
 */
export async function restartProcess(pid, name, startTime) {
  const row = rowFor(pid);
  const label = name || (row && row.name) || `PID ${pid}`;
  if (isSelf(pid, label)) return refuseSelf() || null;
  if (!isExplorer(label)) {
    const sys = (row && row.kind === 'windows') || CRITICAL.has(lc(label));
    const ok = await confirmDialog({
      title: `Restart ${label}?`,
      body: sys
        ? `Orbit ends ${label} and starts it again. It is part of Windows, so this can make Windows unstable. Save your work first.`
        : `Orbit ends ${label} and starts it again with the same command line. Unsaved work in it is lost.`,
      confirmLabel: 'Restart',
      danger: sys,
    });
    if (!ok) return null;
  }
  try {
    const newPid = await call('restart_process', { pid, startTime: startOf(startTime, row) });
    toast(`Restarted ${label}.`, 'success');
    return Number.isInteger(newPid) ? newPid : null;
  } catch (err) {
    toast(errText(err), 'error');
    return null;
  }
}

/** The Set affinity dialog: one checkbox per logical processor, plus All processors. */
export async function setAffinity(row) {
  if (!row) return false;
  const live = rowFor(row.pid) || row;
  if (isSelf(live.pid, live.name)) return refuseSelf();
  let info;
  try {
    info = await call('get_affinity', { pid: live.pid });
  } catch (err) {
    toast(errText(err), 'error');
    return false;
  }
  let system;
  let current;
  try {
    system = BigInt(info.systemMask);
    current = BigInt(info.processMask);
  } catch (_) {
    toast('Could not read the processor list.', 'error');
    return false;
  }
  const cores = [];
  for (let i = 0; i < 64; i++) if ((system >> BigInt(i)) & 1n) cores.push(i);
  let boxes = [];
  let all = null;
  const done = await formDialog({
    label: 'Affinity',
    title: `Set affinity for ${live.name}`,
    body: `Pick which processors ${live.name} (${live.pid}) may run on. The change lasts until it exits.`,
    okLabel: 'OK',
    render(fields, ctl) {
      fields.innerHTML =
        '<label class="check check--strong"><input type="checkbox" data-all><span>All processors</span></label>' +
        `<div class="dialog__grid" role="group" aria-label="Processors">${cores.map((i) =>
          `<label class="check"><input type="checkbox" data-cpu="${i}"><span>CPU ${i}</span></label>`).join('')}</div>`;
      all = fields.querySelector('[data-all]');
      boxes = [...fields.querySelectorAll('[data-cpu]')];
      for (const b of boxes) b.checked = !!((current >> BigInt(b.dataset.cpu)) & 1n);
      const sync = () => {
        const n = boxes.filter((b) => b.checked).length;
        all.checked = n === boxes.length;
        all.indeterminate = n > 0 && n < boxes.length;
        ctl.setOk(n > 0);
        ctl.error('');
      };
      all.addEventListener('change', () => {
        for (const b of boxes) b.checked = all.checked;
        sync();
      });
      for (const b of boxes) b.addEventListener('change', sync);
      sync();
    },
    async onSubmit() {
      let mask = 0n;
      for (const b of boxes) if (b.checked) mask |= 1n << BigInt(b.dataset.cpu);
      if (mask === 0n) return 'Pick at least one processor.';
      await call('set_affinity', { pid: live.pid, mask: mask.toString(), startTime: startOf(row.startTime, live) });
      return true;
    },
  });
  if (done) toast(`Affinity set for ${live.name}.`, 'success');
  return done;
}

export async function copyDetails(row) {
  if (!row) return;
  const lines = [
    `Name: ${row.name}`,
    `PID: ${row.pid}`,
    `Status: ${statusLabel(row)}`,
    `User: ${row.user || DASH}`,
    `CPU: ${pct(row.cpu || 0, 1)}`,
    `Memory: ${bytes(row.memory || 0)}`,
    `Path: ${row.exe || 'Access denied'}`,
    `Command line: ${row.cmd || DASH}`,
  ];
  if (row.windowTitle) lines.splice(1, 0, `Window: ${row.windowTitle}`);
  const ok = await copyText(lines.join('\n'));
  toast(ok ? 'Copied details.' : 'Could not copy to the clipboard.', ok ? 'success' : 'error');
}

export function togglePin(row) {
  if (!row) return;
  const s = store.settings || {};
  // Any entry for this pid goes: a matching one is being unpinned, a stale one is a reused pid.
  const others = (Array.isArray(s.watchlist) ? s.watchlist : EMPTY).filter((e) => e.pid !== row.pid);
  const names = (Array.isArray(s.watchNames) ? s.watchNames : EMPTY).slice();
  const lower = lc(row.name);
  if (isPinned(row)) {
    setSettings({ watchlist: others, watchNames: names.filter((n) => lc(n) !== lower) });
    toast(`Unpinned ${row.name}.`, 'info');
  } else {
    setSettings({ watchlist: others.concat([pinEntry(row)]) });
    toast(`Pinned ${row.name} to the watchlist.`, 'success');
  }
}

function pinGroup(g) {
  const s = store.settings || {};
  const names = (Array.isArray(s.watchNames) ? s.watchNames : EMPTY).slice();
  const lower = g.name;
  if (names.some((n) => lc(n) === lower)) {
    setSettings({ watchNames: names.filter((n) => lc(n) !== lower) });
    toast(`Unpinned ${g.primary.name}.`, 'info');
  } else {
    names.push(lower);
    setSettings({ watchNames: names });
    toast(`Pinned every ${g.primary.name} to the watchlist.`, 'success');
  }
}

async function openLocation(row) {
  if (!row || !row.exe) {
    toast('The file location is hidden. Try Restart as administrator.', 'error');
    return;
  }
  try {
    await call('open_location', { path: row.exe });
  } catch (err) {
    toast(errText(err), 'error');
  }
}

async function searchOnline(name) {
  try {
    await call('search_online', { query: name });
  } catch (err) {
    toast(errText(err), 'error');
  }
}

function menuPoint() {
  if (state.lastPointer) return state.lastPointer;
  return { x: Math.round(window.innerWidth / 2), y: Math.round(window.innerHeight / 3) };
}

async function openPriorityMenu(row) {
  const pt = menuPoint();
  let current = null;
  try { current = await call('get_priority', { pid: row.pid }); } catch (_) { current = null; }
  // Let the first menu finish closing before the second one opens.
  await new Promise((r) => setTimeout(r, 0));
  showMenu(pt.x, pt.y, PRIORITIES.map((p) => ({
    label: p.label,
    icon: p.value === current ? 'i-check' : undefined,
    action: () => setPriority(row.pid, p.value, row.startTime),
  })));
}

export function processMenuItems(row) {
  if (!row) return [];
  const pinned = isPinned(row);
  const live = rowFor(row.pid) || row;
  const self = isSelf(live.pid, live.name);
  const restart = { label: 'Restart', icon: 'i-refresh', disabled: self, action: () => restartProcess(live.pid, live.name, live.startTime) };
  // Explorer: Restart comes first, as in Task Manager.
  const explorer = isExplorer(live.name);
  return [
    explorer ? restart : null,
    { label: 'End task', icon: 'i-stop', danger: true, disabled: self, action: () => endTask(live.pid, live.name, live.startTime) },
    { label: 'End process tree', icon: 'i-tree', danger: true, disabled: self, action: () => endTree(live.pid, live.name, live.startTime) },
    explorer ? null : restart,
    {
      label: live.suspended ? 'Resume' : 'Suspend',
      icon: live.suspended ? 'i-play' : 'i-pause',
      disabled: self,
      action: () => toggleSuspend(live),
    },
    { label: 'Set priority', icon: 'i-bolt', disabled: self, action: () => openPriorityMenu(live) },
    { label: 'Set affinity', icon: 'i-cpu', disabled: self, action: () => setAffinity(live) },
    { separator: true },
    { label: 'Open file location', icon: 'i-folder', disabled: !live.exe, action: () => openLocation(live) },
    { label: 'Search online', icon: 'i-globe', action: () => searchOnline(live.name) },
    { label: 'Copy details', icon: 'i-copy', action: () => copyDetails(live) },
    { label: pinned ? 'Unpin from watchlist' : 'Pin to watchlist', icon: 'i-pin', action: () => togglePin(live) },
    { separator: true },
    { label: 'Details', icon: 'i-arrow-right', action: () => selectProcess(live.pid) },
  ];
}

function groupMenuItems(item) {
  const g = item.g;
  const p = g.primary;
  const pinned = (pinSets().names).has(g.name);
  const noEnd = NO_END_ALL.has(g.name);
  return [
    isExplorer(p.name)
      ? { label: 'Restart', icon: 'i-refresh', disabled: isSelf(p.pid, p.name), action: () => restartProcess(p.pid, p.name, p.startTime) }
      : null,
    {
      label: `End all (${g.members.length})`,
      icon: 'i-stop',
      danger: true,
      disabled: noEnd,
      hint: noEnd ? 'Windows needs it' : undefined,
      action: () => endGroup(g),
    },
    { label: 'End process tree', icon: 'i-tree', danger: true, action: () => endTree(p.pid, p.name, p.startTime) },
    { separator: true },
    { label: item.open ? 'Collapse' : 'Expand', icon: 'i-chevron', action: () => toggleItem(item) },
    { label: 'Open file location', icon: 'i-folder', disabled: !p.exe, action: () => openLocation(p) },
    { label: 'Search online', icon: 'i-globe', action: () => searchOnline(p.name) },
    { label: 'Copy details', icon: 'i-copy', action: () => copyDetails(p) },
    { label: pinned ? 'Unpin from watchlist' : 'Pin to watchlist', icon: 'i-pin', action: () => pinGroup(g) },
    { separator: true },
    { label: 'Details', icon: 'i-arrow-right', action: () => selectProcess(p.pid) },
  ];
}

function sectionMenuItems(item) {
  return [{ label: item.open ? 'Collapse section' : 'Expand section', icon: 'i-chevron', action: () => toggleItem(item) }];
}

async function endGroup(g) {
  const p = g.primary;
  const n = g.members.length;
  if (NO_END_ALL.has(g.name)) {
    toast(`${p.name} can't be ended. Windows stops if it does.`, 'info');
    return false;
  }
  const explorer = g.name === 'explorer.exe';
  const sys = g.members.some((r) => r.kind === 'windows' || CRITICAL.has(lc(r.name)));
  const ok = await confirmDialog({
    title: `End all ${p.name}?`,
    body: explorer
      ? `This ends ${n} Explorer processes. Explorer will restart the taskbar.`
      : sys
        ? `This ends ${n} ${p.name} processes. ${p.name} is part of Windows. Ending it can make Windows unstable, sign you out or restart the PC. Save your work first.`
        : `This ends ${n} ${p.name} processes. Unsaved work in them is lost.`,
    confirmLabel: 'End all',
    danger: !explorer,
  });
  if (!ok) return false;
  // Helpers first, the windowed process last, so nothing respawns in between.
  const order = g.members.filter((r) => r !== p).concat([p]);
  let ended = 0;
  let lastErr = null;
  for (const r of order) {
    try {
      await call('end_task', { pid: r.pid, startTime: r.startTime });
      ended++;
    } catch (err) {
      lastErr = err;
    }
  }
  if (ended === n) toast(`Ended ${n} ${p.name} processes.`, 'success');
  else if (ended > 0) toast(`Ended ${ended} of ${n}. ${errText(lastErr)}`, 'error');
  else toast(errText(lastErr), 'error');
  return ended > 0;
}

/* ------------------------------------------------------------------ */
/* CSV export                                                          */
/* ------------------------------------------------------------------ */

function csvText(v) {
  let s = v == null ? '' : String(v);
  if (/^[=+\-@\t\r]/.test(s)) s = `'${s}`;
  if (/[",\r\n]/.test(s) || s !== s.trim()) return `"${s.replace(/"/g, '""')}"`;
  return s;
}

function csvNum(v, digits) {
  return Number.isFinite(v) ? v.toFixed(digits) : '';
}

function two(n) {
  return n < 10 ? `0${n}` : String(n);
}

export async function exportCsv() {
  if (!store.snapshot) {
    toast('Nothing to export yet. Waiting for the first sample.', 'info');
    return;
  }
  if (state.lastSnap !== store.snapshot) buildModel();
  const rows = state.model.filtered.slice().sort(cmpRows);
  const lines = ['Name,PID,Status,User,CPU %,Memory MB,I/O read KB/s,I/O write KB/s,GPU %,Path,Command line'];
  for (const r of rows) {
    lines.push([
      csvText(r.name),
      r.pid,
      csvText(statusLabel(r)),
      csvText(r.user || ''),
      csvNum(r.cpu, 1),
      csvNum((r.memory || 0) / 1048576, 1),
      csvNum((r.diskReadBps || 0) / 1024, 1),
      csvNum((r.diskWriteBps || 0) / 1024, 1),
      r.gpu == null ? '' : csvNum(r.gpu, 1),
      csvText(r.exe || ''),
      csvText(r.cmd || ''),
    ].join(','));
  }
  const now = new Date();
  const fileName = `orbit-processes-${now.getFullYear()}-${two(now.getMonth() + 1)}-${two(now.getDate())}-${two(now.getHours())}${two(now.getMinutes())}.csv`;
  // No BOM here: export_report adds the one UTF-8 BOM for .csv files.
  const contents = `${lines.join('\r\n')}\r\n`;
  try {
    const path = await call('export_report', { fileName, contents });
    toast(`Saved to ${path}`, 'success');
  } catch (err) {
    toast(errText(err), 'error');
  }
}

/* ------------------------------------------------------------------ */
/* Model: filter, sort, group                                          */
/* ------------------------------------------------------------------ */

function parseQuery(q) {
  return String(q || '').trim().toLowerCase().split(/\s+/).filter(Boolean);
}

function termMatches(r, t) {
  if (isFuzzy(r.name, t)) return true;
  if (/^\d+$/.test(t) && String(r.pid).startsWith(t)) return true;
  if (r.windowTitle && r.windowTitle.toLowerCase().includes(t)) return true;
  if (r.exe && r.exe.toLowerCase().includes(t)) return true;
  if (r.cmd && r.cmd.toLowerCase().includes(t)) return true;
  if (r.user && r.user.toLowerCase().includes(t)) return true;
  return false;
}

function matchesAll(r, terms) {
  for (let i = 0; i < terms.length; i++) if (!termMatches(r, terms[i])) return false;
  return true;
}

function passesChips(r, pins) {
  for (const c of state.chips) {
    if (c === 'cpu' && !(r.cpu >= HIGH_CPU)) return false;
    if (c === 'mem' && !(r.memory >= HIGH_MEM)) return false;
    if (c === 'nr' && !r.notResponding) return false;
    if (c === 'mine' && !(state.mineUserLc && r.user && r.user.toLowerCase() === state.mineUserLc)) return false;
    if (c === 'watch' && !isPinnedRow(r, pins)) return false;
  }
  return true;
}

function computeMineUser(rows) {
  for (const r of rows) {
    if (r.user && /^orbit(\.exe)?$/i.test(r.name || '')) return r.user;
  }
  const counts = new Map();
  const original = new Map();
  let best = null;
  let bestN = 0;
  for (const r of rows) {
    if (!r.user) continue;
    const u = r.user.toLowerCase();
    const bare = u.includes('\\') ? u.slice(u.lastIndexOf('\\') + 1) : u;
    if (SERVICE_USERS.has(u) || SERVICE_USERS.has(bare) || bare.startsWith('dwm-') || bare.startsWith('umfd-')) continue;
    const n = (counts.get(u) || 0) + 1;
    counts.set(u, n);
    if (!original.has(u)) original.set(u, r.user);
    if (n > bestN) { best = u; bestN = n; }
  }
  return best ? original.get(best) : null;
}

function sortValue(r) {
  return (r.diskReadBps || 0) + (r.diskWriteBps || 0);
}

function cmpRows(a, b) {
  let r = 0;
  switch (state.sort.key) {
    case 'name': r = collator.compare(a.name || '', b.name || ''); break;
    case 'pid': r = a.pid - b.pid; break;
    case 'status': r = collator.compare(statusLabel(a), statusLabel(b)); break;
    case 'user': r = collator.compare(a.user || '', b.user || ''); break;
    case 'cpu': r = (a.cpu || 0) - (b.cpu || 0); break;
    case 'memory': r = (a.memory || 0) - (b.memory || 0); break;
    case 'disk': r = sortValue(a) - sortValue(b); break;
    case 'gpu': r = (a.gpu == null ? -1 : a.gpu) - (b.gpu == null ? -1 : b.gpu); break;
    default: r = 0;
  }
  if (r === 0) r = (state.sort.key === 'name' ? 0 : collator.compare(a.name || '', b.name || '')) || a.pid - b.pid;
  return state.sort.dir === 'asc' ? r : -r;
}

function procItem(r, depth) {
  return { t: 'proc', key: `p:${r.pid}`, r, depth, kids: 0, open: false, dim: false };
}

function makeGroup(name, members) {
  let primary = null;
  for (const m of members) if (m.kind === 'app' && m.windowTitle && (!primary || m.pid < primary.pid)) primary = m;
  if (!primary) for (const m of members) if (m.kind === 'app' && (!primary || m.pid < primary.pid)) primary = m;
  if (!primary) for (const m of members) if (!primary || m.pid < primary.pid) primary = m;
  let cpu = 0, memory = 0, rd = 0, wr = 0, gpu = null, nr = false, allSus = true;
  let user = primary.user || null;
  for (const m of members) {
    cpu += m.cpu || 0;
    memory += m.memory || 0;
    rd += m.diskReadBps || 0;
    wr += m.diskWriteBps || 0;
    if (m.gpu != null) gpu = (gpu || 0) + m.gpu;
    if (m.notResponding) nr = true;
    if (!m.suspended) allSus = false;
    if (user && m.user !== user) user = null;
  }
  const v = {
    pid: primary.pid,
    parentPid: primary.parentPid,
    name: primary.name,
    exe: primary.exe,
    cmd: primary.cmd,
    user,
    cpu,
    memory,
    diskReadBps: rd,
    diskWriteBps: wr,
    gpu: gpu == null ? null : Math.min(100, gpu),
    status: primary.status,
    startTime: primary.startTime,
    kind: 'app',
    windowTitle: primary.windowTitle,
    notResponding: nr,
    suspended: allSus,
  };
  return { name, members, primary, v };
}

function pushSection(items, id, label, rows, byName, filterActive) {
  if (!rows.length) return;
  const open = !state.collapsedSections.has(id);
  let cpu = 0;
  let memory = 0;
  for (const r of rows) { cpu += r.cpu || 0; memory += r.memory || 0; }
  if (!byName) {
    items.push({ t: 'sec', key: `s:${id}`, id, label, n: rows.length, open, cpu, memory, depth: -1 });
    if (!open) return;
    rows.sort(cmpRows);
    for (const r of rows) items.push(procItem(r, 0));
    return;
  }
  const map = new Map();
  for (const r of rows) {
    const k = lc(r.name);
    let g = map.get(k);
    if (!g) map.set(k, (g = []));
    g.push(r);
  }
  // Like Task Manager, the header counts the entries shown (one per app name).
  items.push({ t: 'sec', key: `s:${id}`, id, label, n: map.size, open, cpu, memory, depth: -1 });
  if (!open) return;
  const entries = [];
  for (const [k, members] of map) {
    entries.push(members.length === 1 ? { single: members[0], v: members[0] } : makeGroup(k, members));
  }
  entries.sort((a, b) => cmpRows(a.v, b.v));
  for (const e of entries) {
    if (e.single) {
      items.push(procItem(e.single, 0));
      continue;
    }
    const key = `g:${e.name}`;
    const gopen = filterActive ? !state.filterCollapsed.has(key) : state.expandedGroups.has(e.name);
    items.push({ t: 'grp', key, g: e, open: gopen, depth: 0 });
    if (gopen) {
      e.members.sort(cmpRows);
      for (const r of e.members) items.push(procItem(r, 1));
    }
  }
}

function buildGrouped(all, filtered, items, filterActive, pins) {
  const appNames = new Set();
  for (const r of all) if (r.kind === 'app') appNames.add(lc(r.name));
  const watch = [];
  const apps = [];
  const background = [];
  const windows = [];
  for (const r of filtered) {
    if (isPinnedRow(r, pins)) watch.push(r);
    else if (r.pid > 4 && appNames.has(lc(r.name))) apps.push(r);
    else if (r.kind === 'windows') windows.push(r);
    else background.push(r);
  }
  pushSection(items, 'watch', 'Watchlist', watch, false, filterActive);
  pushSection(items, 'app', 'Apps', apps, true, filterActive);
  pushSection(items, 'background', 'Background processes', background, false, filterActive);
  pushSection(items, 'windows', 'Windows processes', windows, false, filterActive);
}

function buildTree(all, filtered, items, filterActive, pins, byPid) {
  const parentOf = new Map();
  for (const r of all) {
    const p = validParent(r, byPid);
    if (p) parentOf.set(r.pid, p.pid);
  }
  // Break any parent cycles (PID reuse) so every node reaches a root.
  for (const r of all) {
    let pid = r.pid;
    let hops = 0;
    while (parentOf.has(pid)) {
      pid = parentOf.get(pid);
      if (++hops > 64 || pid === r.pid) {
        parentOf.delete(r.pid);
        break;
      }
    }
  }
  const kids = new Map();
  for (const r of all) {
    const p = parentOf.get(r.pid);
    if (p == null) continue;
    let a = kids.get(p);
    if (!a) kids.set(p, (a = []));
    a.push(r);
  }
  let include = null;
  let matched = null;
  if (filterActive) {
    matched = new Set();
    include = new Set();
    for (const r of filtered) {
      matched.add(r.pid);
      let pid = r.pid;
      let hops = 0;
      while (pid != null && !include.has(pid) && hops++ < 128) {
        include.add(pid);
        pid = parentOf.get(pid);
      }
    }
  }
  const boost = new Set();
  for (const r of all) {
    if (!isPinnedRow(r, pins)) continue;
    let pid = r.pid;
    let hops = 0;
    while (pid != null && !boost.has(pid) && hops++ < 128) {
      boost.add(pid);
      pid = parentOf.get(pid);
    }
  }
  const cmpTree = (a, b) => ((boost.has(b.pid) ? 1 : 0) - (boost.has(a.pid) ? 1 : 0)) || cmpRows(a, b);
  const roots = [];
  for (const r of all) if (!parentOf.has(r.pid) && (!include || include.has(r.pid))) roots.push(r);
  roots.sort(cmpTree);
  const visited = new Set();
  const walk = (r, depth) => {
    if (visited.has(r.pid)) return;
    visited.add(r.pid);
    let ch = kids.get(r.pid) || EMPTY;
    if (include && ch.length) ch = ch.filter((c) => include.has(c.pid));
    const key = `p:${r.pid}`;
    const open = ch.length > 0 && (filterActive ? !state.filterCollapsed.has(key) : !state.collapsedTree.has(r.pid));
    items.push({ t: 'proc', key, r, depth, kids: ch.length, open, dim: !!matched && !matched.has(r.pid) });
    if (open) {
      const sorted = ch.slice().sort(cmpTree);
      for (const c of sorted) walk(c, depth + 1);
    }
  };
  for (const r of roots) walk(r, 0);
}

function buildFlat(filtered, items, pins) {
  const rows = filtered.slice();
  rows.sort((a, b) => ((isPinnedRow(b, pins) ? 1 : 0) - (isPinnedRow(a, pins) ? 1 : 0)) || cmpRows(a, b));
  for (const r of rows) items.push(procItem(r, 0));
}

function buildModel() {
  const snap = store.snapshot;
  const all = currentRows();
  const byPid = new Map();
  for (const r of all) byPid.set(r.pid, r);
  state.byPid = byPid;
  state.lastSnap = snap;
  state.pins = pinSets();
  state.mineUser = computeMineUser(all);
  state.mineUserLc = state.mineUser ? state.mineUser.toLowerCase() : null;
  state.terms = parseQuery(store.query);
  state.queryKey = state.terms.join(' ');
  const g = (store.settings && store.settings.grouping) || 'grouped';
  state.grouping = g === 'tree' || g === 'flat' ? g : 'grouped';

  const filterActive = state.terms.length > 0 || state.chips.size > 0;
  const filtered = [];
  for (const r of all) if (passesChips(r, state.pins) && matchesAll(r, state.terms)) filtered.push(r);

  const items = [];
  if (state.grouping === 'tree') buildTree(all, filtered, items, filterActive, state.pins, byPid);
  else if (state.grouping === 'flat') buildFlat(filtered, items, state.pins);
  else buildGrouped(all, filtered, items, filterActive, state.pins);

  const keyIndex = new Map();
  for (let i = 0; i < items.length; i++) keyIndex.set(items[i].key, i);
  state.model = { items, keyIndex, filtered, total: all.length, filterActive };

  validateSelection();
  queuePrune(all, byPid);
}

function queuePrune(all, byPid) {
  if (state.pruneQueued || !all.length) return;
  const wl = store.settings && Array.isArray(store.settings.watchlist) ? store.settings.watchlist : EMPTY;
  // Keep a pid pin only while that exact process (same start time) runs: a reused pid drops out.
  const alive = (e, map) => pinStartMatches(e.start, map.get(e.pid));
  if (wl.every((e) => alive(e, byPid))) return;
  state.pruneQueued = true;
  setTimeout(() => {
    state.pruneQueued = false;
    const list = store.settings && Array.isArray(store.settings.watchlist) ? store.settings.watchlist : EMPTY;
    const kept = list.filter((e) => alive(e, state.byPid));
    if (kept.length !== list.length && state.byPid.size) setSettings({ watchlist: kept });
  }, 0);
}

function validateSelection() {
  const k = state.selKey;
  if (!k) return;
  if (k.startsWith('p:')) {
    if (!state.byPid.has(Number(k.slice(2)))) state.selKey = null;
    return;
  }
  const i = state.model.keyIndex.get(k);
  if (i != null) {
    const item = state.model.items[i];
    if (item.t === 'grp') state.selGroupPid = item.g.primary.pid;
    return;
  }
  // A group that no longer shows (grouping changed, or one member left):
  // keep the selection on its main process when that is still running.
  const pid = k.startsWith('g:') ? state.selGroupPid : null;
  state.selKey = pid != null && state.byPid.has(pid) ? `p:${pid}` : null;
}

/* ------------------------------------------------------------------ */
/* Selection                                                           */
/* ------------------------------------------------------------------ */

function selectedItem() {
  const i = state.model.keyIndex.get(state.selKey);
  return i == null ? null : state.model.items[i];
}

// Index of the row that shows the selection: the row itself, or the collapsed
// app group that contains the selected process. -1 when nothing shows it.
function selIndex() {
  const { keyIndex } = state.model;
  const i = keyIndex.get(state.selKey);
  if (i != null) return i;
  if (state.selKey && state.selKey.startsWith('p:')) {
    const r = state.byPid.get(Number(state.selKey.slice(2)));
    const g = r ? keyIndex.get(`g:${lc(r.name)}`) : undefined;
    if (g != null) return g;
  }
  return -1;
}

function selectedRow() {
  const item = selectedItem();
  if (item) {
    if (item.t === 'proc') return item.r;
    if (item.t === 'grp') return item.g.primary;
    return null;
  }
  if (state.selKey && state.selKey.startsWith('p:')) return state.byPid.get(Number(state.selKey.slice(2))) || null;
  return null;
}

function setSelection(key, scroll) {
  state.selKey = key;
  const item = selectedItem();
  if (item && item.t === 'grp') state.selGroupPid = item.g.primary.pid;
  updateEndButton();
  if (scroll) {
    const i = state.model.keyIndex.get(key);
    if (i != null) ensureVisible(i);
  }
  scheduleRender();
}

function ensureVisible(i) {
  const el = state.els && state.els.scroll;
  if (!el) return;
  const rowH = state.rowH;
  const avail = Math.max(rowH, state.viewH - state.headH);
  const y = i * rowH;
  let top = state.scrollTop;
  if (y < top) top = y;
  else if (y + rowH > top + avail) top = y + rowH - avail;
  if (top !== state.scrollTop) {
    el.scrollTop = top;
    state.scrollTop = el.scrollTop;
  }
}

function moveTo(i) {
  const items = state.model.items;
  if (!items.length) return;
  const idx = Math.max(0, Math.min(items.length - 1, i));
  setSelection(items[idx].key, true);
}

function toggleItem(item) {
  if (!item) return;
  const filterActive = state.model.filterActive;
  if (item.t === 'sec') {
    if (state.collapsedSections.has(item.id)) state.collapsedSections.delete(item.id);
    else state.collapsedSections.add(item.id);
  } else if (item.t === 'grp' || (item.t === 'proc' && item.kids > 0)) {
    if (filterActive) {
      if (state.filterCollapsed.has(item.key)) state.filterCollapsed.delete(item.key);
      else state.filterCollapsed.add(item.key);
    } else if (item.t === 'grp') {
      if (state.expandedGroups.has(item.g.name)) state.expandedGroups.delete(item.g.name);
      else state.expandedGroups.add(item.g.name);
    } else if (state.collapsedTree.has(item.r.pid)) {
      state.collapsedTree.delete(item.r.pid);
    } else {
      state.collapsedTree.add(item.r.pid);
    }
  } else {
    return;
  }
  refresh();
}

function openDetailsFor(item) {
  if (!item) return;
  if (item.t === 'proc') selectProcess(item.r.pid);
  else if (item.t === 'grp') selectProcess(item.g.primary.pid);
  else toggleItem(item);
}

function endSelected() {
  const item = selectedItem();
  if (item && item.t === 'grp') {
    endGroup(item.g); // refuses the NO_END_ALL groups itself
    return;
  }
  const r = selectedRow();
  if (r) endTask(r.pid, r.name, r.startTime);
}

function menuItemsFor(item) {
  if (!item) return [];
  if (item.t === 'sec') return sectionMenuItems(item);
  if (item.t === 'grp') return groupMenuItems(item);
  return processMenuItems(item.r);
}

/* ------------------------------------------------------------------ */
/* View DOM                                                            */
/* ------------------------------------------------------------------ */

const ROW_TPL =
  '<div class="proc-cell proc-cell--name" role="gridcell" data-c="name">' +
    '<span class="proc-twisty" aria-hidden="true"><svg class="icon"><use href="#i-chevron"/></svg></span>' +
    '<span class="proc-ico" aria-hidden="true"></span>' +
    '<span class="proc-name"></span><span class="proc-n"></span>' +
    '<span class="proc-meta"></span><span class="proc-tags"></span>' +
  '</div>' +
  '<div class="proc-cell cell-num" role="gridcell" data-c="pid"></div>' +
  '<div class="proc-cell" role="gridcell" data-c="status"></div>' +
  '<div class="proc-cell" role="gridcell" data-c="user"></div>' +
  '<div class="proc-cell cell-num heat" role="gridcell" data-c="cpu"></div>' +
  '<div class="proc-cell cell-num heat" role="gridcell" data-c="memory"></div>' +
  '<div class="proc-cell cell-num heat" role="gridcell" data-c="disk"></div>' +
  '<div class="proc-cell cell-num heat" role="gridcell" data-c="gpu"></div>';

const SEC_TPL =
  '<div class="proc-cell proc-cell--section" role="gridcell" aria-colspan="8">' +
    '<span class="proc-twisty" aria-hidden="true"><svg class="icon"><use href="#i-chevron"/></svg></span>' +
    '<span class="proc-sec-label"></span><span class="proc-sec-count"></span>' +
    '<span class="proc-sec-sum"></span>' +
  '</div>';

function viewTemplate() {
  const ths = COLUMNS.map((c) =>
    `<div class="proc-th${c.num ? ' proc-th--num' : ''}" role="columnheader" data-c="${c.key}" aria-sort="none">` +
      `<button type="button" class="proc-sort" data-sort="${c.key}" title="${c.title ? `${c.title}. Sort by ${c.label}` : `Sort by ${c.label.toLowerCase()}`}">` +
        `<span class="proc-th-total" data-total="${c.key}"></span>` +
        `<span class="proc-th-label">${c.label}<svg class="icon proc-sort-ico" aria-hidden="true"><use href="#i-arrow-down"/></svg></span>` +
      '</button>' +
    '</div>').join('');
  const segs = GROUPINGS.map((g) =>
    `<button type="button" class="proc-seg-btn" data-group="${g.id}" aria-pressed="false">` +
      `<svg class="icon" aria-hidden="true"><use href="#${g.icon}"/></svg><span>${g.label}</span>` +
    '</button>').join('');
  const chips = CHIPS.map((c) =>
    `<button type="button" class="chip proc-chip" data-chip="${c.id}" aria-pressed="false" title="${c.title}">${c.label}</button>`).join('');
  const stat = (id, label) =>
    `<div class="stat proc-stat"><span class="stat-label micro">${label}</span><span class="stat-value proc-stat-v" data-stat="${id}">${DASH}</span></div>`;
  return (
    '<div class="proc-view">' +
      '<header class="view-head proc-headbar">' +
        '<div class="proc-headtext">' +
          '<p class="micro">01 / Processes</p>' +
          '<h1 class="view-title">Every process. <em>In orbit.</em></h1>' +
        '</div>' +
        `<div class="proc-stats">${stat('count', 'Processes')}${stat('cpu', 'CPU')}${stat('mem', 'Memory')}</div>` +
      '</header>' +
      '<div class="toolbar proc-toolbar" aria-label="Process tools">' +
        `<div class="proc-seg" role="group" aria-label="Grouping">${segs}</div>` +
        `<div class="proc-chips" role="group" aria-label="Filters">${chips}</div>` +
        '<span class="proc-count micro" data-count></span>' +
        '<span class="proc-spacer"></span>' +
        '<button type="button" class="btn btn--ghost btn--sm" data-act="run" title="Run new task (Ctrl+N)"><span>Run task</span><svg class="icon" aria-hidden="true"><use href="#i-play"/></svg></button>' +
        '<button type="button" class="btn btn--ghost btn--sm" data-act="export"><span>Export CSV</span><svg class="icon" aria-hidden="true"><use href="#i-download"/></svg></button>' +
        '<button type="button" class="btn btn--danger btn--sm" data-act="end" disabled><span>End task</span><svg class="icon" aria-hidden="true"><use href="#i-stop"/></svg></button>' +
      '</div>' +
      '<div class="proc-panel">' +
        '<div class="proc-scroll" tabindex="0" role="treegrid" aria-label="Processes" aria-rowcount="1" aria-colcount="8">' +
          '<div class="proc-inner">' +
            `<div class="proc-thead" role="row" aria-rowindex="1">${ths}</div>` +
            '<div class="proc-canvas" role="rowgroup"></div>' +
          '</div>' +
        '</div>' +
        '<div class="empty proc-empty" hidden>' +
          '<p class="micro" data-empty-micro></p>' +
          '<p class="proc-empty-title" data-empty-title></p>' +
          '<button type="button" class="btn btn--ghost btn--sm" data-act="clear" hidden><span>Clear filters</span><svg class="icon" aria-hidden="true"><use href="#i-x"/></svg></button>' +
        '</div>' +
      '</div>' +
    '</div>'
  );
}

export function mount(rootEl) {
  if (!rootEl) return;
  state.root = rootEl;
  // The table runs its own scroller, so the view fills #main instead of growing.
  rootEl.classList.add('view--fill');
  rootEl.innerHTML = viewTemplate();
  const q = (s) => rootEl.querySelector(s);
  const els = {
    view: q('.proc-view'),
    scroll: q('.proc-scroll'),
    thead: q('.proc-thead'),
    canvas: q('.proc-canvas'),
    count: q('[data-count]'),
    endBtn: q('[data-act="end"]'),
    empty: q('.proc-empty'),
    emptyMicro: q('[data-empty-micro]'),
    emptyTitle: q('[data-empty-title]'),
    clearBtn: q('[data-act="clear"]'),
    stats: { count: q('[data-stat="count"]'), cpu: q('[data-stat="cpu"]'), mem: q('[data-stat="mem"]') },
    totals: {},
    ths: {},
    segs: Array.from(rootEl.querySelectorAll('[data-group]')),
    chips: Array.from(rootEl.querySelectorAll('[data-chip]')),
  };
  for (const c of COLUMNS) {
    els.totals[c.key] = q(`[data-total="${c.key}"]`);
    els.ths[c.key] = q(`.proc-th[data-c="${c.key}"]`);
  }
  state.els = els;

  rootEl.addEventListener('click', onRootClick);
  els.scroll.addEventListener('scroll', onScroll, { passive: true });
  els.scroll.addEventListener('click', onGridClick);
  els.scroll.addEventListener('dblclick', onGridDblClick);
  els.scroll.addEventListener('contextmenu', onGridContextMenu);
  els.scroll.addEventListener('keydown', onGridKey);

  document.addEventListener('pointerdown', (e) => {
    state.lastPointer = { x: e.clientX, y: e.clientY };
  }, true);

  if (typeof ResizeObserver === 'function') {
    const ro = new ResizeObserver((entries) => {
      for (const e of entries) {
        const box = e.borderBoxSize && e.borderBoxSize[0];
        const h = box ? box.blockSize : e.contentRect.height;
        if (e.target === els.scroll) state.viewH = els.scroll.clientHeight || h;
        else if (e.target === els.thead && h > 0) state.headH = h;
      }
      if (isShown()) {
        updateHead();
        updateToolbar();
        scheduleRender();
      }
    });
    ro.observe(els.scroll);
    ro.observe(els.thead);
  } else {
    window.addEventListener('resize', () => {
      state.viewH = els.scroll.clientHeight;
      scheduleRender();
    });
    state.viewH = els.scroll.clientHeight;
  }

  on('snapshot', () => {
    // Hidden view or minimised window: skip the model. onShow, visibilitychange,
    // exportCsv and 'select' rebuild a stale one.
    if (isShown() && !document.hidden) refresh();
    requestIcons();
  });
  document.addEventListener('visibilitychange', () => {
    if (!document.hidden && isShown() && state.lastSnap !== store.snapshot) refresh();
  });
  on('query', () => {
    state.filterCollapsed.clear();
    buildModel();
    resetScroll();
    refreshVisible();
  });
  on('settings', () => {
    applySettings();
    refresh();
  });
  on('select', (data) => {
    const pid = data && typeof data === 'object' ? data.pid : data;
    if (pid == null) return;
    const n = Number(pid);
    const cur = selectedRow();
    if (state.lastSnap !== store.snapshot) buildModel();
    if (cur && cur.pid === n) return;
    if (!state.byPid.has(n)) return;
    const key = `p:${n}`;
    state.selKey = key;
    updateEndButton();
    const i = state.model.keyIndex.get(key);
    if (i != null && isShown()) ensureVisible(i);
    scheduleRender();
  });

  applySettings();
  updateSortHeaders();
  updateChips();
  buildModel();
  requestIcons();
  refreshVisible();
}

export function onShow() {
  state.visible = true;
  state.showKnown = true;
  if (state.els) state.scrollTop = state.els.scroll.scrollTop;
  if (state.lastSnap !== store.snapshot) buildModel();
  refreshVisible();
}

export function onHide() {
  state.visible = false;
  state.showKnown = true;
}

function isShown() {
  return state.showKnown ? state.visible : state.viewH > 0;
}

function refresh() {
  buildModel();
  refreshVisible();
}

function refreshVisible() {
  if (!state.els || !isShown()) return;
  updateHead();
  updateToolbar();
  scheduleRender();
}

function resetScroll() {
  if (!state.els) return;
  state.els.scroll.scrollTop = 0;
  state.scrollTop = 0;
}

function applySettings() {
  const s = store.settings || {};
  const rowH = s.compact ? ROW_H_COMPACT : ROW_H;
  const g = s.grouping === 'tree' || s.grouping === 'flat' ? s.grouping : 'grouped';
  if (!state.els) return;
  if (rowH !== state.rowH || !state.els.view.style.getPropertyValue('--proc-row-h')) {
    state.rowH = rowH;
    state.els.view.style.setProperty('--proc-row-h', `${rowH}px`);
    state.els.view.classList.toggle('is-compact', rowH === ROW_H_COMPACT);
  }
  if (g !== state.grouping) resetScroll();
  for (const b of state.els.segs) b.setAttribute('aria-pressed', b.dataset.group === g ? 'true' : 'false');
}

function updateChips() {
  if (!state.els) return;
  for (const b of state.els.chips) {
    const on = state.chips.has(b.dataset.chip);
    b.classList.toggle('chip--on', on);
    b.setAttribute('aria-pressed', on ? 'true' : 'false');
  }
}

function updateSortHeaders() {
  if (!state.els) return;
  for (const c of COLUMNS) {
    const th = state.els.ths[c.key];
    const on = state.sort.key === c.key;
    th.classList.toggle('is-sorted-asc', on && state.sort.dir === 'asc');
    th.classList.toggle('is-sorted-desc', on && state.sort.dir === 'desc');
    th.setAttribute('aria-sort', on ? (state.sort.dir === 'asc' ? 'ascending' : 'descending') : 'none');
    const use = th.querySelector('.proc-sort-ico use');
    if (use) use.setAttribute('href', on && state.sort.dir === 'asc' ? '#i-arrow-up' : '#i-arrow-down');
  }
}

function setSort(key) {
  if (!COL_KEYS.has(key)) return;
  if (state.sort.key === key) state.sort = { key, dir: state.sort.dir === 'asc' ? 'desc' : 'asc' };
  else state.sort = { key, dir: ASC_FIRST.has(key) ? 'asc' : 'desc' };
  saveSort();
  updateSortHeaders();
  refresh();
}

function setText(cache, key, el, value) {
  if (cache[key] !== value) {
    cache[key] = value;
    el.textContent = value;
  }
}

function updateHead() {
  const snap = state.lastSnap;
  const els = state.els;
  if (!snap || !els) return;
  const hc = state.hc;
  const mem = snap.memory || {};
  const total = mem.total || (store.staticInfo && store.staticInfo.totalMemory) || 0;
  const used = mem.used || 0;
  const cpu = snap.cpu && Number.isFinite(snap.cpu.usage) ? snap.cpu.usage : 0;
  const procCount = Array.isArray(snap.processes) ? snap.processes.length : 0;
  setText(hc, 'count', els.stats.count, String(procCount));
  setText(hc, 'cpu', els.stats.cpu, pct(cpu, 0));
  setText(hc, 'mem', els.stats.mem, `${bytes(used)} / ${bytes(total)}`);
  setText(hc, 't-cpu', els.totals.cpu, pct(cpu, 0));
  setText(hc, 't-memory', els.totals.memory, total > 0 ? pct((used / total) * 100, 0) : DASH);
  // The I/O column shows per-process rates, so its header sums the rows that pass the filters.
  let io = 0;
  for (const r of state.model.filtered) io += (r.diskReadBps || 0) + (r.diskWriteBps || 0);
  setText(hc, 't-disk', els.totals.disk, rate(io));
  const gpu = snap.gpu || {};
  setText(hc, 't-gpu', els.totals.gpu, gpu.usage != null ? pct(gpu.usage, 0) : DASH);
}

function updateEndButton() {
  if (!state.els) return;
  const item = selectedItem();
  const enabled = item ? item.t !== 'sec' : !!selectedRow();
  if (state.els.endBtn.disabled === enabled) state.els.endBtn.disabled = !enabled;
  const r = selectedRow();
  const title = enabled && r ? (item && item.t === 'grp' ? `End all ${r.name}` : `End ${r.name} (${r.pid})`) : 'Select a process first';
  if (state.els.endBtn.title !== title) state.els.endBtn.title = title;
}

function updateToolbar() {
  const els = state.els;
  if (!els) return;
  const m = state.model;
  setText(state.hc, 'showing', els.count, `Showing ${m.filtered.length} of ${m.total}`);
  const mineBtn = els.chips.find((b) => b.dataset.chip === 'mine');
  if (mineBtn) {
    const t = state.mineUser ? `Processes run by ${state.mineUser}` : 'Processes run by you';
    if (mineBtn.title !== t) mineBtn.title = t;
  }
  updateEndButton();
  const empty = m.items.length === 0;
  if (els.empty.hidden === empty) els.empty.hidden = !empty;
  if (empty) {
    if (!state.lastSnap) {
      setText(state.hc, 'em', els.emptyMicro, 'Live / Waiting');
      setText(state.hc, 'et', els.emptyTitle, 'Waiting for the first sample.');
      els.clearBtn.hidden = true;
    } else if (m.filterActive) {
      setText(state.hc, 'em', els.emptyMicro, '0 / Matches');
      setText(state.hc, 'et', els.emptyTitle, store.query ? `No processes match "${String(store.query).trim()}".` : 'No processes match these filters.');
      els.clearBtn.hidden = false;
    } else {
      setText(state.hc, 'em', els.emptyMicro, '0 / Processes');
      setText(state.hc, 'et', els.emptyTitle, 'No processes to show.');
      els.clearBtn.hidden = true;
    }
  }
}

/* ------------------------------------------------------------------ */
/* Icons                                                               */
/* ------------------------------------------------------------------ */

function requestIcons() {
  const need = [];
  for (const r of currentRows()) {
    if (r.exe && !state.iconReq.has(r.exe)) {
      state.iconReq.add(r.exe);
      need.push(r.exe);
    }
  }
  for (let i = 0; i < need.length; i += ICON_BATCH) {
    const paths = need.slice(i, i + ICON_BATCH);
    call('get_icons', { paths }).then((map) => {
      let added = false;
      for (const p of paths) {
        const src = map && typeof map === 'object' ? map[p] : undefined;
        if (typeof src === 'string' && src.startsWith('data:image/')) {
          state.iconMap.set(p, src);
          added = true;
        } else {
          retryIcon(p);
        }
      }
      if (added && isShown()) scheduleRender();
    }).catch(() => { for (const p of paths) retryIcon(p); });
  }
}

// Icon lookups can fail for a while after Orbit starts: ask again a few times, later.
function retryIcon(path) {
  const n = (state.iconTries.get(path) || 0) + 1;
  state.iconTries.set(path, n);
  if (n < ICON_TRIES) setTimeout(() => state.iconReq.delete(path), ICON_RETRY_MS * n);
}

/* ------------------------------------------------------------------ */
/* Virtualised rendering                                               */
/* ------------------------------------------------------------------ */

function scheduleRender() {
  if (!state.raf) state.raf = requestAnimationFrame(render);
}

function growPool(need) {
  const canvas = state.els.canvas;
  const size = need + 4;
  while (state.pool.length < size) {
    const el = document.createElement('div');
    el.className = 'proc-row';
    el.setAttribute('role', 'row');
    el.id = `proc-slot-${state.pool.length}`;
    el.hidden = true;
    canvas.appendChild(el);
    state.pool.push({ el, i: -1, y: -1, type: null, c: {}, x: null, hidden: true });
  }
  for (const s of state.pool) s.i = -1;
}

function render() {
  state.raf = 0;
  const els = state.els;
  if (!els || !isShown()) return;
  const { items } = state.model;
  const n = items.length;
  const rowH = state.rowH;
  const height = n * rowH;
  if (state.hc.h !== height) {
    state.hc.h = height;
    els.canvas.style.height = `${height}px`;
    els.scroll.setAttribute('aria-rowcount', String(n + 1));
  }
  const avail = Math.max(rowH, state.viewH - state.headH);
  const need = Math.ceil(avail / rowH) + OVERSCAN * 2 + 1;
  if (need > state.pool.length) growPool(need);
  const size = state.pool.length;
  const top = state.scrollTop;
  const start = Math.max(0, Math.floor(top / rowH) - OVERSCAN);
  const end = Math.min(n, start + size, Math.ceil((top + avail) / rowH) + OVERSCAN);
  const selIdx = selIndex();
  for (let i = start; i < end; i++) bindSlot(state.pool[i % size], i, items[i], i === selIdx);
  for (let k = 0; k < size; k++) {
    const slot = state.pool[k];
    if (slot.i < start || slot.i >= end) {
      if (!slot.hidden) {
        slot.el.hidden = true;
        slot.hidden = true;
      }
      slot.i = -1;
    }
  }
  const active = selIdx >= start && selIdx < end ? state.pool[selIdx % size].el.id : null;
  if (state.activeDesc !== active) {
    state.activeDesc = active;
    if (active) els.scroll.setAttribute('aria-activedescendant', active);
    else els.scroll.removeAttribute('aria-activedescendant');
  }
}

function buildSlot(slot, type) {
  const el = slot.el;
  el.className = type === 'sec' ? 'proc-row proc-row--section' : 'proc-row row';
  el.innerHTML = type === 'sec' ? SEC_TPL : ROW_TPL;
  slot.type = type;
  slot.c = {};
  const q = (s) => el.querySelector(s);
  slot.x = type === 'sec'
    ? { label: q('.proc-sec-label'), count: q('.proc-sec-count'), sum: q('.proc-sec-sum') }
    : {
      name: q('[data-c="name"]'),
      ico: q('.proc-ico'),
      text: q('.proc-name'),
      n: q('.proc-n'),
      meta: q('.proc-meta'),
      tags: q('.proc-tags'),
      pid: q('[data-c="pid"]'),
      status: q('[data-c="status"]'),
      user: q('[data-c="user"]'),
      cpu: q('[data-c="cpu"]'),
      memory: q('[data-c="memory"]'),
      disk: q('[data-c="disk"]'),
      gpu: q('[data-c="gpu"]'),
    };
}

function flag(slot, name, on) {
  if (slot.c[name] !== on) {
    slot.c[name] = on;
    slot.el.classList.toggle(name, on);
  }
}

function attr(slot, key, name, value) {
  if (slot.c[key] !== value) {
    slot.c[key] = value;
    if (value == null) slot.el.removeAttribute(name);
    else slot.el.setAttribute(name, value);
  }
}

function heatCell(slot, key, el, value) {
  if (slot.c[key] !== value) {
    slot.c[key] = value;
    el.style.setProperty('--h', String(value));
  }
}

function bindSlot(slot, i, item, selected) {
  if (slot.hidden) {
    slot.el.hidden = false;
    slot.hidden = false;
  }
  const y = i * state.rowH;
  if (slot.y !== y) {
    slot.y = y;
    slot.el.style.transform = `translateY(${y}px)`;
  }
  const type = item.t === 'sec' ? 'sec' : 'row';
  if (slot.type !== type) buildSlot(slot, type);
  if (slot.i !== i) {
    slot.i = i;
    slot.el.dataset.i = String(i);
  }
  attr(slot, 'ri', 'aria-rowindex', String(i + 2));
  if (type === 'sec') patchSection(slot, item, selected);
  else patchRow(slot, item, selected);
}

function patchSection(slot, item, selected) {
  const x = slot.x;
  flag(slot, 'is-selected', selected);
  flag(slot, 'is-open', item.open);
  attr(slot, 'sel', 'aria-selected', selected ? 'true' : 'false');
  attr(slot, 'exp', 'aria-expanded', item.open ? 'true' : 'false');
  attr(slot, 'lvl', 'aria-level', '1');
  setText(slot.c, 'label', x.label, item.label);
  setText(slot.c, 'count', x.count, `(${item.n})`);
  setText(slot.c, 'sum', x.sum, `${pct(item.cpu, 1)} CPU / ${bytes(item.memory)}`);
}

function highlight(name, terms) {
  const safe = String(name || '');
  if (!terms.length) return escapeHtml(safe);
  const lower = safe.toLowerCase();
  if (lower.length !== safe.length) return escapeHtml(safe);
  const marks = new Uint8Array(safe.length);
  let any = false;
  for (const t of terms) {
    const at = lower.indexOf(t);
    if (at >= 0) {
      marks.fill(1, at, at + t.length);
      any = true;
      continue;
    }
    let j = 0;
    const pos = [];
    for (let i = 0; i < lower.length && j < t.length; i++) {
      if (lower[i] === t[j]) {
        pos.push(i);
        j++;
      }
    }
    if (j === t.length) {
      for (const p of pos) marks[p] = 1;
      any = true;
    }
  }
  if (!any) return escapeHtml(safe);
  let out = '';
  let i = 0;
  while (i < safe.length) {
    const m = marks[i];
    let j = i;
    while (j < safe.length && marks[j] === m) j++;
    const part = escapeHtml(safe.slice(i, j));
    out += m ? `<mark>${part}</mark>` : part;
    i = j;
  }
  return out;
}

function memHeatMax() {
  const total = (store.staticInfo && store.staticInfo.totalMemory) || (state.lastSnap && state.lastSnap.memory && state.lastSnap.memory.total) || 0;
  return Math.max(512 * 1024 * 1024, total * 0.12);
}

function patchRow(slot, item, selected) {
  const c = slot.c;
  const x = slot.x;
  const isGrp = item.t === 'grp';
  const r = isGrp ? item.g.v : item.r;
  const expandable = isGrp || item.kids > 0;
  const pinned = !isGrp && isPinnedRow(r, state.pins);

  flag(slot, 'is-selected', selected);
  flag(slot, 'is-dim', !!item.dim);
  flag(slot, 'is-group', isGrp);
  flag(slot, 'has-twisty', expandable);
  flag(slot, 'is-open', expandable && item.open);
  flag(slot, 'is-pinned', pinned);
  attr(slot, 'sel', 'aria-selected', selected ? 'true' : 'false');
  attr(slot, 'exp', 'aria-expanded', expandable ? (item.open ? 'true' : 'false') : null);
  attr(slot, 'lvl', 'aria-level', String(item.depth + (state.grouping === 'grouped' ? 2 : 1)));

  const d = Math.min(item.depth, MAX_INDENT);
  if (c.d !== d) {
    c.d = d;
    x.name.style.setProperty('--d', String(d));
  }

  // Icon: exe icon when the backend has one, otherwise a generic glyph.
  const src = r.exe ? state.iconMap.get(r.exe) : undefined;
  const iconKey = src || (r.kind === 'windows' ? '#i-shield' : '#i-grid');
  if (c.ico !== iconKey) {
    c.ico = iconKey;
    if (src) {
      const img = document.createElement('img');
      img.alt = '';
      img.width = 16;
      img.height = 16;
      img.decoding = 'async';
      img.src = src;
      x.ico.replaceChildren(img);
    } else {
      x.ico.innerHTML = `<svg class="icon"><use href="${iconKey}"/></svg>`;
    }
  }

  const nameKey = `${r.name}\u0000${state.queryKey}`;
  if (c.nameKey !== nameKey) {
    c.nameKey = nameKey;
    x.text.innerHTML = highlight(r.name, state.terms);
  }
  setText(c, 'n', x.n, isGrp ? `(${item.g.members.length})` : '');
  setText(c, 'meta', x.meta, (isGrp || r.kind === 'app') && r.windowTitle ? r.windowTitle : '');
  const title = r.windowTitle ? `${r.name} / ${r.windowTitle}` : (r.exe || r.name);
  if (c.title !== title) {
    c.title = title;
    x.name.title = title;
  }

  const tagKey = (r.notResponding ? 'n' : '') + (r.suspended ? 's' : '') + (pinned ? 'p' : '');
  if (c.tags !== tagKey) {
    c.tags = tagKey;
    x.tags.innerHTML =
      (r.notResponding ? '<span class="proc-tag proc-tag--warn">Not responding</span>' : '') +
      (r.suspended ? '<span class="proc-tag">Suspended</span>' : '') +
      (pinned ? '<span class="proc-pin" title="Pinned to watchlist"><svg class="icon" aria-hidden="true"><use href="#i-pin"/></svg></span>' : '');
  }

  setText(c, 'pid', x.pid, isGrp ? '' : String(r.pid));
  const status = statusLabel(r);
  setText(c, 'status', x.status, status);
  const tone = r.notResponding ? 'warn' : r.suspended ? 'muted' : '';
  if (c.tone !== tone) {
    c.tone = tone;
    x.status.classList.toggle('is-warn', tone === 'warn');
    x.status.classList.toggle('is-muted', tone === 'muted');
  }
  setText(c, 'user', x.user, r.user || DASH);

  setText(c, 'cpu', x.cpu, pct(r.cpu || 0, 1));
  heatCell(slot, 'hcpu', x.cpu, heatOf(r.cpu, 40));
  setText(c, 'mem', x.memory, bytes(r.memory || 0));
  heatCell(slot, 'hmem', x.memory, heatOf(r.memory, memHeatMax()));
  const rd = r.diskReadBps || 0;
  const wr = r.diskWriteBps || 0;
  setText(c, 'disk', x.disk, rate(rd + wr));
  heatCell(slot, 'hdisk', x.disk, heatOf(rd + wr, 25 * 1024 * 1024));
  const diskTitle = `Read ${rate(rd)} / Write ${rate(wr)}`;
  if (c.diskTitle !== diskTitle) {
    c.diskTitle = diskTitle;
    x.disk.title = diskTitle;
  }
  setText(c, 'gpu', x.gpu, r.gpu == null ? DASH : pct(r.gpu, 1));
  heatCell(slot, 'hgpu', x.gpu, heatOf(r.gpu, 50));
}

/* ------------------------------------------------------------------ */
/* Events                                                              */
/* ------------------------------------------------------------------ */

function onScroll() {
  state.scrollTop = state.els.scroll.scrollTop;
  scheduleRender();
}

function itemFromEvent(e) {
  const rowEl = e.target instanceof Element ? e.target.closest('.proc-canvas > .proc-row') : null;
  if (!rowEl) return null;
  const i = Number(rowEl.dataset.i);
  return Number.isInteger(i) ? state.model.items[i] || null : null;
}

function onRootClick(e) {
  const t = e.target instanceof Element ? e.target : null;
  if (!t) return;
  const seg = t.closest('[data-group]');
  if (seg) {
    const g = seg.dataset.group;
    if (g !== state.grouping) setSettings({ grouping: g });
    return;
  }
  const chip = t.closest('[data-chip]');
  if (chip) {
    const id = chip.dataset.chip;
    if (state.chips.has(id)) state.chips.delete(id);
    else state.chips.add(id);
    state.filterCollapsed.clear();
    updateChips();
    buildModel();
    resetScroll();
    refreshVisible();
    return;
  }
  const act = t.closest('[data-act]');
  if (!act || !state.root.contains(act)) return;
  const a = act.dataset.act;
  if (a === 'export') exportCsv();
  else if (a === 'run') openRunTask();
  else if (a === 'end') endSelected();
  else if (a === 'clear') clearFilters();
}

function clearFilters() {
  state.chips.clear();
  state.filterCollapsed.clear();
  updateChips();
  const input = document.getElementById('search');
  if (input && 'value' in input) input.value = '';
  if (store.query) setQuery('');
  else {
    buildModel();
    refreshVisible();
  }
  if (state.els) state.els.scroll.focus({ preventScroll: true });
}

function onGridClick(e) {
  const t = e.target instanceof Element ? e.target : null;
  if (!t) return;
  const sortBtn = t.closest('.proc-sort');
  if (sortBtn) {
    setSort(sortBtn.dataset.sort);
    return;
  }
  const item = itemFromEvent(e);
  if (!item) return;
  if (item.t === 'sec' || t.closest('.proc-twisty')) {
    setSelection(item.key, false);
    toggleItem(item);
    return;
  }
  setSelection(item.key, false);
}

function onGridDblClick(e) {
  const t = e.target instanceof Element ? e.target : null;
  if (!t || t.closest('.proc-twisty') || t.closest('.proc-thead')) return;
  const item = itemFromEvent(e);
  if (!item || item.t === 'sec') return;
  openDetailsFor(item);
}

function onGridContextMenu(e) {
  const item = itemFromEvent(e);
  if (!item) return;
  e.preventDefault();
  setSelection(item.key, false);
  state.lastPointer = { x: e.clientX, y: e.clientY };
  showMenu(e.clientX, e.clientY, menuItemsFor(item));
}

function openMenuForSelection() {
  const item = selectedItem();
  const row = item ? null : selectedRow();
  if (!item && !row) return;
  const i = selIndex();
  const size = state.pool.length;
  let x = Math.round(window.innerWidth / 2);
  let y = Math.round(window.innerHeight / 3);
  if (size && i >= 0) {
    const slot = state.pool[i % size];
    if (slot.i === i) {
      const rect = slot.el.getBoundingClientRect();
      x = Math.round(rect.left + 48);
      y = Math.round(rect.bottom);
    }
  }
  state.lastPointer = { x, y };
  showMenu(x, y, item ? menuItemsFor(item) : processMenuItems(row));
}

function parentIndex(idx) {
  const items = state.model.items;
  const item = items[idx];
  if (!item) return -1;
  for (let i = idx - 1; i >= 0; i--) {
    const it = items[i];
    if (it.t === 'sec') return item.t === 'sec' ? -1 : i;
    if (it.depth < item.depth) return i;
  }
  return -1;
}

function onGridKey(e) {
  const items = state.model.items;
  const key = e.key;
  const ctrl = e.ctrlKey || e.metaKey;
  const stop = () => {
    e.preventDefault();
    e.stopPropagation();
  };

  if (ctrl && (key === 'c' || key === 'C')) {
    const r = selectedRow();
    if (!r) return;
    stop();
    copyText(`${r.name} (${r.pid}) ${r.exe || ''}`.trim()).then((ok) => {
      toast(ok ? `Copied ${r.name} (${r.pid}).` : 'Could not copy to the clipboard.', ok ? 'success' : 'error');
    });
    return;
  }
  if (key === 'ContextMenu' || (e.shiftKey && key === 'F10')) {
    if (!selectedItem() && !selectedRow()) return;
    stop();
    openMenuForSelection();
    return;
  }
  if (ctrl || e.altKey || !items.length) return;

  const idx = selIndex();
  const item = idx >= 0 ? items[idx] : null;
  const exact = selectedItem();
  const page = Math.max(1, Math.floor((state.viewH - state.headH) / state.rowH) - 1);
  const expandable = item && (item.t === 'sec' || item.t === 'grp' || item.kids > 0);

  switch (key) {
    case 'ArrowDown': moveTo(idx < 0 ? 0 : idx + 1); break;
    case 'ArrowUp': moveTo(idx < 0 ? 0 : idx - 1); break;
    case 'PageDown': moveTo(idx < 0 ? 0 : idx + page); break;
    case 'PageUp': moveTo(idx < 0 ? 0 : idx - page); break;
    case 'Home': moveTo(0); break;
    case 'End': moveTo(items.length - 1); break;
    case 'ArrowRight':
      if (!item) moveTo(0);
      else if (expandable && !item.open) toggleItem(item);
      else if (expandable) moveTo(idx + 1);
      break;
    case 'ArrowLeft':
      if (!item) moveTo(0);
      else if (expandable && item.open) toggleItem(item);
      else {
        const p = parentIndex(idx);
        if (p >= 0) moveTo(p);
      }
      break;
    case 'Enter':
      if (exact) openDetailsFor(exact);
      else if (selectedRow()) selectProcess(selectedRow().pid);
      else return;
      break;
    case 'Delete':
      if (exact ? exact.t === 'sec' : !selectedRow()) return;
      endSelected();
      break;
    default:
      return;
  }
  stop();
}
