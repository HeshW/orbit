// Orbit / mock: Developer leftovers and the 7-day hold (src-tauri/src/cleanup/devleft.rs) for the
// browser preview. Same shapes and rules as Rust: the UI sends ids from the last scan, never paths;
// held items can be restored (refused when something is already at the original path) or
// deleted now. The folder picker "picks" the next folder from a fixed list.

const MB = 1024 ** 2;
const DAY = 86400;
const now = () => Math.floor(Date.now() / 1000);

const PICKS = ['E:\\Work', 'D:\\Projects'];
const PROJECTS = {
  'E:\\Work': [
    ['old-site', 'node', 612 * MB, 48211, 64],
    ['pitch-deck', 'node', 388 * MB, 30102, 41],
    ['tauri-demo', 'rust', 2.3 * 1024 * MB, 6120, 95],
  ],
  'D:\\Projects': [['game-jam', 'rust', 940 * MB, 2210, 120]],
};
const RECENT = { 'E:\\Work': 3, 'D:\\Projects': 1 };

const folders = ['E:\\Work'];
const held = [{ id: 'h1', original: 'E:\\Work\\landing-2023\\node_modules', heldAt: now() - 2 * DAY, bytes: 410 * MB }];
const gone = new Set();
let last = [];
let gen = 0;

function state() {
  return {
    folders: folders.map((path) => ({ path, problem: null })),
    held: held.map((h) => ({ ...h, expiresAt: h.heldAt + 7 * DAY })),
    holdDays: 7,
    untouchedDays: 30,
  };
}

export default {
  devleft_state() {
    return state();
  },

  devleft_add_folder() {
    const next = PICKS.find((p) => !folders.includes(p));
    if (next) folders.push(next);
    return state();
  },

  devleft_remove_folder({ path } = {}) {
    const i = folders.findIndex((f) => f.toLowerCase() === String(path || '').toLowerCase());
    if (i >= 0) folders.splice(i, 1);
    return state();
  },

  async devleft_scan(_args, ctx) {
    await ctx.delay(150);
    gen++;
    last = [];
    let recent = 0;
    for (const f of folders) {
      recent += RECENT[f] || 0;
      for (const [name, kind, bytes, files, days] of PROJECTS[f] || []) {
        const path = `${f}\\${name}\\${kind === 'node' ? 'node_modules' : 'target'}`;
        if (gone.has(path)) continue;
        last.push({ id: `${gen}-${last.length}`, name, project: `${f}\\${name}`, path, kind, bytes: Math.round(bytes), files, lastChange: now() - days * DAY });
      }
    }
    last.sort((a, b) => b.bytes - a.bytes);
    return { scannedAt: now(), projects: last, recent, folders: state().folders };
  },

  async devleft_hold({ ids } = {}, ctx) {
    if (!Array.isArray(ids) || !ids.length) throw 'Pick at least one project.';
    const picks = ids.map((id) => last.find((p) => p.id === id));
    if (picks.some((p) => !p)) throw 'That list is out of date. Scan again, then pick the projects.';
    await ctx.delay(120);
    const out = { done: 0, bytes: 0, kept: [] };
    for (const p of picks) {
      if (gone.has(p.path)) continue;
      gone.add(p.path);
      held.push({ id: `h${now()}-${held.length}`, original: p.path, heldAt: now(), bytes: p.bytes });
      out.done++;
      out.bytes += p.bytes;
    }
    last = last.filter((p) => !gone.has(p.path));
    return out;
  },

  devleft_restore({ ids } = {}) {
    const out = { done: 0, bytes: 0, kept: [] };
    for (const id of ids || []) {
      const i = held.findIndex((h) => h.id === id);
      if (i < 0) throw 'That item is no longer held. Open Cleanup again to refresh the list.';
      const h = held[i];
      if (!folders.some((f) => h.original.toLowerCase().startsWith(`${f.toLowerCase()}\\`))) {
        out.kept.push({ path: h.original, reason: 'Orbit only puts folders back inside a projects folder you added. Add it again, then restore.' });
        continue;
      }
      gone.delete(h.original);
      held.splice(i, 1);
      out.done++;
      out.bytes += h.bytes;
    }
    return out;
  },

  devleft_delete_held({ ids } = {}) {
    const out = { done: 0, bytes: 0, kept: [] };
    for (const id of ids || []) {
      const i = held.findIndex((h) => h.id === id);
      if (i < 0) throw 'That item is no longer held. Open Cleanup again to refresh the list.';
      out.done++;
      out.bytes += held[i].bytes;
      held.splice(i, 1);
    }
    return out;
  },
};
