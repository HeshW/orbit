// Orbit / mock: disk cleanup and biggest folders (src-tauri/src/cleanup.rs) for the browser
// preview. Same shapes and rules as Rust: admin categories are skipped unelevated, Edge is
// "open" so its cache is blocked, the 24-hour rule applies to temp folders and the update
// cache. Scans and cleans send orbit://cleanup-progress events and can be cancelled.

const GB = 1024 ** 3;
const MB = 1024 ** 2;
const EVENT = 'orbit://cleanup-progress';
const BROWSER = 'Cached pages and images only. Cookies, history, passwords and settings are never touched.';
const LOCAL = 'C:\\Users\\you\\AppData\\Local';

// [id, label, description, needsAdmin, ageFilter, paths, bytes, files, oldShare, browser]
const DEFS = [
  ['user-temp', 'Your temporary files', 'Files apps leave in your Temp folder.', false, true, [`${LOCAL}\\Temp`], 2.4 * GB, 13202, 0.78],
  ['windows-temp', 'Windows temporary files', 'The Windows Temp folder, used by installers and services.', true, true, ['C:\\Windows\\Temp'], 640 * MB, 812, 0.9],
  ['update-cache', 'Windows Update downloads', 'Update files Windows already downloaded. It fetches them again if it still needs them.', true, true, ['C:\\Windows\\SoftwareDistribution\\Download'], 370 * MB, 19, 0.06],
  ['crash-dumps', 'App crash dumps', 'Memory snapshots apps save when they crash.', false, false, [`${LOCAL}\\CrashDumps`], 320 * MB, 10, 1],
  ['system-dumps', 'Windows crash dumps', 'Minidumps and MEMORY.DMP from blue screens.', true, false, ['C:\\Windows\\Minidump', 'C:\\Windows\\memory.dmp'], 0, 0, 1],
  ['thumbnails', 'Thumbnail cache', 'Picture previews File Explorer saves. It rebuilds them; the ones it has open are skipped.', false, false, [`${LOCAL}\\Microsoft\\Windows\\Explorer\\thumbcache_*.db`], 100 * MB, 15, 1],
  ['browser-edge', 'Microsoft Edge cache', BROWSER, false, false, [`${LOCAL}\\Microsoft\\Edge\\User Data`], 1.0 * GB, 6955, 1, 'Edge'],
  ['browser-chrome', 'Google Chrome cache', BROWSER, false, false, [`${LOCAL}\\Google\\Chrome\\User Data`], 1.1 * GB, 8250, 1, 'Chrome'],
  ['browser-brave', 'Brave cache', BROWSER, false, false, [`${LOCAL}\\BraveSoftware\\Brave-Browser\\User Data`], 0, 0, 1, 'Brave'],
  ['browser-firefox', 'Firefox cache', BROWSER, false, false, [`${LOCAL}\\Mozilla\\Firefox\\Profiles`], 210 * MB, 1890, 1, 'Firefox'],
  ['recycle-bin', 'Recycle Bin', 'Files you deleted. Emptying it removes them for good.', false, false, ['Recycle Bin (every drive)'], 7.7 * MB, 5, 1],
];
const RUNNING = new Set(['Edge']);
const MISSING = new Set(['browser-brave']);

// What is left in each category (cleaning lowers it).
const left = new Map(DEFS.map((d) => [d[0], { bytes: Math.round(d[6]), files: d[7], oldBytes: Math.round(d[6] * d[8]), oldFiles: Math.round(d[7] * d[8]) }]));
const cancel = { scan: false, clean: false, folders: false };
const running = { scan: false, clean: false, folders: false };
const now = () => Math.floor(Date.now() / 1000);

function categoryScan(d, isAdmin) {
  const [id, label, description, needsAdmin, ageFilter, paths, , , , browser] = d;
  const l = left.get(id);
  const readable = !(needsAdmin && !isAdmin && id !== 'update-cache');
  const found = !MISSING.has(id);
  return {
    id, label, description, needsAdmin, ageFilter, paths, found, readable,
    bytes: readable && found ? l.bytes : 0,
    files: readable && found ? l.files : 0,
    oldBytes: readable && found ? (ageFilter ? l.oldBytes : l.bytes) : 0,
    oldFiles: readable && found ? (ageFilter ? l.oldFiles : l.files) : 0,
    unreadable: id === 'user-temp' ? 2 : 0,
    blocked: browser && RUNNING.has(browser) ? `${browser} is open. Close it to clear its cache.` : null,
    note: null,
  };
}

async function steps(job, items, ctx, each) {
  let files = 0;
  let bytes = 0;
  for (let i = 0; i < items.length; i++) {
    if (cancel[job]) break;
    const [label, f, b] = each(items[i]);
    for (let k = 1; k <= 3; k++) {
      ctx.emit(EVENT, { job, label, step: i + 1, steps: items.length, files: files + Math.round((f * k) / 3), bytes: bytes + Math.round((b * k) / 3), done: false });
      await ctx.delay(70);
      if (cancel[job]) break;
    }
    files += f;
    bytes += b;
  }
  ctx.emit(EVENT, { job, label: '', step: items.length, steps: items.length, files, bytes, done: true });
}

function begin(job) {
  if (running[job]) throw `A ${job === 'folders' ? 'folder scan' : job} is already running.`;
  running[job] = true;
  cancel[job] = false;
}

// ---- biggest folders: a made-up tree, the same on every load

const TREE = {
  'C:\\': ['Users', 'Windows', 'Program Files', 'Program Files (x86)', 'ProgramData', 'Games', '$Recycle.Bin'],
  'C:\\Users': ['you', 'Public', 'Default'],
  'C:\\Users\\you': ['AppData', 'Videos', 'Downloads', 'Documents', 'Pictures', 'OneDrive', 'Desktop', 'Music'],
  'C:\\Users\\you\\AppData': ['Local', 'Roaming', 'LocalLow'],
  'C:\\Users\\you\\AppData\\Local': ['Temp', 'Google', 'Microsoft', 'Packages', 'Discord', 'CrashDumps'],
  'C:\\Games': ['Steam', 'Epic Games'],
  'D:\\': ['Backups', 'Media', 'Projects'],
};
const DRIVES = [
  { mount: 'C:\\', name: 'Windows', total: 953 * GB, available: 71 * GB, removable: false },
  { mount: 'D:\\', name: 'Data', total: 1863 * GB, available: 1204 * GB, removable: false },
];

function hash(s) {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619);
  return (h >>> 0) / 4294967296;
}

function sizeOf(path, depth = 0) {
  const kids = TREE[path];
  if (kids && depth < 4) return kids.reduce((a, k) => a + sizeOf(`${path.replace(/\\$/, '')}\\${k}`, depth + 1), 0) + Math.round(hash(`${path}#f`) * 400 * MB);
  return Math.round((hash(path) ** 2) * 60 * GB) + 3 * MB;
}

function listing(path) {
  const base = path.replace(/\\$/, '');
  const names = TREE[path] || ['cache', 'data', 'logs', 'bin', 'assets', 'old'];
  const folders = names.map((name) => {
    const p = `${base}\\${name}`;
    const bytes = sizeOf(p);
    return { name, path: p, bytes, files: Math.round(bytes / (420 * 1024)) + 1, unreadable: name.startsWith('$') ? 1 : 0 };
  }).sort((a, b) => b.bytes - a.bytes);
  const filesHereBytes = Math.round(hash(`${path}#f`) * 400 * MB) + (path === 'C:\\' ? 16 * GB : 0);
  const bytes = folders.reduce((a, f) => a + f.bytes, 0) + filesHereBytes;
  return {
    path, bytes, files: folders.reduce((a, f) => a + f.files, 0) + 12, folders,
    otherBytes: 0, otherCount: 0, filesHereBytes, filesHere: 12,
    unreadable: folders.reduce((a, f) => a + f.unreadable, 0), links: path === 'C:\\Users\\you' ? 1 : 0,
    cancelled: false, scannedAt: now(), cached: false,
  };
}

export default {
  async cleanup_scan(_args, ctx) {
    begin('scan');
    try {
      await steps('scan', DEFS, ctx, (d) => [d[1], left.get(d[0]).files, left.get(d[0]).bytes]);
      const isAdmin = ctx.isAdmin();
      return { scannedAt: now(), isAdmin, categories: DEFS.map((d) => categoryScan(d, isAdmin)), cancelled: cancel.scan, testRoot: null };
    } finally {
      running.scan = false;
    }
  },

  async cleanup_clean({ ids, olderThanDay = true } = {}, ctx) {
    if (!Array.isArray(ids) || !ids.length) throw 'Pick at least one category.';
    const defs = [];
    for (const id of ids) {
      const d = DEFS.find((x) => x[0] === id);
      if (!d) throw 'Unknown cleanup category.';
      if (!defs.includes(d)) defs.push(d);
    }
    begin('clean');
    const isAdmin = ctx.isAdmin();
    const out = { freedBytes: 0, deletedFiles: 0, skippedInUse: 0, skippedDenied: 0, skippedOther: 0, keptNew: 0, cancelled: false, categories: [] };
    try {
      const plans = defs.map((d) => {
        const [id, label, , needsAdmin, ageFilter, , , , , browser] = d;
        const c = { id, label, freedBytes: 0, deletedFiles: 0, skippedInUse: 0, skippedDenied: 0, skippedOther: 0, keptNew: 0, skipped: null };
        const l = left.get(id);
        if (needsAdmin && !isAdmin) c.skipped = 'Needs administrator rights.';
        else if (browser && RUNNING.has(browser)) c.skipped = `${browser} is open. Close it, then clean again.`;
        else if (!MISSING.has(id)) {
          const age = ageFilter && olderThanDay;
          const files = age ? l.oldFiles : l.files;
          const bytes = age ? l.oldBytes : l.bytes;
          c.skippedInUse = Math.min(files, id === 'user-temp' ? 14 : id === 'thumbnails' ? 3 : 0);
          c.skippedDenied = id === 'user-temp' ? Math.min(files - c.skippedInUse, 2) : 0;
          c.deletedFiles = files - c.skippedInUse - c.skippedDenied;
          c.freedBytes = files ? Math.round(bytes * (c.deletedFiles / files)) : 0;
          c.keptNew = age ? l.files - l.oldFiles : 0;
        }
        return c;
      });
      await steps('clean', plans, ctx, (c) => [c.label, c.deletedFiles, c.freedBytes]);
      for (const c of plans) {
        if (cancel.clean) break;
        const l = left.get(c.id);
        l.bytes -= c.freedBytes;
        l.files -= c.deletedFiles;
        l.oldBytes = Math.max(0, l.oldBytes - c.freedBytes);
        l.oldFiles = Math.max(0, l.oldFiles - c.deletedFiles);
        for (const k of ['freedBytes', 'deletedFiles', 'skippedInUse', 'skippedDenied', 'skippedOther', 'keptNew']) out[k] += c[k];
        out.categories.push(c);
      }
      out.cancelled = cancel.clean;
      return out;
    } finally {
      running.clean = false;
    }
  },

  cleanup_cancel({ job } = {}) {
    if (!(job in cancel)) throw 'Unknown job.';
    cancel[job] = true;
    return null;
  },

  cleanup_drives() {
    return DRIVES;
  },

  async cleanup_folder_sizes({ path, fresh = false } = {}, ctx) {
    const p = String(path || '').trim().replace(/\//g, '\\');
    if (!/^[a-z]:(\\|$)/i.test(p) || /\\\.\.?(\\|$)/.test(p)) throw 'Type a full folder path, like C:\\Users.';
    const norm = /^[a-z]:\\?$/i.test(p) ? `${p.slice(0, 2).toUpperCase()}\\` : p.replace(/\\$/, '');
    const drive = norm.slice(0, 3).toUpperCase();
    if (!DRIVES.some((d) => d.mount === drive)) throw "That folder doesn't exist.";
    const parent = norm.replace(/\\[^\\]+$/, '') || norm;
    if (!fresh && TREE[parent] && norm !== drive) return { ...listing(norm), cached: true };
    begin('folders');
    try {
      const l = listing(norm);
      await steps('folders', l.folders.slice(0, 6), ctx, (f) => [f.name, f.files, f.bytes]);
      if (cancel.folders) return { ...listing(norm), folders: [], bytes: 0, files: 0, cancelled: true };
      return l;
    } finally {
      running.folders = false;
    }
  },
};
