// Orbit / mock: game and focus boost (src-tauri/src/boost.rs) for the browser preview.
// Same shapes and rules as Rust: only listed apps outside Windows, never a listed game,
// priority lowered only from Normal / Above normal / High, everything put back on stop.
// It changes the mock process list through the built-in set_priority / suspend_process
// handlers, so the Processes view shows the change. The power plans are made up: Balanced
// is on and High performance exists. Auto mode reacts when the config is sent (the real
// one reacts to each snapshot). The file picker "picks" a made-up game.

const EVENT = 'orbit://boost';
const BALANCED = { guid: '381b4222-f694-41f0-9685-ff5bb260df2e', name: 'Balanced' };
const HIGH = { guid: '8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c', name: 'High performance' };
const LOWERABLE = new Set(['normal', 'aboveNormal', 'high']);
const now = () => Math.floor(Date.now() / 1000);
const same = (a, b) => String(a).toLowerCase() === String(b).toLowerCase();
const listed = (list, name) => list.some((n) => same(n, name));

const state = {
  config: { games: [], apps: [], auto: false, pause: false, power: true },
  power: BALANCED,
  active: null, // { startedAt, reason, trigger, powerBefore, procs: [{ pid, start, name, paused, before }] }
  skipped: [],
  suppressed: false,
  lastRestore: null,
};

function cleanList(list) {
  const seen = new Set();
  const out = [];
  for (const raw of Array.isArray(list) ? list : []) {
    const name = String(raw ?? '').trim().replace(/^"|"$/g, '').split(/[\\/]/).pop().trim();
    if (!name || seen.has(name.toLowerCase())) continue;
    seen.add(name.toLowerCase());
    out.push(name);
  }
  return out.slice(0, 64);
}

function mayTarget(r) {
  return r.kind !== 'windows' && !/^[a-z]:\\windows\\/i.test(r.exe || '') && !listed(state.config.games, r.name) && !same(r.name, 'orbit.exe') && r.pid > 4;
}

function runningGames(rows) {
  const out = new Map();
  for (const r of rows) if (listed(state.config.games, r.name)) out.set(r.name.toLowerCase(), r.name);
  return [...out.values()].sort();
}

function status(ctx) {
  const a = state.active;
  return {
    active: !!a,
    reason: a ? a.reason : null,
    trigger: a ? a.trigger : null,
    since: a ? a.startedAt : null,
    config: structuredClone(state.config),
    power: { current: state.power, target: HIGH, before: a && a.powerBefore ? a.powerBefore : null, note: null },
    changed: a ? a.procs.map((t) => ({ pid: t.pid, name: t.name, action: t.paused ? 'paused' : 'lowered', before: t.paused ? null : t.before })) : [],
    skipped: state.skipped,
    runningGames: runningGames(ctx.snapshot().processes),
    lastRestore: state.lastRestore,
    recovered: null,
  };
}

function send(ctx) {
  ctx.emit(EVENT, status(ctx));
}

async function start(ctx, reason, trigger) {
  const rows = ctx.snapshot().processes.filter((r) => listed(state.config.apps, r.name) && mayTarget(r));
  const pause = state.config.pause;
  const procs = [];
  const skipped = [];
  for (const r of rows) {
    if (r.suspended) {
      skipped.push({ pid: r.pid, name: r.name, reason: 'Already paused in Orbit.' });
      continue;
    }
    try {
      const before = await ctx.call('get_priority', { pid: r.pid });
      if (!pause && !LOWERABLE.has(before)) {
        skipped.push({ pid: r.pid, name: r.name, reason: 'Already at a low priority.' });
        continue;
      }
      if (pause) await ctx.call('suspend_process', { pid: r.pid, startTime: r.startTime });
      else await ctx.call('set_priority', { pid: r.pid, priority: 'belowNormal', startTime: r.startTime });
      procs.push({ pid: r.pid, start: r.startTime, name: r.name, paused: pause, before });
    } catch (err) {
      skipped.push({ pid: r.pid, name: r.name, reason: String(err) });
    }
  }
  let powerBefore = null;
  if (state.config.power && state.power.guid !== HIGH.guid) {
    powerBefore = state.power;
    state.power = HIGH;
  }
  state.active = { startedAt: now(), reason, trigger, powerBefore, procs };
  state.skipped = skipped;
}

async function stop(ctx) {
  const a = state.active;
  if (!a) return;
  const r = { at: now(), restored: 0, exited: 0, leftAlone: 0, failed: [], power: 'none', powerNote: null };
  for (const t of a.procs) {
    try {
      if (t.paused) {
        await ctx.call('resume_process', { pid: t.pid, startTime: t.start });
        r.restored++;
      } else if ((await ctx.call('get_priority', { pid: t.pid })) !== 'belowNormal') {
        r.leftAlone++;
      } else {
        await ctx.call('set_priority', { pid: t.pid, priority: t.before, startTime: t.start });
        r.restored++;
      }
    } catch {
      r.exited++;
    }
  }
  if (a.powerBefore) {
    state.power = a.powerBefore;
    r.power = 'restored';
  }
  state.active = null;
  state.skipped = [];
  state.lastRestore = r;
}

async function autoCheck(ctx) {
  const running = runningGames(ctx.snapshot().processes);
  if (!running.length) state.suppressed = false;
  if (state.active?.reason === 'auto' && !running.length) await stop(ctx);
  else if (state.config.auto && !state.active && !state.suppressed && running.length) await start(ctx, 'auto', running[0]);
}

export default {
  boost_status(_args, ctx) {
    return status(ctx);
  },

  async boost_configure({ config }, ctx) {
    const c = config || {};
    state.config = {
      games: cleanList(c.games),
      apps: cleanList(c.apps),
      auto: !!c.auto,
      pause: !!c.pause,
      power: c.power !== false,
    };
    await autoCheck(ctx);
    send(ctx);
    return status(ctx);
  },

  async boost_start(_args, ctx) {
    if (!state.active) await start(ctx, 'manual', null);
    send(ctx);
    return status(ctx);
  },

  async boost_stop(_args, ctx) {
    if (state.active?.reason === 'auto' && runningGames(ctx.snapshot().processes).length) state.suppressed = true;
    await stop(ctx);
    send(ctx);
    return status(ctx);
  },

  boost_suggestions(_args, ctx) {
    const groups = new Map();
    for (const r of ctx.snapshot().processes) {
      if (!mayTarget(r) || same(r.name, 'msedgewebview2.exe')) continue;
      const key = r.name.toLowerCase();
      const g = groups.get(key) || { name: r.name, exe: r.exe, cpu: 0, memory: 0, count: 0, hasWindow: false };
      g.cpu += r.cpu || 0;
      g.memory += r.memory || 0;
      g.count++;
      g.hasWindow = g.hasWindow || r.kind === 'app';
      groups.set(key, g);
    }
    const score = (s) => s.cpu + s.memory / (100 * 1024 * 1024);
    return [...groups.values()]
      .filter((s) => s.cpu >= 1 || s.memory >= 200 * 1024 * 1024)
      .map((s) => ({ ...s, cpu: Math.round(s.cpu * 10) / 10 }))
      .sort((a, b) => score(b) - score(a))
      .slice(0, 8);
  },

  async boost_pick_exe(_args, ctx) {
    await ctx.delay(300);
    return 'C:\\Games\\Hollow Knight\\hollow_knight.exe';
  },
};
