// Optimize > Debloat (0.6, feature 8f): Analyze reads the startup apps, scheduled tasks,
// services and built-in apps on this PC and explains each one from Orbit's reviewed list
// (src-tauri/src/debloat/known.rs): Safe to turn off (with what you lose), Keep (why), or
// Unknown (leave it; Search online). Only Safe items can be ticked; Review changes shows
// exactly what will change (debloat_preview) before anything is applied. Every change is
// saved with its previous state first, and Changes lists them with Undo and Undo all.
// Sub-tab API (see ../optimize.js): mount(paneEl, ctx), onShow(), onHide().
import { call } from '../../api.js';
import { escapeHtml, time } from '../../format.js';
import { toast, confirmDialog, formDialog, restartAsAdmin, navigate } from '../../app.js';
import { enterView, countUp } from '../../motion.js';

const VERDICT = {
  off: { label: 'Safe to turn off', chip: 'chip--ok' },
  keep: { label: 'Keep', chip: 'chip--on' },
  unknown: { label: 'Unknown', chip: '' },
};
const IMPACT = { high: ['High impact', 'chip--danger'], medium: ['Medium impact', 'chip--warn'], low: ['Low impact', ''] };
const GROUP_NOTE = {
  startup: 'Apps that start when you sign in. Turned off, they stay installed and open when you start them.',
  task: 'Jobs Windows runs on a schedule. Turned off, they stay in Task Scheduler.',
  service: 'Background parts of Windows and your apps. Orbit sets them to Manual, so Windows can still start one when it is needed.',
  app: 'Apps that came with Windows. Removed for you only; they come back from the Microsoft Store.',
};
const DONE_TEXT = { startup: 'Turned off at sign-in', task: 'Task turned off', service: 'Set to Manual', app: 'Removed for you' };

let root = null;
let ctx = null;
let visible = false;
let data = null;
let changes = [];
let loading = false;
let busy = false;
let error = null;
const picked = new Set();
const open = new Set(); // "<group>:keep" / "<group>:unknown" sections the user opened
const els = {};

const icon = (id) => `<svg class="icon" aria-hidden="true"><use href="#${id}"/></svg>`;
const text = (s) => escapeHtml(String(s ?? ''));
const plural = (n, one, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

function errMsg(err) {
  if (typeof err === 'string') return err;
  if (err && typeof err.message === 'string') return err.message;
  return 'Something went wrong.';
}

/** The raw Windows name in small text, when it differs from the plain one. */
const rawHtml = (x) => (x.raw && x.raw !== x.name ? `<span class="db-raw">${text(x.raw)}</span>` : '');

const allItems = () => (data ? data.groups.flatMap((g) => g.items) : []);
const itemById = (id) => allItems().find((it) => it.id === id);

/* ---------- data ---------- */

async function loadChanges() {
  try {
    changes = await call('debloat_changes');
  } catch (err) {
    toast(errMsg(err), 'error');
  }
}

async function analyze() {
  if (loading) return;
  loading = true;
  error = null;
  setBusy(true);
  render();
  try {
    const [a] = await Promise.all([call('debloat_analyze'), loadChanges()]);
    data = a;
    // Keep ticks that are still possible.
    for (const id of [...picked]) if (!itemById(id)?.actionable) picked.delete(id);
  } catch (err) {
    error = errMsg(err);
    if (data) toast(error, 'error');
  } finally {
    loading = false;
    setBusy(false);
  }
  render();
  // Fresh results rise in (motion.js; nothing runs with Animations off).
  if (data && visible && !error) {
    enterView(els.body);
    const title = els.body.querySelector('[data-db-title]');
    if (title) countUp(title);
  }
}

/* ---------- actions ---------- */

async function searchOnline(query) {
  try {
    await call('search_online', { query });
  } catch (err) {
    toast(errMsg(err), 'error');
  }
}

function previewHtml(p) {
  const rows = p.changes.map((c) => `<li class="db-pv-row">
      <span class="db-pv-name">${text(c.name)}${rawHtml(c)}</span>
      <span class="db-pv-change">${text(c.action)}: ${text(c.from)} ${icon('i-arrow-right')} ${text(c.to)}</span>
    </li>`).join('');
  const skipped = p.refused.map((r) => `<li class="db-pv-row db-pv-row--skip">
      <span class="db-pv-name">${text(r.name)}</span><span class="db-pv-change">${text(r.reason)}</span>
    </li>`).join('');
  const apps = p.changes.some((c) => c.group === 'app');
  return `<ul class="db-pv-list" data-db-preview>${rows}</ul>
    ${skipped ? `<p class="db-pv-head">Skipped</p><ul class="db-pv-list">${skipped}</ul>` : ''}
    ${apps ? '<p class="db-pv-note">Removed apps come back from the Microsoft Store. Undo opens each one’s Store page.</p>' : ''}`;
}

async function review() {
  if (busy || !picked.size) return;
  busy = true;
  renderSummary();
  let p;
  try {
    p = await call('debloat_preview', { ids: [...picked] });
  } catch (err) {
    toast(errMsg(err), 'error');
    busy = false;
    renderSummary();
    return;
  }
  busy = false;
  renderSummary();
  const adminOnly = p.refused.filter((r) => r.needsAdmin);
  if (!p.changes.length) {
    if (adminOnly.length) {
      const ok = await confirmDialog({
        title: 'Restart as administrator?',
        body: `${plural(adminOnly.length, 'item needs', 'items need')} administrator rights. Orbit restarts as administrator; then analyze and pick them again.`,
        confirmLabel: 'Restart as administrator',
      });
      if (ok) {
        await restartAsAdmin();
        analyze(); // the browser preview stays in this page; the real app restarts
      }
    } else {
      toast(p.refused[0]?.reason || 'Nothing to change.', 'warn');
    }
    return;
  }
  const n = p.changes.length;
  const done = await formDialog({
    label: 'Debloat',
    title: `Apply ${plural(n, 'change')}?`,
    body: 'Orbit saves each change first, so you can undo it from Changes.',
    okLabel: n === 1 ? 'Apply change' : `Apply ${n} changes`,
    render(fields) {
      fields.innerHTML = previewHtml(p);
    },
    async onSubmit() {
      const r = await call('debloat_apply', { ids: p.changes.map((c) => c.id) });
      changes = r.changes;
      const parts = [];
      if (r.applied.length) parts.push(`Changed ${plural(r.applied.length, 'item')}. Undo any of them from Changes.`);
      if (r.failed.length) parts.push(`${r.failed[0].name}: ${r.failed[0].error}`);
      toast(parts.join(' ') || 'Nothing changed.', r.failed.length ? (r.applied.length ? 'warn' : 'error') : 'success');
      for (const c of r.applied) picked.delete(c.item);
      return true;
    },
  });
  if (done) analyze();
}

function undoneText(c) {
  if (c.group === 'service') return `Set ${c.name} back to ${c.from}.`;
  if (c.group === 'task') return `Turned ${c.name} back on.`;
  return `${c.name} starts at sign-in again.`;
}

async function undo(id) {
  const c = changes.find((x) => x.id === id);
  if (!c || busy) return;
  busy = true;
  renderChanges();
  try {
    const r = await call('debloat_undo', { id });
    if (r.group === 'app') toast(`Opened the Store page for ${r.name}. Install it there to bring it back.`, 'info');
    else toast(undoneText(r), 'success');
  } catch (err) {
    const msg = errMsg(err);
    if (/administrator/i.test(msg)) {
      toast(msg, 'warn', { label: 'Restart as administrator', run: () => restartAsAdmin() });
    } else {
      toast(msg, 'error');
    }
  } finally {
    busy = false;
  }
  await analyze();
}

async function undoAll() {
  const live = changes.filter((c) => c.state !== 'undone');
  if (!live.length || busy) return;
  const ok = await confirmDialog({
    title: 'Undo all changes?',
    body: 'Orbit turns every startup app and task back on and puts services back the way they were. Removed apps stay listed so you can open their Store pages one at a time.',
    confirmLabel: 'Undo all',
  });
  if (!ok) return;
  busy = true;
  renderChanges();
  try {
    const r = await call('debloat_undo_all');
    changes = r.changes;
    const parts = [];
    if (r.undone.length) parts.push(`Undid ${plural(r.undone.length, 'change')}.`);
    if (r.storeApps.length) parts.push(`${plural(r.storeApps.length, 'removed app')} can come back from the Store.`);
    if (r.failed.length) parts.push(`${r.failed[0].name}: ${r.failed[0].error}`);
    toast(parts.join(' ') || 'Nothing to undo.', r.failed.length ? 'warn' : 'success');
  } catch (err) {
    toast(errMsg(err), 'error');
  } finally {
    busy = false;
  }
  await analyze();
}

/* ---------- render ---------- */

function setBusy(on) {
  if (!els.analyze) return;
  els.analyze.classList.toggle('is-busy', on);
  els.analyze.setAttribute('aria-busy', on ? 'true' : 'false');
  els.analyze.disabled = on;
}

function summaryHtml() {
  const c = data.counts;
  const n = picked.size;
  const pickedItems = [...picked].map(itemById).filter(Boolean);
  const needAdmin = !data.isAdmin && pickedItems.some((it) => it.needsAdmin);
  const total = c.off + c.keep + c.unknown;
  const title = c.ready
    ? `${plural(c.ready, 'thing is', 'things are')} safe to turn off`
    : 'Nothing here needs turning off';
  const body = c.ready
    ? `Orbit checked ${total} items against its reviewed list. Tick what you don't need, then review the changes.`
    : `Orbit checked ${total} items against its reviewed list. What it knows is already off or worth keeping.`;
  const chips = [
    ['off', c.ready, 'chip--ok', 'safe to turn off'],
    ['keep', c.keep, 'chip--on', 'to keep'],
    ['unknown', c.unknown, '', 'not on Orbit’s list'],
  ].filter(([, v]) => v).map(([k, v, chip, label]) => `<span class="chip lv-chip ${chip}" data-db-count="${k}">${v} ${text(label)}</span>`).join('');
  const errors = data.errors?.length ? `<p class="db-errors">${icon('i-x')}${text(data.errors.join(' '))}</p>` : '';
  return `<div class="db-summary${c.ready ? '' : ' db-summary--clear'}">
      <span class="db-summary-icon">${icon(c.ready ? 'i-spark' : 'i-check')}</span>
      <div class="db-summary-text">
        <p class="db-summary-title" data-db-title>${text(title)}</p>
        <p class="db-summary-body">${text(body)}</p>
        <div class="db-summary-chips">${chips}</div>
        ${errors}
      </div>
      <div class="db-summary-acts">
        ${c.ready ? `<button type="button" class="btn btn--ghost btn--sm" data-db-all>${n === c.ready ? 'Clear ticks' : 'Tick all safe'}</button>` : ''}
        <button type="button" class="btn btn--primary" data-db-review ${n && !busy ? '' : 'disabled'}>${icon('i-check')}Review changes${n ? ` (${n})` : ''}</button>
        ${needAdmin ? `<p class="db-admin">${icon('i-shield')}Tasks and services need administrator rights. <button type="button" class="db-link" data-db-admin>Restart as administrator</button></p>` : ''}
      </div>
    </div>`;
}

function changeHtml(c) {
  const undone = c.state === 'undone';
  const label = c.group === 'app' ? 'Open Store page' : 'Undo';
  const btn = undone
    ? '<span class="chip lv-chip">Undone</span>'
    : `<button type="button" class="btn btn--ghost btn--sm" data-db-undo="${c.id}" ${busy ? 'disabled' : ''}
         title="${c.group === 'app' ? 'Reinstalling needs the Microsoft Store: this opens the app’s Store page' : 'Put it back the way it was'}">${icon(c.group === 'app' ? 'i-globe' : 'i-history')}${label}</button>`;
  return `<li class="db-change${undone ? ' db-change--undone' : ''}" data-db-change="${c.id}">
      <div class="db-change-text">
        <span class="db-change-name">${text(c.name)}${rawHtml(c)}</span>
        <span class="db-change-sub">${text(DONE_TEXT[c.group])}${c.group === 'service' ? ` (was ${text(c.from)})` : ''} · ${text(time(undone ? c.undoneAt : c.at))}${undone ? ' · undone' : ''}</span>
      </div>
      ${btn}
    </li>`;
}

function changesHtml() {
  const live = changes.filter((c) => c.state !== 'undone').length;
  return `<div class="db-panel-head">
      <h2 class="micro db-panel-title" id="db-changes-title">Changes</h2>
      <span class="micro lv-count">${live ? `${live} active` : 'All undone'}</span>
      <button type="button" class="btn btn--ghost btn--sm" data-db-undo-all ${live && !busy ? '' : 'disabled'}>${icon('i-history')}Undo all</button>
    </div>
    <ul class="db-changes">${changes.map(changeHtml).join('')}</ul>`;
}

function itemHtml(it) {
  const v = VERDICT[it.verdict];
  const tick = it.actionable
    ? `<input type="checkbox" class="db-tick" data-db-pick="${text(it.id)}" aria-label="Turn off ${text(it.name)}"${picked.has(it.id) ? ' checked' : ''}>`
    : `<span class="db-tick-gap" aria-hidden="true"></span>`;
  const chips = [`<span class="chip lv-chip ${v.chip}" data-db-verdict="${it.verdict}">${v.label}</span>`];
  if (it.verdict === 'off' && !it.on) chips.push('<span class="chip lv-chip">Already off</span>');
  if (it.impact && IMPACT[it.impact]) chips.push(`<span class="chip lv-chip ${IMPACT[it.impact][1]}">${IMPACT[it.impact][0]}</span>`);
  if (it.actionable && it.needsAdmin && !data.isAdmin) chips.push(`<span class="chip lv-chip" title="Needs administrator rights">${icon('i-shield')}Admin</span>`);
  let note = '';
  const faster = it.group === 'startup' && it.actionable && (it.impact === 'high' || it.impact === 'medium');
  if (it.verdict === 'off') note = `<p class="db-note"><span class="db-note-label">You lose</span>${text(it.note)}</p>${faster ? '<p class="db-note db-note--gain">Windows may start a little faster.</p>' : ''}`;
  else if (it.verdict === 'keep') note = `<p class="db-note"><span class="db-note-label">Why keep</span>${text(it.note)}</p>`;
  else if (it.suggested) note = '<p class="db-note db-note--hint">Orbit measured a high startup cost. If you don’t need it at sign-in, you can turn it off in Startup. <button type="button" class="db-link" data-db-startup>Open Startup</button></p>';
  else note = '<p class="db-note db-note--muted">Not on Orbit’s list. Leave it on unless you know what it is.</p>';
  const what = it.what ? `<p class="db-what">${text(it.what)}</p>` : '';
  const detail = it.detail ? `<p class="db-detail" title="${text(it.detail)}">${text(it.detail)}</p>` : '';
  const search = it.verdict === 'unknown'
    ? `<button type="button" class="btn btn--ghost btn--sm" data-db-search="${text(it.search)}" title="Search the web for ${text(it.search)}">${icon('i-search')}Search online</button>`
    : '';
  return `<li class="db-item db-item--${it.verdict}${picked.has(it.id) ? ' is-picked' : ''}" data-db-item="${text(it.id)}">
      <label class="db-item-main">
        ${tick}
        <span class="db-item-text">
          <span class="db-name">${text(it.name)}${rawHtml(it)}</span>
          ${what}
        </span>
      </label>
      <div class="db-item-side">
        <div class="db-chips">${chips.join('')}</div>
        ${note}
        ${detail}
      </div>
      ${search ? `<div class="db-item-acts">${search}</div>` : ''}
    </li>`;
}

function sectionHtml(group, kind, items) {
  if (!items.length) return '';
  const key = `${group}:${kind}`;
  const label = kind === 'keep' ? `Keep (${items.length})` : `Not on Orbit’s list (${items.length})`;
  return `<details class="db-sub" data-db-sub="${key}"${open.has(key) ? ' open' : ''}>
      <summary class="db-sub-head">${icon('i-chevron')}<span>${text(label)}</span></summary>
      <ul class="db-items">${items.map(itemHtml).join('')}</ul>
    </details>`;
}

function groupHtml(g) {
  const off = g.items.filter((it) => it.verdict === 'off');
  const keep = g.items.filter((it) => it.verdict === 'keep');
  const unknown = g.items.filter((it) => it.verdict === 'unknown');
  const c = g.counts;
  const head = [
    c.ready ? `<span class="chip lv-chip chip--ok">${c.ready} safe to turn off</span>` : '',
    off.length > c.ready ? `<span class="lv-faint">${off.length - c.ready} already off</span>` : '',
  ].join('');
  const empty = !g.items.length ? '<p class="db-empty">Nothing from this group on this PC.</p>' : '';
  return `<section class="panel db-group" data-db-group="${g.id}" aria-labelledby="db-g-${g.id}">
      <div class="db-panel-head">
        <h2 class="db-group-title" id="db-g-${g.id}">${text(g.title)}</h2>
        <span class="db-group-chips">${head}</span>
        <span class="micro lv-count">${plural(g.items.length, 'item')}</span>
      </div>
      <p class="db-group-note">${text(GROUP_NOTE[g.id])}</p>
      ${off.length ? `<ul class="db-items db-items--off">${off.map(itemHtml).join('')}</ul>` : ''}
      ${sectionHtml(g.id, 'keep', keep)}
      ${sectionHtml(g.id, 'unknown', unknown)}
      ${empty}
    </section>`;
}

function renderSummary() {
  if (els.summary && data) els.summary.innerHTML = summaryHtml();
}

function renderChanges() {
  if (!els.changes) return;
  els.changes.hidden = !changes.length;
  els.changes.innerHTML = changes.length ? changesHtml() : '';
}

function render() {
  if (!root) return;
  els.analyze.innerHTML = `${icon(data ? 'i-refresh' : 'i-radar')}${data ? 'Analyze again' : 'Analyze'}`;
  if (!data) {
    els.when.textContent = '';
    ctx.setBadge('');
    els.body.innerHTML = loading
      ? `<div class="empty">${icon('i-clock')}<strong>Analyzing this PC</strong><p>Reading startup apps, scheduled tasks, services and built-in apps. Nothing changes.</p></div>`
      : error
        ? `<div class="empty">${icon('i-x')}<strong>Could not analyze this PC</strong><p>${text(error)}</p>
             <button type="button" class="btn btn--ghost btn--sm" data-db-retry>Try again</button></div>`
        : `<div class="empty db-intro">${icon('i-radar')}<strong>See what starts with Windows, and what you can do without</strong>
             <p>Orbit reads this PC and explains each item in plain words, from a list it ships with. Nothing changes until you pick items and confirm.</p>
             <button type="button" class="btn btn--primary" data-db-start>${icon('i-radar')}Analyze this PC</button></div>`;
    els.body.insertAdjacentHTML('beforeend', '<section class="panel db-panel" data-db-changes aria-labelledby="db-changes-title" hidden></section>');
    els.changes = els.body.querySelector('[data-db-changes]');
    renderChanges();
    return;
  }
  ctx.setBadge(data.counts.ready || '');
  els.when.textContent = `Analyzed ${time(data.readAt)}`;
  els.body.innerHTML = `
    <div data-db-summary></div>
    <section class="panel db-panel" data-db-changes aria-labelledby="db-changes-title" hidden></section>
    ${data.groups.map(groupHtml).join('')}`;
  els.summary = els.body.querySelector('[data-db-summary]');
  els.changes = els.body.querySelector('[data-db-changes]');
  renderSummary();
  renderChanges();
}

/* ---------- mount ---------- */

export function mount(paneEl, context) {
  root = paneEl;
  ctx = context;
  root.innerHTML = `
    <div class="toolbar db-toolbar">
      <p class="db-lede">Startup apps, scheduled tasks, services and built-in apps, explained. Only items Orbit knows are safe can be turned off, and every change can be undone.</p>
      <span class="micro lv-count" data-db-when aria-live="polite"></span>
      <button type="button" class="btn btn--ghost btn--sm" data-db-analyze></button>
    </div>
    <div class="db-main" data-db-main></div>`;
  els.body = root.querySelector('[data-db-main]');
  els.when = root.querySelector('[data-db-when]');
  els.analyze = root.querySelector('[data-db-analyze]');

  els.analyze.addEventListener('click', () => analyze());
  root.addEventListener('click', (ev) => {
    const t = ev.target;
    const s = t.closest('[data-db-search]');
    if (s) {
      ev.preventDefault();
      searchOnline(s.dataset.dbSearch);
      return;
    }
    const u = t.closest('[data-db-undo]');
    if (u) {
      undo(Number(u.dataset.dbUndo));
      return;
    }
    if (t.closest('[data-db-start]') || t.closest('[data-db-retry]')) analyze();
    else if (t.closest('[data-db-review]')) review();
    else if (t.closest('[data-db-undo-all]')) undoAll();
    else if (t.closest('[data-db-admin]')) restartAsAdmin().then(() => analyze());
    else if (t.closest('[data-db-startup]')) {
      ev.preventDefault();
      navigate('startup');
    } else if (t.closest('[data-db-all]')) {
      const ready = allItems().filter((it) => it.actionable);
      if (picked.size === ready.length) picked.clear();
      else for (const it of ready) picked.add(it.id);
      render();
    }
  });
  root.addEventListener('change', (ev) => {
    const box = ev.target.closest('[data-db-pick]');
    if (!box) return;
    if (box.checked) picked.add(box.dataset.dbPick);
    else picked.delete(box.dataset.dbPick);
    box.closest('.db-item')?.classList.toggle('is-picked', box.checked);
    renderSummary();
  });
  root.addEventListener('toggle', (ev) => {
    const d = ev.target.closest?.('[data-db-sub]');
    if (!d) return;
    if (d.open) open.add(d.dataset.dbSub);
    else open.delete(d.dataset.dbSub);
  }, true);
  render();
}

export async function onShow() {
  visible = true;
  if (!data && !loading && !changes.length) {
    await loadChanges();
    if (visible && !data) render();
  }
}

export function onHide() {
  visible = false;
}
