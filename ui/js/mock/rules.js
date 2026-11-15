// Orbit / mock: per-app rules (src-tauri/src/rules.rs) for the browser preview.
// Same shapes and rules as Rust: path or name match, no Realtime, an affinity that fits
// the PC and keeps a core, a plain refusal for Orbit, Windows, Windows Security and
// anti-cheat names, one rule per app. Matching rules are "applied" to the running mock
// processes (a path rule beats a name rule); an unelevated Orbit gets "access denied" on
// a process of another user, which reads as the administrator message, like the real one.
// Seeded: a rule that holds (Discord), one for a game that is not running, and one the
// preview cannot apply (steamservice.exe runs as SYSTEM). The log has an applied, a
// blocked and a reverted entry.

const EVENT = 'orbit://rules';
const PRIORITIES = ['low', 'belowNormal', 'normal', 'aboveNormal', 'high'];
const LABEL = { low: 'Low', belowNormal: 'Below normal', normal: 'Normal', aboveNormal: 'Above normal', high: 'High' };
const WHY = {
  orbit: "Orbit can't change itself.",
  windows: "Windows needs this process. Orbit won't change it.",
  defender: "Windows Security protects itself. Orbit won't change it.",
  anticheat: "Anti-cheat software blocks changes like this. Orbit won't try.",
};
const NEEDS_ADMIN = 'This app runs as administrator. Restart Orbit as administrator to change it.';
const WINDOWS = new Set(['csrss.exe', 'wininit.exe', 'smss.exe', 'lsass.exe', 'lsaiso.exe', 'services.exe', 'winlogon.exe', 'svchost.exe', 'dwm.exe', 'fontdrvhost.exe', 'system', 'system idle process', 'registry', 'secure system', 'memory compression']);
const DEFENDER = new Set(['msmpeng.exe', 'nissrv.exe', 'mpdefendercoreservice.exe']);
const ANTICHEAT = new Set(['vgc.exe', 'vgtray.exe']);
const ANTICHEAT_PREFIX = ['vgk', 'easyanticheat', 'beservice', 'bedaisy', 'equ8', 'xigncode', 'gameguard', 'faceit'];

const now = () => Date.now();
const fileName = (s) => String(s || '').trim().replace(/^"|"$/g, '').split(/[\\/]/).pop().trim();
const normPath = (p) => String(p || '').trim().replace(/^"|"$/g, '').replace(/\//g, '\\').toLowerCase().replace(/^\\\\\?\\/, '');
const popcount = (mask) => [...BigInt(mask).toString(2)].filter((c) => c === '1').length;

export function refusal(value) {
  const name = fileName(value).toLowerCase();
  if (name === 'orbit.exe') return WHY.orbit;
  if (WINDOWS.has(name)) return WHY.windows;
  if (DEFENDER.has(name)) return WHY.defender;
  if (ANTICHEAT.has(name) || ANTICHEAT_PREFIX.some((p) => name.startsWith(p))) return WHY.anticheat;
  if (/[\\/]/.test(value) && normPath(value).startsWith('c:\\windows\\')) return WHY.windows;
  return null;
}

const state = {
  enabled: true,
  rules: [
    { id: 'rdisc1', match: { kind: 'name', value: 'Discord.exe' }, priority: 'belowNormal', affinity: null, enabled: true },
    { id: 'rlol01', match: { kind: 'name', value: 'League of Legends.exe' }, priority: 'high', affinity: null, enabled: true },
    { id: 'rsteam', match: { kind: 'name', value: 'steamservice.exe' }, priority: 'low', affinity: null, enabled: true },
  ],
  stats: {},
  log: [],
  next: 1,
};
const t0 = now();
state.stats.rdisc1 = { lastApplied: t0 - 9 * 60_000, lastFailed: null };
state.stats.rlol01 = { lastApplied: t0 - 3 * 3600_000, lastFailed: { at: t0 - 26 * 3600_000, reason: "Kept being changed back by something else.", needsAdmin: false } };
state.stats.rsteam = { lastApplied: null, lastFailed: { at: t0 - 9 * 60_000, reason: NEEDS_ADMIN, needsAdmin: true } };
state.log = [
  { at: t0 - 9 * 60_000, kind: 'blocked', ruleId: 'rsteam', app: 'steamservice.exe', pid: 4412, text: NEEDS_ADMIN, needsAdmin: true },
  { at: t0 - 9 * 60_000 - 2000, kind: 'applied', ruleId: 'rdisc1', app: 'Discord.exe', pid: 9120, text: 'Set to Below normal priority.', needsAdmin: false },
  { at: t0 - 26 * 3600_000, kind: 'reverted', ruleId: 'rlol01', app: 'League of Legends.exe', pid: 7340, text: 'Kept being changed back by something else. Orbit stopped for this run.', needsAdmin: false },
  { at: t0 - 26 * 3600_000 - 40_000, kind: 'reverted', ruleId: 'rlol01', app: 'League of Legends.exe', pid: 7340, text: 'The priority was changed to Low. Orbit set High priority again.', needsAdmin: false },
  { at: t0 - 26 * 3600_000 - 55_000, kind: 'applied', ruleId: 'rlol01', app: 'League of Legends.exe', pid: 7340, text: 'Set to High priority.', needsAdmin: false },
];

function summary(rule) {
  const parts = [];
  if (rule.priority) parts.push(`${LABEL[rule.priority]} priority`);
  if (rule.affinity) {
    const n = popcount(rule.affinity);
    parts.push(`${n} ${n === 1 ? 'core' : 'cores'}`);
  }
  return parts.join(', ');
}

function matches(rule, row) {
  if (!rule.enabled) return false;
  if (rule.match.kind === 'path') return !!row.exe && normPath(row.exe) === normPath(rule.match.value);
  return row.name.toLowerCase() === rule.match.value.toLowerCase();
}

function ruleFor(row) {
  if (!state.enabled || row.kind === 'windows' || row.pid <= 4 || refusal(row.name)) return null;
  return state.rules.find((r) => r.match.kind === 'path' && matches(r, row)) || state.rules.find((r) => r.match.kind === 'name' && matches(r, row)) || null;
}

/** Statuses of the running mock processes, as the real engine would hold them. */
function statuses(ctx) {
  const out = [];
  for (const row of ctx.snapshot().processes) {
    const rule = ruleFor(row);
    if (!rule) continue;
    const stat = state.stats[rule.id] || {};
    const denied = !ctx.isAdmin() && row.user !== 'Hesh';
    out.push({
      pid: row.pid,
      startTime: row.startTime,
      ruleId: rule.id,
      state: denied ? 'failed' : 'applied',
      summary: summary(rule),
      reason: denied ? NEEDS_ADMIN : null,
      needsAdmin: denied,
      at: (denied ? stat.lastFailed?.at : stat.lastApplied) || now(),
    });
  }
  return out;
}

function view(ctx, rule) {
  const stat = state.stats[rule.id] || {};
  const running = statuses(ctx).filter((s) => s.ruleId === rule.id).length;
  return { ...structuredClone(rule), lastApplied: stat.lastApplied ?? null, lastFailed: stat.lastFailed ?? null, running };
}

function logCores(ctx) {
  return ctx.snapshot().cpu?.perCore?.length || 8;
}

function validate(input, others, cores) {
  const kind = String(input?.match?.kind || '').trim();
  if (kind !== 'path' && kind !== 'name') throw 'Pick whether the rule matches a path or a name.';
  const value = String(input.match.value || '').trim().replace(/^"|"$/g, '').trim();
  if (!value) throw 'Pick an app first.';
  if (!/\.exe$/i.test(value)) throw 'Pick an .exe file.';
  if (kind === 'name' && /[\\/]/.test(value)) throw 'Use just the file name, like game.exe.';
  if (kind === 'path' && !/^([a-z]:[\\/]|\\\\)/i.test(value)) throw 'Use the full path, like C:\\Games\\game.exe.';
  const why = refusal(value);
  if (why) throw why;
  let priority = input.priority ? String(input.priority) : null;
  if (priority === 'realtime') throw 'Orbit never sets Realtime. It can freeze Windows.';
  if (priority && !PRIORITIES.includes(priority)) throw 'That priority is not valid.';
  if (!priority) priority = null;
  let affinity = input.affinity ? String(input.affinity).trim() : null;
  if (affinity) {
    if (!/^\d+$/.test(affinity)) throw 'That processor selection is not valid.';
    const m = BigInt(affinity);
    if (m === 0n) throw 'Pick at least one processor.';
    if (m >= 1n << BigInt(cores)) throw 'That selection includes processors this PC does not have.';
    affinity = m.toString();
  } else {
    affinity = null;
  }
  if (!priority && !affinity) throw 'Pick a priority or some processors to set.';
  const key = (m) => (m.kind === 'path' ? normPath(m.value) : m.value.toLowerCase());
  const rule = { id: input.id || '', match: { kind, value }, priority, affinity, enabled: input.enabled !== false };
  if (others.some((o) => o.id !== rule.id && o.match.kind === kind && key(o.match) === key(rule.match))) throw 'There is already a rule for that app.';
  return rule;
}

async function applyNow(ctx, rule) {
  const rows = ctx.snapshot().processes.filter((r) => ruleFor(r)?.id === rule.id);
  for (const row of rows) {
    const denied = !ctx.isAdmin() && row.user !== 'Hesh';
    if (denied) {
      state.stats[rule.id] = { ...(state.stats[rule.id] || {}), lastFailed: { at: now(), reason: NEEDS_ADMIN, needsAdmin: true } };
      state.log.unshift({ at: now(), kind: 'blocked', ruleId: rule.id, app: row.name, pid: row.pid, text: NEEDS_ADMIN, needsAdmin: true });
      continue;
    }
    try {
      if (rule.priority) await ctx.call('set_priority', { pid: row.pid, priority: rule.priority === 'low' ? 'idle' : rule.priority, startTime: row.startTime });
      if (rule.affinity) await ctx.call('set_affinity', { pid: row.pid, mask: rule.affinity, startTime: row.startTime });
    } catch {
      /* the preview's own guards: leave it */
    }
    state.stats[rule.id] = { ...(state.stats[rule.id] || {}), lastApplied: now() };
    state.log.unshift({ at: now(), kind: 'applied', ruleId: rule.id, app: row.name, pid: row.pid, text: `Set to ${summary(rule)}.`, needsAdmin: false });
  }
  state.log.length = Math.min(state.log.length, 200);
}

function send(ctx) {
  ctx.emit(EVENT, { statuses: statuses(ctx) });
}

const api = {
  rules_list(_args, ctx) {
    return {
      rulesEnabled: state.enabled,
      isAdmin: ctx.isAdmin(),
      logicalCores: logCores(ctx),
      rules: state.rules.map((r) => view(ctx, r)),
    };
  },

  rules_check({ value }) {
    return refusal(String(value || '')) || null;
  },

  async rules_create({ rule }, ctx) {
    if (state.rules.length >= 200) throw 'That is the most rules Orbit keeps.';
    const clean = validate(rule, state.rules, logCores(ctx));
    clean.id = `r${now().toString(16)}${state.next++}`;
    state.rules.push(clean);
    await applyNow(ctx, clean);
    send(ctx);
    return view(ctx, clean);
  },

  async rules_update({ rule }, ctx) {
    const i = state.rules.findIndex((r) => r.id === rule?.id);
    if (i < 0) throw 'That rule no longer exists.';
    const clean = validate(rule, state.rules.filter((r) => r.id !== rule.id), logCores(ctx));
    state.rules[i] = clean;
    await applyNow(ctx, clean);
    send(ctx);
    return view(ctx, clean);
  },

  rules_delete({ id }, ctx) {
    state.rules = state.rules.filter((r) => r.id !== id);
    delete state.stats[id];
    send(ctx);
    return null;
  },

  rules_set_enabled({ enabled }, ctx) {
    state.enabled = !!enabled;
    send(ctx);
    return api.rules_list({}, ctx);
  },

  rules_log() {
    return structuredClone(state.log);
  },

  rules_status(_args, ctx) {
    return statuses(ctx);
  },

  async rules_pick_exe(_args, ctx) {
    await ctx.delay(300);
    return 'C:\\Games\\Hollow Knight\\hollow_knight.exe';
  },
};

export default api;
