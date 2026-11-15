// Optimize > Rules (0.9): set an app's CPU priority, processors or efficiency mode once and
// Orbit applies them every time the app starts. Rust does the work (rules.rs); this tab lists
// the rules, edits them through the shared dialog (../../rules.js), resets one (the running app
// goes back to Normal and every core, and the rule switches off), turns all rules on or off
// (the master switch) and shows a log of what Orbit did.
// Sub-tab API (see ../optimize.js): mount(paneEl, ctx), onShow(), onHide().
import { call, onEvent } from '../../api.js';
import { store } from '../../store.js';
import { escapeHtml } from '../../format.js';
import { toast, confirmDialog, restartAsAdmin } from '../../app.js';
import { openRuleDialog, ago, ruleSummary } from '../../rules.js';

let root = null;
let ctx = null;
let info = null; // rules_list
let log = [];
let icons = {}; // exe path -> data URL
let visible = false;
let timer = 0;
let loaded = false;
const els = {};

const icon = (id) => `<svg class="icon" aria-hidden="true"><use href="#${id}"/></svg>`;
const text = (s) => escapeHtml(String(s ?? ''));
const fileName = (p) => String(p || '').split(/[\\/]/).pop();

function errMsg(err) {
  return typeof err === 'string' ? err : (err && err.message) || 'Something went wrong.';
}

/** Path of a running process with this name, to fetch its icon. */
function exeFor(rule) {
  if (rule.match.kind === 'path') return rule.match.value;
  const lower = rule.match.value.toLowerCase();
  return (store.snapshot?.processes || []).find((p) => p.name.toLowerCase() === lower && p.exe)?.exe || null;
}

async function loadIcons() {
  const paths = [...new Set((info?.rules || []).map(exeFor).filter((p) => p && !icons[p]))];
  if (!paths.length) return;
  try {
    Object.assign(icons, await call('get_icons', { paths }));
    render();
  } catch {
    /* the generic icon stays */
  }
}

async function refresh() {
  try {
    [info, log] = await Promise.all([call('rules_list'), call('rules_log')]);
    loaded = true;
    ctx?.setBadge(info.rules.length || '');
    render();
    loadIcons();
  } catch (err) {
    console.error('[orbit] rules failed to load', err);
    if (!loaded) els.list.innerHTML = `<div class="empty"><strong>Rules did not load.</strong><p>${text(errMsg(err))}</p></div>`;
  }
}

function later() {
  clearTimeout(timer);
  timer = setTimeout(() => visible && refresh(), 250);
}

function lastLine(r) {
  const failed = r.lastFailed;
  const applied = r.lastApplied;
  if (failed && (!applied || failed.at >= applied)) {
    return { tone: 'warn', body: failed.reason, when: ago(failed.at), admin: failed.needsAdmin };
  }
  if (applied) return { tone: 'ok', body: 'Applied', when: ago(applied), admin: false };
  return { tone: '', body: r.boostOnly ? 'Waiting for Boost and the app.' : 'Waiting for the app to start.', when: '', admin: false };
}

function ruleHtml(r) {
  const exe = exeFor(r);
  const src = exe ? icons[exe] : null;
  const last = lastLine(r);
  const name = fileName(r.match.value);
  const where = r.match.kind === 'path' ? r.match.value : 'Any folder';
  const adminBtn = last.admin && !info.isAdmin
    ? `<button type="button" class="btn btn--ghost btn--sm" data-ru-admin>${icon('i-shield')}Restart as administrator</button>` : '';
  return `<li class="ru-rule${r.enabled ? '' : ' is-off'}" data-id="${text(r.id)}">
      <span class="ru-ico" aria-hidden="true">${src ? `<img src="${text(src)}" alt="" width="20" height="20">` : icon('i-grid')}</span>
      <div class="ru-main">
        <p class="ru-name"><span class="mono">${text(name)}</span>${r.running ? `<span class="chip chip--sm chip--live">Running${r.running > 1 ? ` ×${r.running}` : ''}</span>` : ''}</p>
        <p class="ru-where" title="${text(where)}">${text(where)}</p>
      </div>
      <p class="ru-sets">${text(ruleSummary(r))}</p>
      <div class="ru-last ru-last--${last.tone}">
        <span>${text(last.body)}${last.when ? ` <span class="lv-faint">${text(last.when)}</span>` : ''}</span>
        ${adminBtn}
      </div>
      <button type="button" class="switch" role="switch" data-ru-toggle aria-checked="${r.enabled ? 'true' : 'false'}" aria-label="Rule for ${text(name)}"><span class="switch-thumb" aria-hidden="true"></span></button>
      <button type="button" class="icon-btn" data-ru-edit aria-label="Edit rule for ${text(name)}" title="Edit">${icon('i-gear')}</button>
      <button type="button" class="icon-btn" data-ru-reset aria-label="Reset ${text(name)} to default" title="Reset to default">${icon('i-refresh')}</button>
      <button type="button" class="icon-btn" data-ru-delete aria-label="Delete rule for ${text(name)}" title="Delete">${icon('i-x')}</button>
    </li>`;
}

const LOG_ICON = { applied: 'i-check', blocked: 'i-lock', reverted: 'i-refresh', reset: 'i-history' };
const LOG_LABEL = { applied: 'Applied', blocked: 'Blocked', reverted: 'Changed back', reset: 'Put back' };

function logHtml() {
  if (!log.length) return '<p class="ru-empty">Nothing yet. Orbit writes a line here each time it applies a rule.</p>';
  return `<ul class="ru-log-list">${log.map((l) => `
      <li class="ru-entry ru-entry--${text(l.kind)}">
        <span class="ru-entry-ico" title="${text(LOG_LABEL[l.kind] || l.kind)}">${icon(LOG_ICON[l.kind] || 'i-clock')}</span>
        <span class="ru-entry-text"><span class="mono">${text(l.app)}</span> ${text(l.text)}</span>
        <span class="ru-entry-when lv-faint">${text(ago(l.at))}</span>
      </li>`).join('')}</ul>`;
}

function render() {
  if (!root || !info) return;
  const on = info.rulesEnabled !== false;
  els.master.setAttribute('aria-checked', on ? 'true' : 'false');
  els.master.disabled = false;
  els.off.hidden = on;
  root.classList.toggle('ru-all-off', !on);
  if (!info.rules.length) {
    els.list.innerHTML = `<div class="empty">${icon('i-bolt')}
      <strong>No rules yet.</strong>
      <p>Add one and Orbit sets that app's priority every time it starts.</p></div>`;
  } else {
    els.list.innerHTML = `<ul class="ru-rules">${info.rules.map(ruleHtml).join('')}</ul>`;
  }
  els.logCount.textContent = log.length ? String(log.length) : '';
  els.logBody.innerHTML = logHtml();
}

/** The stored part of a rule view (rules_update takes the rule, not the view). */
function ruleOf(r) {
  const { id, match, priority, affinity, efficiency, inFront, behind, boostOnly, enabled } = r;
  return { id, match, priority, affinity, efficiency: !!efficiency, inFront: !!inFront, behind: behind || null, boostOnly: !!boostOnly, enabled };
}

async function toggle(id) {
  const r = info.rules.find((x) => x.id === id);
  if (!r) return;
  try {
    await call('rules_update', { rule: { ...ruleOf(r), enabled: !r.enabled } });
    await refresh();
  } catch (err) {
    toast(errMsg(err), 'error');
  }
}

async function setMaster(on) {
  els.master.disabled = true;
  try {
    info = await call('rules_set_enabled', { enabled: on });
    toast(on ? 'Rules are on.' : 'Rules are off. Orbit leaves every app as it is.', 'success');
    await refresh();
  } catch (err) {
    toast(errMsg(err), 'error');
    render();
  }
}

async function reset(id) {
  const r = info.rules.find((x) => x.id === id);
  if (!r) return;
  const name = fileName(r.match.value);
  const what = [r.priority || r.efficiency ? 'Normal priority' : '', r.affinity ? 'all processors' : '', r.efficiency ? 'efficiency mode off' : ''].filter(Boolean).join(', ');
  const ok = await confirmDialog({
    title: `Reset ${name} to default?`,
    body: `A running ${name} goes back to ${what}, and this rule switches off so Orbit stops changing it. Switch the rule on again any time.`,
    confirmLabel: 'Reset',
  });
  if (!ok) return;
  try {
    const res = await call('rules_reset', { id });
    if (res.problems?.length) toast(res.problems.join(' '), 'error');
    else toast(res.reset ? `${name} is back to default. The rule is off.` : `The rule is off. ${name} isn't running, so there was nothing to put back.`, 'success');
    await refresh();
  } catch (err) {
    toast(errMsg(err), 'error');
  }
}

async function remove(id) {
  const r = info.rules.find((x) => x.id === id);
  if (!r) return;
  const name = fileName(r.match.value);
  const ok = await confirmDialog({
    title: `Delete the rule for ${name}?`,
    body: `Orbit stops changing ${name}. A running copy keeps its current priority until it closes. To put it back first, use Reset.`,
    confirmLabel: 'Delete rule',
    danger: true,
  });
  if (!ok) return;
  try {
    await call('rules_delete', { id });
    toast(`Rule for ${name} deleted.`, 'success');
    await refresh();
  } catch (err) {
    toast(errMsg(err), 'error');
  }
}

export function mount(paneEl, context) {
  root = paneEl;
  ctx = context;
  root.innerHTML = `
    <div class="toolbar ru-toolbar">
      <p class="ru-lede">Set an app's priority or processors once. Orbit applies it every time the app starts, and again if something changes it back. Windows, security and anti-cheat programs are never touched.</p>
      <div class="ru-master">
        <span class="lv-label" id="ru-master-l">Rules</span>
        <button type="button" class="switch" role="switch" data-ru-master aria-checked="true" aria-labelledby="ru-master-l" disabled><span class="switch-thumb" aria-hidden="true"></span></button>
      </div>
      <button type="button" class="btn btn--primary" data-ru-add>${icon('i-bolt')}Add rule</button>
    </div>
    <p class="ru-off" data-ru-off role="status" hidden>${icon('i-pause')}<span>Rules are off. Orbit isn't changing any app. Apps keep whatever priority they have now.</span></p>
    <section class="panel ru-panel" aria-label="Rules" data-ru-list aria-live="polite"><p class="ru-empty">Reading rules…</p></section>
    <details class="panel ru-log" data-ru-log>
      <summary><span class="micro">Log</span><span class="chip chip--sm" data-ru-log-count></span></summary>
      <div data-ru-log-body></div>
    </details>`;
  els.list = root.querySelector('[data-ru-list]');
  els.logCount = root.querySelector('[data-ru-log-count]');
  els.logBody = root.querySelector('[data-ru-log-body]');
  els.master = root.querySelector('[data-ru-master]');
  els.off = root.querySelector('[data-ru-off]');

  onEvent('orbit://rules', () => later());

  root.addEventListener('click', async (ev) => {
    const t = ev.target;
    if (t.closest('[data-ru-add]')) {
      if (await openRuleDialog()) refresh();
      return;
    }
    if (t.closest('[data-ru-admin]')) return void restartAsAdmin().then(() => refresh());
    const master = t.closest('[data-ru-master]');
    if (master) return void (!master.disabled && setMaster(master.getAttribute('aria-checked') !== 'true'));
    const li = t.closest('.ru-rule');
    if (!li) return;
    const id = li.dataset.id;
    if (t.closest('[data-ru-toggle]')) return void toggle(id);
    if (t.closest('[data-ru-delete]')) return void remove(id);
    if (t.closest('[data-ru-reset]')) return void reset(id);
    if (t.closest('[data-ru-edit]')) {
      const rule = info.rules.find((x) => x.id === id);
      if (rule && (await openRuleDialog({ rule }))) refresh();
    }
  });
  refresh();
}

export function onShow() {
  visible = true;
  refresh();
}

export function onHide() {
  visible = false;
  clearTimeout(timer);
}
