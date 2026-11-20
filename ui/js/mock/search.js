// Orbit / mock/search: file search and keyboard layouts for the browser preview (v1.4,
// docs/features/file-search.md). As administrator the first query "builds" an index for a
// moment (status "indexing" with a percent), then answers; without it the Windows Search
// route answers at once, or reports "unavailable" when the test turns it off.
//
// Test hooks (mock only): mock_search_set({ windowsSearch, admin }) (admin: true/false
// overrides the mock's admin state for this feature, null clears it) and mock_search_opened().

const FILES = [
  ['chrome.exe', 'C:\\Program Files\\Google\\Chrome\\Application', false],
  ['Chrome Apps', 'C:\\Users\\you\\AppData\\Roaming\\Microsoft\\Windows\\Start Menu\\Programs', true],
  ['client-reel.prproj', 'E:\\Work\\Edits', false],
  ['notes.txt', 'C:\\Users\\you\\Desktop', false],
  ['orbit', 'E:\\Work\\LegacyHub\\apps', true],
  ['Visual Studio Code.lnk', 'C:\\Users\\you\\Desktop', false],
  ['report-2026.docx', 'C:\\Users\\you\\Documents', false],
];

const state = {
  windowsSearch: true,
  admin: null,
  built: false,
  buildStarted: 0,
  base: 1000,
  last: [],
  opened: [],
};

const BUILD_MS = 700;

function rank(name, q) {
  const h = name.toLowerCase();
  if (h === q) return 10000;
  const i = h.indexOf(q);
  if (i === 0) return 8000 - h.length;
  if (i > 0) return (/[\s._-]/.test(h[i - 1]) ? 6000 : 4000) - i - h.length;
  const initials = name.split(/[\s._-]+/).map((w) => w[0] || '').join('').toLowerCase();
  if (initials.startsWith(q) && q.length >= 2) return 3000;
  return -1;
}

const isAdmin = (ctx) => (typeof state.admin === 'boolean' ? state.admin : ctx.isAdmin());

function status(ctx) {
  if (!isAdmin(ctx)) {
    return { route: 'windowsSearch', state: state.windowsSearch ? 'ready' : 'unavailable', drive: null, percent: null, files: 0, memoryBytes: 0 };
  }
  if (!state.built && state.buildStarted) {
    const t = Date.now() - state.buildStarted;
    if (t >= BUILD_MS) state.built = true;
    else return { route: 'index', state: 'indexing', drive: 'C:', percent: Math.min(99, Math.round((t / BUILD_MS) * 100)), files: 0, memoryBytes: 0 };
  }
  return state.built
    ? { route: 'index', state: 'ready', drive: null, percent: null, files: 1284316, memoryBytes: 41 * 1024 * 1024 }
    : { route: 'index', state: 'notBuilt', drive: null, percent: null, files: 0, memoryBytes: 0 };
}

export default {
  async file_search({ query }, ctx) {
    const q = String(query || '').trim().toLowerCase();
    if (isAdmin(ctx) && !state.buildStarted) state.buildStarted = Date.now();
    await ctx.delay(30);
    const st = status(ctx);
    if ([...q].length < 2 || st.state !== 'ready') return { query, stale: false, status: st, results: [] };
    const hits = FILES.map(([name, folder, isDir]) => ({ name, folder, isDir, s: rank(name, q) }))
      .filter((x) => x.s >= 0)
      .sort((a, b) => b.s - a.s)
      .slice(0, 50);
    state.base += 1000;
    state.last = hits.map((h) => `${h.folder}\\${h.name}`);
    return { query, stale: false, status: st, results: hits.map((h, i) => ({ id: state.base + i, name: h.name, folder: h.folder, isDir: h.isDir })) };
  },

  file_search_status(_args, ctx) {
    return status(ctx);
  },

  file_search_open({ id, reveal }) {
    const i = Number(id) - state.base;
    const path = Number.isInteger(i) && i >= 0 ? state.last[i] : undefined;
    if (!path) throw 'That result is out of date. Search again.';
    state.opened.push({ path, reveal: !!reveal });
    return null;
  },

  // US and Arabic (101): enough for "ؤاقخةث" -> "chrome".
  keyboard_layouts() {
    // Same keys in order; b is left out (it types a two-letter ligature on Arabic).
    const us = 'qwertyuiop[]asdfghjkl;\'zxcvnm,./';
    const ar = 'ضصثقفغعهخحجدشسيبلاتنمكطئءؤرىةوزظ';
    const a2u = {};
    const u2a = {};
    [...us].forEach((c, i) => {
      const d = [...ar][i];
      if (d && d !== c) {
        if (!(d in a2u)) a2u[d] = c;
        u2a[c] = d;
      }
    });
    return [
      { from: '00000409', to: '00000401', map: u2a },
      { from: '00000401', to: '00000409', map: a2u },
    ];
  },

  mock_search_set({ windowsSearch, admin }) {
    if (typeof windowsSearch === 'boolean') state.windowsSearch = windowsSearch;
    if (admin !== undefined) state.admin = typeof admin === 'boolean' ? admin : null;
    return null;
  },

  mock_search_opened() {
    return state.opened.slice();
  },
};
