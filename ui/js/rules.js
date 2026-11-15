// Orbit / per-app rules (0.9), the parts shared by Processes, Optimize > Rules and Boost:
// the status of each process that has a rule (for the row badge) and the dialog that
// creates or edits a rule. Rust does the work (rules.rs); the status arrives with the
// orbit://rules event, which is sent only when something changes, so reading it per row
// is a map lookup and nothing more.
import { call, onEvent } from './api.js';
import { store } from './store.js';
import { escapeHtml, time } from './format.js';
import { toast, formDialog } from './app.js';

// No Realtime: it can freeze Windows, so a rule never sets it.
export const PRIORITY_CHOICES = [
  { value: 'high', label: 'High' },
  { value: 'aboveNormal', label: 'Above normal' },
  { value: 'normal', label: 'Normal' },
  { value: 'belowNormal', label: 'Below normal' },
  { value: 'low', label: 'Low' },
];

export const priorityLabel = (p) => PRIORITY_CHOICES.find((c) => c.value === p)?.label || '';

/** What an "only while in front" rule uses while the app is behind. */
export const BEHIND_CHOICES = [
  { value: 'normal', label: 'Normal' },
  { value: 'belowNormal', label: 'Below normal' },
];

/** "High priority in front, Normal behind, 2 processors, while Boost is on". */
export function ruleSummary(r) {
  const bits = [];
  if (r.efficiency) bits.push('Efficiency mode');
  if (r.priority) {
    bits.push(r.inFront
      ? `${priorityLabel(r.priority)} priority in front, ${priorityLabel(r.behind || 'normal')} behind`
      : `${priorityLabel(r.priority)} priority`);
  }
  if (r.affinity) {
    let n = 0;
    for (let m = BigInt(r.affinity); m; m >>= 1n) n += Number(m & 1n);
    bits.push(`${n} ${n === 1 ? 'processor' : 'processors'}`);
  }
  if (r.boostOnly) bits.push('while Boost is on');
  return bits.join(', ');
}

// ------------------------------------------------------------------ status per process

const byPid = new Map();
const listeners = new Set();
let started = false;

function setStatuses(list) {
  byPid.clear();
  for (const s of Array.isArray(list) ? list : []) byPid.set(s.pid, s);
  for (const cb of listeners) cb();
}

/** Starts listening once (the Processes view calls it when it mounts). */
export function startRuleStatus() {
  if (started) return;
  started = true;
  call('rules_status').then(setStatuses).catch(() => {});
  onEvent('orbit://rules', (p) => setStatuses(p?.statuses));
}

/** Runs `cb` whenever the statuses change. Returns an unsubscribe function. */
export function onRuleStatus(cb) {
  listeners.add(cb);
  return () => listeners.delete(cb);
}

/** The rule status of a process row, or null. A reused pid never matches. */
export function ruleStatusFor(row) {
  const s = byPid.get(row.pid);
  return s && s.startTime === row.startTime ? s : null;
}

/** "just now", "4 min ago", "3 h ago", else the date. `ms` is Unix milliseconds. */
export function ago(ms) {
  if (!ms) return '';
  const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
  if (s < 10) return 'just now';
  if (s < 60) return `${s} s ago`;
  if (s < 3600) return `${Math.round(s / 60)} min ago`;
  if (s < 86400) return `${Math.round(s / 3600)} h ago`;
  return time(ms / 1000);
}

/** What the row badge says: kind of badge and its tooltip. */
export function badgeOf(s) {
  if (s.state === 'applied') return { tone: '', title: `Rule: ${s.summary}. Applied ${ago(s.at)}.` };
  if (s.state === 'gaveUp') return { tone: 'warn', title: `Rule stopped: ${s.reason}` };
  return { tone: 'warn', title: `Rule: ${s.summary}. ${s.reason}` };
}

// ------------------------------------------------------------------ the rule dialog

const fileName = (p) => String(p || '').trim().replace(/^"|"$/g, '').split(/[\\/]/).pop();
const normPath = (p) => String(p || '').trim().replace(/^"|"$/g, '').replace(/\//g, '\\').toLowerCase().replace(/^\\\\\?\\/, '');

/** The existing rule that covers a process row, if any (a path rule first). */
export function ruleForRow(rules, row) {
  const hit = (r) => r.enabled !== false && (r.match.kind === 'path'
    ? !!row.exe && normPath(row.exe) === normPath(r.match.value)
    : r.match.value.toLowerCase() === row.name.toLowerCase());
  return rules.find((r) => r.match.kind === 'path' && hit(r)) || rules.find((r) => r.match.kind === 'name' && hit(r)) || null;
}

/** The rule for an exe name (Boost's game list): a name rule, or a path rule for that file. */
export function ruleForName(rules, name) {
  const lower = String(name || '').toLowerCase();
  return rules.find((r) => r.match.kind === 'name' && r.match.value.toLowerCase() === lower)
    || rules.find((r) => r.match.kind === 'path' && fileName(r.match.value).toLowerCase() === lower) || null;
}

function runningApps() {
  const rows = store.snapshot?.processes || [];
  const seen = new Map();
  for (const r of rows) {
    if (r.kind === 'windows' || r.pid === store.staticInfo?.pid || r.pid <= 4) continue;
    const k = r.name.toLowerCase();
    const g = seen.get(k) || { name: r.name, exe: r.exe || null, app: false };
    g.exe = g.exe || r.exe || null;
    if (r.kind === 'app') g.app = true;
    seen.set(k, g);
  }
  return [...seen.values()].sort((a, b) => (b.app - a.app) || a.name.localeCompare(b.name));
}

/**
 * Create or edit a rule. `row` is a process (Processes menu), `rule` an existing rule
 * (Rules tab), `name` an exe name (Boost's game list), none of them for a new rule where
 * the app is picked in the dialog. `defaults` presets a new rule (`{ boostOnly: true }`).
 * Resolves true when a rule was saved.
 */
export async function openRuleDialog({ row = null, rule = null, name = '', defaults = {} } = {}) {
  let info;
  try {
    info = await call('rules_list');
  } catch (err) {
    toast(typeof err === 'string' ? err : 'Could not read the rules.', 'error');
    return false;
  }
  const existing = rule || (row ? ruleForRow(info.rules, row) : name ? ruleForName(info.rules, name) : null);
  const cores = Math.min(64, Math.max(1, info.logicalCores || 8));
  const eco = !!info.efficiencySupported;
  const start = existing || { priority: 'high', affinity: null, efficiency: false, inFront: false, behind: 'normal', boostOnly: false, ...defaults };
  const target = { name: '', exe: null };
  let kind = 'name';
  if (existing) {
    kind = existing.match.kind;
    target.name = fileName(existing.match.value);
    target.exe = kind === 'path' ? existing.match.value : (row?.exe || null);
  } else if (row) {
    target.name = row.name;
    target.exe = row.exe || null;
  } else if (name) {
    target.name = fileName(name);
  }
  const picking = !existing && !row && !name;
  const apps = picking ? runningApps() : [];
  const els = {};
  let token = 0;

  const valueNow = () => (kind === 'path' ? target.exe || '' : target.name);

  const done = await formDialog({
    label: 'Rules',
    title: existing ? `Rule for ${target.name}` : picking ? 'Add a rule' : `Always use this priority for ${target.name}`,
    body: 'Orbit sets this every time the app starts, and again if something changes it back.',
    okLabel: existing ? 'Save rule' : 'Add rule',
    render(fields, ctl) {
      fields.innerHTML = `
        ${picking ? `<div class="ru-pick">
          <label class="dialog__field ru-pick-field">
            <span class="dialog__field-label micro">App</span>
            <select class="select" data-ru-app aria-label="Running app"${apps.length ? '' : ' disabled'}>
              <option value="">${apps.length ? 'Pick a running app' : 'No running apps'}</option>
              ${apps.map((a) => `<option value="${escapeHtml(a.name)}">${escapeHtml(a.name)}</option>`).join('')}
            </select>
          </label>
          <button type="button" class="btn btn--ghost btn--sm" data-ru-browse>Browse</button>
        </div>` : ''}
        <p class="ru-target" data-ru-target></p>
        <div class="lv-seg ru-match" role="radiogroup" aria-label="Match">
          <button type="button" role="radio" data-ru-match="name" aria-checked="true">Any folder</button>
          <button type="button" role="radio" data-ru-match="path" aria-checked="false" tabindex="-1">This file only</button>
        </div>
        <label class="dialog__field">
          <span class="dialog__field-label micro">CPU priority</span>
          <select class="select" data-ru-prio aria-label="CPU priority">
            <option value="">Leave as it is</option>
            ${PRIORITY_CHOICES.map((p) => `<option value="${p.value}">${p.label}</option>`).join('')}
          </select>
        </label>
        <label class="check"><input type="checkbox" data-ru-front><span>Only while it's in front</span></label>
        <label class="dialog__field ru-sub" data-ru-behind-field hidden>
          <span class="dialog__field-label micro">When it's behind</span>
          <select class="select" data-ru-behind aria-label="Priority when it's behind">
            ${BEHIND_CHOICES.map((p) => `<option value="${p.value}">${p.label}</option>`).join('')}
          </select>
        </label>
        ${eco ? `<label class="check"><input type="checkbox" data-ru-eco><span>Efficiency mode <span class="lv-faint">(Low priority and less power, like Task Manager)</span></span></label>` : ''}
        <label class="check"><input type="checkbox" data-ru-limit><span>Limit which processors it can use</span></label>
        <div class="dialog__grid" role="group" aria-label="Processors" data-ru-cores hidden>
          ${Array.from({ length: cores }, (_, i) => `<label class="check"><input type="checkbox" data-ru-cpu="${i}"><span>CPU ${i}</span></label>`).join('')}
        </div>
        <label class="check"><input type="checkbox" data-ru-boost><span>Only while Boost is on</span></label>`;
      els.target = fields.querySelector('[data-ru-target]');
      els.prio = fields.querySelector('[data-ru-prio]');
      els.front = fields.querySelector('[data-ru-front]');
      els.behindField = fields.querySelector('[data-ru-behind-field]');
      els.behind = fields.querySelector('[data-ru-behind]');
      els.eco = fields.querySelector('[data-ru-eco]');
      els.limit = fields.querySelector('[data-ru-limit]');
      els.cores = fields.querySelector('[data-ru-cores]');
      els.boxes = [...fields.querySelectorAll('[data-ru-cpu]')];
      els.boost = fields.querySelector('[data-ru-boost]');
      els.match = [...fields.querySelectorAll('[data-ru-match]')];
      els.app = fields.querySelector('[data-ru-app]');

      els.prio.value = start.priority || '';
      els.front.checked = !!start.inFront;
      els.behind.value = start.behind || 'normal';
      if (els.eco) els.eco.checked = !!start.efficiency;
      els.boost.checked = !!start.boostOnly;
      if (start.affinity) {
        const m = BigInt(start.affinity);
        els.limit.checked = true;
        for (const b of els.boxes) b.checked = !!((m >> BigInt(b.dataset.ruCpu)) & 1n);
      }

      const sync = async () => {
        const mine = ++token;
        const ecoOn = !!els.eco?.checked;
        els.cores.hidden = !els.limit.checked;
        // Efficiency mode runs at Low and can't follow the window.
        els.prio.disabled = ecoOn;
        if (ecoOn) els.prio.value = '';
        els.front.disabled = ecoOn;
        if (ecoOn) els.front.checked = false;
        els.behindField.hidden = !els.front.checked;
        for (const b of els.match) {
          const on = b.dataset.ruMatch === kind;
          b.setAttribute('aria-checked', on ? 'true' : 'false');
          b.tabIndex = on ? 0 : -1;
          if (b.dataset.ruMatch === 'path') b.disabled = !target.exe;
        }
        els.target.textContent = !target.name ? 'Pick an app to start.'
          : kind === 'path' ? target.exe : `${target.name}, in any folder`;
        els.target.title = target.exe || '';
        const n = els.boxes.filter((b) => b.checked).length;
        const needCores = els.limit.checked && n === 0;
        const needPrio = els.front.checked && !els.prio.value;
        const samePrio = els.front.checked && els.prio.value === els.behind.value;
        ctl.setOk(!!target.name && !needCores && !needPrio && !samePrio && !!(els.prio.value || els.limit.checked || ecoOn));
        ctl.error(needCores ? 'Pick at least one processor.'
          : needPrio ? 'Pick a priority to use while the app is in front.'
            : samePrio ? 'Pick a different priority for when the app is in front.' : '');
        if (!target.name) return;
        let why = null;
        try {
          why = await call('rules_check', { value: valueNow() });
        } catch {
          why = null;
        }
        if (mine !== token || !why) return;
        ctl.setOk(false);
        ctl.error(why);
      };
      els.sync = sync;
      for (const b of els.match) b.addEventListener('click', () => {
        if (b.disabled) return;
        kind = b.dataset.ruMatch;
        sync();
      });
      for (const el of [els.prio, els.front, els.behind, els.eco, els.limit, els.boost, ...els.boxes]) el?.addEventListener('change', sync);
      els.app?.addEventListener('change', () => {
        const a = apps.find((x) => x.name === els.app.value);
        target.name = a ? a.name : '';
        target.exe = a ? a.exe : null;
        if (!target.exe) kind = 'name';
        sync();
      });
      fields.querySelector('[data-ru-browse]')?.addEventListener('click', async () => {
        try {
          const path = await call('rules_pick_exe');
          if (!path) return;
          target.name = fileName(path);
          target.exe = path;
          if (els.app) els.app.value = '';
          sync();
        } catch (err) {
          ctl.error(typeof err === 'string' ? err : 'Could not open the file picker.');
        }
      });
      sync();
    },
    async onSubmit() {
      let mask = 0n;
      if (els.limit.checked) for (const b of els.boxes) if (b.checked) mask |= 1n << BigInt(b.dataset.ruCpu);
      const front = els.front.checked;
      const payload = {
        id: existing?.id || '',
        match: { kind, value: valueNow() },
        priority: els.prio.value || null,
        affinity: els.limit.checked ? mask.toString() : null,
        efficiency: !!els.eco?.checked,
        inFront: front,
        behind: front ? els.behind.value : null,
        boostOnly: els.boost.checked,
        enabled: existing ? existing.enabled !== false : true,
      };
      await call(existing ? 'rules_update' : 'rules_create', { rule: payload });
      return true;
    },
  });
  if (done) toast(existing ? `Rule saved for ${target.name}.` : `Rule added. Orbit will set ${target.name} every time it starts.`, 'success');
  return done;
}
