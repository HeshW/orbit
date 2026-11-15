// Orbit / mock: per-app rules (src-tauri/src/rules.rs) for the browser preview.
// Same shapes and rules as Rust: path or name match, no Realtime, an affinity that fits
// the PC and keeps a core, "only while in front" (Normal or Below normal behind), "only while
// Boost is on", efficiency mode (Windows 11 only: the preview is Windows 10 unless the URL
// has ?win11), a plain refusal for Orbit, Windows, Windows Security and anti-cheat names,
// one rule per app. Matching rules are "applied" to the running mock processes (a path rule
// beats a name rule); an unelevated Orbit gets "access denied" on a process of another user,
// which reads as the administrator message, like the real one. The preview has no window in
// front, so in-front rules run at their "behind" priority. Reset puts the app back to
// Normal and every core and switches the rule off; export and import merge like Rust.
// Seeded: a rule that holds (Discord), one for a game that is not running, and one the
// preview cannot apply (steamservice.exe runs as SYSTEM). The log has an applied, a
// blocked and a reverted entry.

import { boostActive } from './boost.js';

const EVENT = 'orbit://rules';
const WIN11 = typeof location !== 'undefined' && new URLSearchParams(location.search).has('win11');
const BEHIND = ['normal', 'belowNormal'];
const PRIORITIES = ['low', 'belowNormal', 'normal', 'aboveNormal', 'high'];
const LABEL = { low: 'Low', belowNormal: 'Below normal', normal: 'Normal', aboveNormal: 'Above normal', high: 'High' };
const WHY = {
  orbit: "Orbit can't change itself.",
  windows: "Windows needs this process. Orbit won't change it.",
  defender: "Windows Security protects itself. Orbit won't change it.",
  anticheat: "Anti-cheat software blocks changes like this. Orbit won't try.",
};
const NEEDS_ADMIN = 'This app runs as administrator. Restart Orbit as administrator to change it.';
const NO_ECO = 'Efficiency mode needs Windows 11.';
const WINDOWS = new Set(['csrss.exe', 'wininit.exe', 'smss.exe', 'lsass.exe', 'lsaiso.exe', 'services.exe', 'winlogon.exe', 'svchost.exe', 'dwm.exe', 'fontdrvhost.exe', 'system', 'system idle process', 'registry', 'secure system', 'memory compression']);
const DEFENDER = new Set(['msmpeng.exe', 'nissrv.exe', 'mpdefendercoreservice.exe', 'mpcmdrun.exe', 'mssense.exe', 'sensecncproxy.exe', 'securityhealthservice.exe', 'securityhealthsystray.exe']);
const ANTICHEAT = new Set(['vgc.exe', 'vgtray.exe']);
const ANTICHEAT_PREFIX = ['vgc', 'vgtray', 'vgk', 'easyanticheat', 'eac_', 'beservice', 'bedaisy', 'battleye', 'eaanticheat', 'equ8', 'xigncode', 'gameguard', 'npgg', 'faceit', 'pnkbstr', 'sguard', 'ace-', 'mhyprot'];

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
  if (/[\\/]/.test(value)) {
    const path = normPath(value);
    if (path.startsWith('c:\\windows\\')) return WHY.windows;
    if (path.includes('\\windows defender\\') || path.includes('\\windows defender advanced threat protection\\')) return WHY.defender;
  }
  return null;
}

const state = {
  enabled: true,
  rules: [
    { id: 'rdisc1', match: { kind: 'name', value: 'Discord.exe' }, priority: 'belowNormal', affinity: null, efficiency: false, inFront: false, behind: null, boostOnly: false, enabled: true },
    { id: 'rlol01', match: { kind: 'name', value: 'League of Legends.exe' }, priority: 'high', affinity: null, efficiency: false, inFront: true, behind: 'normal', boostOnly: false, enabled: true },
    { id: 'rsteam', match: { kind: 'name', value: 'steamservice.exe' }, priority: 'low', affinity: null, efficiency: false, inFront: false, behind: null, boostOnly: false, enabled: true },
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
  if (rule.efficiency) parts.push('Efficiency mode');
  if (rule.priority) {
    parts.push(rule.inFront ? `${LABEL[rule.priority]} priority in front, ${LABEL[rule.behind || 'normal']} behind` : `${LABEL[rule.priority]} priority`);
  }
  if (rule.affinity) {
    const n = popcount(rule.affinity);
    parts.push(`${n} ${n === 1 ? 'core' : 'cores'}`);
  }
  if (rule.boostOnly) parts.push('while Boost is on');
  return parts.join(', ');
}

const inForce = (rule) => rule.enabled && (!rule.boostOnly || boostActive());

/** The priority a rule sets now (no window is in front in the preview). */
const priorityNow = (rule) => (rule.efficiency ? 'low' : rule.inFront ? (rule.behind || 'normal') : rule.priority);

function matchesTarget(rule, row) {
  if (rule.match.kind === 'path') return !!row.exe && normPath(row.exe) === normPath(rule.match.value);
  return row.name.toLowerCase() === rule.match.value.toLowerCase();
}

function matches(rule, row) {
  if (!inForce(rule)) return false;
  if (rule.match.kind === 'path') return !!row.exe && normPath(row.exe) === normPath(rule.match.value);
  return row.name.toLowerCase() === rule.match.value.toLowerCase();
}

function ruleFor(row) {
  if (!state.enabled || row.kind === 'windows' || row.pid <= 4 || refusal(row.name) || (row.exe && refusal(row.exe))) return null;
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

function cleanPriority(p, allowed, msg = 'That priority is not valid.') {
  const v = p ? String(p).trim() : '';
  if (!v) return null;
  if (v === 'realtime') throw 'Orbit never sets Realtime. It can freeze Windows.';
  if (!allowed.includes(v)) throw msg;
  return v;
}

function validate(input, others, cores, eco = WIN11) {
  const kind = String(input?.match?.kind || '').trim();
  if (kind !== 'path' && kind !== 'name') throw 'Pick whether the rule matches a path or a name.';
  const value = String(input.match.value || '').trim().replace(/^"|"$/g, '').trim();
  if (!value) throw 'Pick an app first.';
  if (!/\.exe$/i.test(value)) throw 'Pick an .exe file.';
  if (kind === 'name' && /[\\/]/.test(value)) throw 'Use just the file name, like game.exe.';
  if (kind === 'path' && !/^([a-z]:[\\/]|\\\\)/i.test(value)) throw 'Use the full path, like C:\\Games\\game.exe.';
  const why = refusal(value);
  if (why) throw why;
  let priority = cleanPriority(input.priority, PRIORITIES);
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
  const efficiency = !!input.efficiency;
  const inFront = !!input.inFront;
  let behind = null;
  if (efficiency) {
    if (!eco) throw NO_ECO;
    if (inFront) throw 'Efficiency mode can\'t be used with "Only while in front".';
    if (priority && priority !== 'low') throw 'Efficiency mode runs the app at Low priority, so leave the priority as it is.';
    priority = null;
  }
  if (inFront) {
    if (!priority) throw 'Pick a priority to use while the app is in front.';
    behind = cleanPriority(input.behind, BEHIND, 'Pick Normal or Below normal for when it\'s behind.') || 'normal';
    if (behind === priority) throw 'Pick a different priority for when the app is in front.';
  }
  if (!priority && !affinity && !efficiency) throw 'Pick a priority or some processors to set.';
  const rule = { id: input.id || '', match: { kind, value }, priority, affinity, efficiency, inFront, behind, boostOnly: !!input.boostOnly, enabled: input.enabled !== false };
  if (others.some((o) => o.id !== rule.id && sameTarget(o.match, rule.match))) throw 'There is already a rule for that app.';
  return rule;
}

const keyOf = (m) => (m.kind === 'path' ? normPath(m.value) : m.value.toLowerCase());
const sameTarget = (a, b) => a.kind === b.kind && keyOf(a) === keyOf(b);
const stored = (r) => JSON.stringify({ ...r, id: '' });

/** Rust's merge(): replace a rule for the same app (keeping its id), add the rest, skip the bad ones. */
function merge(incoming, cores) {
  const out = structuredClone(state.rules);
  const report = { added: 0, replaced: 0, unchanged: 0, skipped: [], switchTo: null };
  const seen = [];
  for (const raw of Array.isArray(incoming) ? incoming : []) {
    const label = fileName(raw?.match?.value) || 'A rule';
    let clean;
    try {
      clean = validate({ ...raw, id: '' }, [], cores);
    } catch (why) {
      report.skipped.push(`${label}: ${why}`);
      continue;
    }
    if (seen.some((m) => sameTarget(m, clean.match))) {
      report.skipped.push(`${label}: That file has two rules for this app; the first was used.`);
      continue;
    }
    seen.push(clean.match);
    const i = out.findIndex((r) => sameTarget(r.match, clean.match));
    if (i >= 0) {
      if (stored(out[i]) === stored(clean)) report.unchanged++;
      else {
        out[i] = { ...clean, id: out[i].id };
        report.replaced++;
      }
    } else if (out.length >= 200) report.skipped.push(`${label}: Orbit keeps at most 200 rules.`);
    else {
      out.push({ ...clean, id: `r${now().toString(16)}${state.next++}` });
      report.added++;
    }
  }
  return { out, report };
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
    const prio = priorityNow(rule);
    try {
      if (prio) await ctx.call('set_priority', { pid: row.pid, priority: prio === 'low' ? 'idle' : prio, startTime: row.startTime });
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
      efficiencySupported: WIN11,
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

  async rules_reset({ id }, ctx) {
    const rule = state.rules.find((r) => r.id === id);
    if (!rule) throw 'That rule no longer exists.';
    let reset = 0;
    const back = [rule.priority || rule.efficiency ? 'Normal priority' : '', rule.affinity ? 'all cores' : '', rule.efficiency ? 'efficiency mode off' : ''].filter(Boolean);
    const backText = back.length > 1 ? `${back.slice(0, -1).join(', ')} and ${back[back.length - 1]}` : back[0] || '';
    for (const row of ctx.snapshot().processes.filter((r) => r.kind !== 'windows' && r.pid > 4 && matchesTarget(rule, r))) {
      try {
        if (rule.priority || rule.efficiency) await ctx.call('set_priority', { pid: row.pid, priority: 'normal', startTime: row.startTime });
        if (rule.affinity) await ctx.call('set_affinity', { pid: row.pid, mask: String((1n << BigInt(logCores(ctx))) - 1n), startTime: row.startTime });
      } catch {
        /* the preview's own guards: leave it */
      }
      reset++;
      state.log.unshift({ at: now(), kind: 'reset', ruleId: rule.id, app: row.name, pid: row.pid, text: `Put back to ${backText}. The rule is off.`, needsAdmin: false });
    }
    rule.enabled = false;
    send(ctx);
    return { reset, problems: [] };
  },

  rules_export() {
    return { rulesEnabled: state.enabled, rules: structuredClone(state.rules) };
  },

  async rules_import({ backup, apply }, ctx) {
    const list = backup?.rules;
    if (Array.isArray(list) && list.length > 400) throw 'That file has too many rules.';
    const { out, report } = merge(list, logCores(ctx));
    if (typeof backup?.rulesEnabled === 'boolean' && backup.rulesEnabled !== state.enabled) report.switchTo = backup.rulesEnabled;
    if (!apply) return report;
    state.rules = out;
    if (report.switchTo !== null) state.enabled = report.switchTo;
    for (const r of state.rules) await applyNow(ctx, r);
    send(ctx);
    return report;
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
