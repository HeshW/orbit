// Orbit / store: the latest snapshot, 60-sample history, settings and a tiny event bus.
//
// history units:
//   cpu, diskActive, gpu, perCore[i]  percent 0-100 (null in the snapshot is stored as 0)
//   mem                               percent of total memory in use, 0-100
//   diskRead, diskWrite, rx, tx       bytes per second
//   proc: Map pid -> { name, cpu: number[] (percent), mem: number[] (bytes) }
//
// settings.watchlist: [{ pid, start }] (start = the process startTime, epoch seconds)

export const HISTORY_LEN = 60;

const SETTINGS_KEY = 'orbit.settings';
const THEMES = ['night', 'void', 'signal', 'daylight'];
const GROUPINGS = ['grouped', 'tree', 'flat'];
const HISTORY_RANGES = ['today', '7', '30', 'custom'];

const DEFAULT_SETTINGS = Object.freeze({
  theme: 'night',
  intervalMs: 1000,
  paused: false,
  compact: false,
  reducedMotion: false,
  grouping: 'grouped',
  alwaysOnTop: false,
  autoUpdate: true,
  // v0.4 (docs/V04-CONTRACT.md)
  showTray: true,
  closeToTray: false,
  startWithWindows: false,
  startMinimized: true,
  hotkeyEnabled: true,
  historyEnabled: true,
  historyDays: 30,
  historyRange: 'today',
  historyCustomDays: 14,
  watchlist: [],
  watchNames: [],
  // v0.5 Optimize > Boost (sent to Rust with boost_configure; docs/features/boost.md)
  boostGames: [],
  boostApps: [],
  boostAuto: false,
  boostPause: false,
  boostPower: true,
  // v0.7 Performance temperatures: 'c' or 'f' (readings stay °C, only the text changes)
  tempUnit: 'c',
  // v0.8: the startup suggestions card was hidden for this exact set of apps (a short hash of
  // their ids). The card returns when the suggested apps change. '' = not hidden.
  hiddenSuggestions: '',
});

function sanitize(raw) {
  const s = { ...DEFAULT_SETTINGS, watchlist: [], watchNames: [], boostGames: [], boostApps: [] };
  if (!raw || typeof raw !== 'object') return s;
  if (THEMES.includes(raw.theme)) s.theme = raw.theme;
  const iv = Number(raw.intervalMs);
  if (Number.isFinite(iv) && iv >= 250 && iv <= 10000) s.intervalMs = Math.round(iv);
  if (typeof raw.paused === 'boolean') s.paused = raw.paused;
  if (typeof raw.compact === 'boolean') s.compact = raw.compact;
  if (typeof raw.reducedMotion === 'boolean') s.reducedMotion = raw.reducedMotion;
  if (GROUPINGS.includes(raw.grouping)) s.grouping = raw.grouping;
  if (raw.tempUnit === 'c' || raw.tempUnit === 'f') s.tempUnit = raw.tempUnit;
  if (typeof raw.hiddenSuggestions === 'string') s.hiddenSuggestions = raw.hiddenSuggestions.slice(0, 64);
  if (typeof raw.alwaysOnTop === 'boolean') s.alwaysOnTop = raw.alwaysOnTop;
  if (typeof raw.autoUpdate === 'boolean') s.autoUpdate = raw.autoUpdate;
  for (const k of ['showTray', 'closeToTray', 'startWithWindows', 'startMinimized', 'hotkeyEnabled', 'historyEnabled', 'boostAuto', 'boostPause', 'boostPower']) {
    if (typeof raw[k] === 'boolean') s[k] = raw[k];
  }
  const days = Number(raw.historyDays);
  if (Number.isFinite(days)) s.historyDays = Math.min(90, Math.max(1, Math.round(days)));
  if (HISTORY_RANGES.includes(raw.historyRange)) s.historyRange = raw.historyRange;
  const custom = Number(raw.historyCustomDays);
  if (Number.isFinite(custom)) s.historyCustomDays = Math.min(90, Math.max(1, Math.round(custom)));
  if (Array.isArray(raw.watchlist)) {
    // Each pid pin is bound to the process start time, so a pid Windows reuses later
    // is not pinned. Old bare-number entries cannot be checked and are dropped.
    const byPid = new Map();
    for (const e of raw.watchlist) {
      if (!e || typeof e !== 'object') continue;
      const pid = Number(e.pid);
      const start = Number(e.start);
      if (Number.isInteger(pid) && pid >= 0 && Number.isFinite(start)) byPid.set(pid, { pid, start });
    }
    s.watchlist = [...byPid.values()];
  }
  if (Array.isArray(raw.watchNames)) {
    s.watchNames = [...new Set(raw.watchNames.filter((n) => typeof n === 'string' && n.trim()).map((n) => n.trim()))];
  }
  for (const k of ['boostGames', 'boostApps']) {
    if (!Array.isArray(raw[k])) continue;
    const seen = new Set();
    s[k] = raw[k].filter((n) => typeof n === 'string' && n.trim() && !seen.has(n.trim().toLowerCase()) && seen.add(n.trim().toLowerCase())).map((n) => n.trim()).slice(0, 64);
  }
  // Keep any extra keys other modules store, as long as they are plain JSON values.
  for (const [k, v] of Object.entries(raw)) {
    if (!(k in s) && (v === null || ['string', 'number', 'boolean', 'object'].includes(typeof v))) s[k] = v;
  }
  return s;
}

function loadSettings() {
  try {
    const raw = localStorage.getItem(SETTINGS_KEY);
    return sanitize(raw ? JSON.parse(raw) : null);
  } catch {
    return sanitize(null);
  }
}

function saveSettings(s) {
  try {
    localStorage.setItem(SETTINGS_KEY, JSON.stringify(s));
  } catch {
    /* storage unavailable: settings live for this session only */
  }
}

export const store = {
  snapshot: null,
  staticInfo: null,
  history: {
    cpu: [],
    mem: [],
    diskRead: [],
    diskWrite: [],
    diskActive: [],
    rx: [],
    tx: [],
    gpu: [],
    perCore: [[]],
    proc: new Map(),
  },
  settings: loadSettings(),
  query: '',
  selected: null, // pid currently shown in the details drawer (set by selectProcess)
};

// ---------------------------------------------------------------- watchlist pins

/** True when a pin's start time (epoch seconds) belongs to this process row (1 s slack). */
export function pinStartMatches(start, row) {
  return !!row && Math.abs(Number(start) - (Number(row.startTime) || 0)) <= 1;
}

/** True when `list` (settings.watchlist) has a pid pin for this exact process. */
export function isPidPinned(row, list) {
  if (!row || !Array.isArray(list)) return false;
  for (const e of list) if (e && e.pid === row.pid && pinStartMatches(e.start, row)) return true;
  return false;
}

/** A watchlist entry for this process row. */
export function pinEntry(row) {
  return { pid: row.pid, start: Number(row.startTime) || 0 };
}

// ---------------------------------------------------------------- events

const listeners = new Map();

/** Subscribe. Returns an unsubscribe function. */
export function on(event, cb) {
  let set = listeners.get(event);
  if (!set) {
    set = new Set();
    listeners.set(event, set);
  }
  set.add(cb);
  return () => set.delete(cb);
}

export function emit(event, data) {
  const set = listeners.get(event);
  if (!set) return;
  for (const cb of [...set]) {
    try {
      cb(data);
    } catch (err) {
      console.error(`[orbit] "${event}" listener failed`, err);
    }
  }
}

// ---------------------------------------------------------------- settings / query / select

/** Merge a patch into settings, persist, and emit 'settings' with the full settings object. */
export function setSettings(patch) {
  if (!patch || typeof patch !== 'object') return;
  const next = sanitize({ ...store.settings, ...patch });
  let changed = false;
  for (const k of Object.keys(next)) {
    if (JSON.stringify(next[k]) !== JSON.stringify(store.settings[k])) {
      changed = true;
      break;
    }
  }
  if (!changed) return;
  store.settings = next;
  saveSettings(next);
  emit('settings', store.settings);
}

export function setQuery(q) {
  const v = q === null || q === undefined ? '' : String(q);
  if (v === store.query) return;
  store.query = v;
  emit('query', v);
}

export function selectProcess(pid) {
  store.selected = pid === null || pid === undefined ? null : Number(pid);
  emit('select', store.selected);
}

// ---------------------------------------------------------------- snapshot + history

function pushRing(arr, v) {
  arr.push(typeof v === 'number' && Number.isFinite(v) ? v : 0);
  if (arr.length > HISTORY_LEN) arr.splice(0, arr.length - HISTORY_LEN);
}

const livePids = new Set();

// pid -> the full ProcessRow of the latest snapshot, for merging SnapshotDelta events.
let rowCache = new Map();

/** True when an orbit://snapshot payload is a SnapshotDelta rather than a full Snapshot. */
export function isDelta(payload) {
  return !!payload && typeof payload === 'object' && !Array.isArray(payload.processes) && Array.isArray(payload.rows);
}

/**
 * Turn a SnapshotDelta (docs/CONTRACT.md) into a full Snapshot using the rows of the last
 * snapshot: `removed` first, then `added`, then each DynRow merged into its cached row.
 * Every tick gets new row objects, in the backend's order, like a full Snapshot.
 * Returns null when a row has no cached static fields (an event was missed): the
 * caller resyncs with get_snapshot. Does not change the store; pass the result to pushSnapshot.
 */
export function applyDelta(delta) {
  if (!isDelta(delta)) return null;
  const cache = new Map(rowCache);
  const removed = Array.isArray(delta.removed) ? delta.removed : [];
  for (let i = 0; i < removed.length; i++) cache.delete(removed[i]);
  const added = Array.isArray(delta.added) ? delta.added : [];
  for (let i = 0; i < added.length; i++) cache.set(added[i].pid, added[i]);
  const rows = delta.rows;
  const processes = new Array(rows.length);
  for (let i = 0; i < rows.length; i++) {
    const d = rows[i];
    const base = cache.get(d.pid);
    if (!base) return null;
    processes[i] = { ...base, ...d };
  }
  const { added: _a, removed: _r, rows: _rows, ...stats } = delta;
  return { ...stats, processes };
}

export function pushSnapshot(snap) {
  if (!snap || typeof snap !== 'object') return;
  const h = store.history;
  const cpu = snap.cpu || {};
  const mem = snap.memory || {};
  const disk = snap.disk || {};
  const net = snap.network || {};
  const gpu = snap.gpu || {};

  pushRing(h.cpu, cpu.usage);
  pushRing(h.mem, mem.total > 0 ? (mem.used / mem.total) * 100 : 0);
  pushRing(h.diskRead, disk.readBps);
  pushRing(h.diskWrite, disk.writeBps);
  pushRing(h.diskActive, disk.activePct);
  pushRing(h.rx, net.rxBps);
  pushRing(h.tx, net.txBps);
  pushRing(h.gpu, gpu.usage);

  const cores = Array.isArray(cpu.perCore) ? cpu.perCore : [];
  if (h.perCore.length !== cores.length) {
    const prev = h.perCore;
    h.perCore = cores.map((_, i) => prev[i] || []);
    if (!h.perCore.length) h.perCore = [[]];
  }
  for (let i = 0; i < cores.length; i++) pushRing(h.perCore[i], cores[i]);

  const rows = Array.isArray(snap.processes) ? snap.processes : [];
  livePids.clear();
  const cache = new Map();
  for (let i = 0; i < rows.length; i++) {
    const r = rows[i];
    livePids.add(r.pid);
    cache.set(r.pid, r);
    let e = h.proc.get(r.pid);
    if (!e || e.name !== r.name) {
      // New pid, or Windows reused the pid for a different program.
      e = { name: r.name, cpu: [], mem: [] };
      h.proc.set(r.pid, e);
    }
    pushRing(e.cpu, r.cpu);
    pushRing(e.mem, r.memory);
  }
  for (const pid of h.proc.keys()) {
    if (!livePids.has(pid)) h.proc.delete(pid);
  }

  rowCache = cache;
  store.snapshot = snap;
  emit('snapshot', snap);
}
