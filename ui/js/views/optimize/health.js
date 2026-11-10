// Optimize > Health (0.5, feature 7a): "is anything wrong with this PC?" One list of checks
// from get_health_report (disk health, free space, restart, Windows Update, uptime, startup
// load, security scan), each with a status, a plain sentence and a button where Orbit can
// help. Startup suggestions (7b), free space per drive and the physical disks follow.
// Windows Update is searched in the background; while it runs this tab polls
// get_update_status and reloads the report when the search is done.
import { call } from '../../api.js';
import { escapeHtml, bytes, time } from '../../format.js';
import { navigate, toast, restartAsAdmin } from '../../app.js';
import { createSuggestions } from './suggestions.js';

const STATUS = {
  problem: { label: 'Problem', chip: 'chip--danger', icon: 'i-x', rank: 3 },
  warning: { label: 'Warning', chip: 'chip--warn', icon: 'i-bolt', rank: 2 },
  unknown: { label: 'Unknown', chip: '', icon: 'i-clock', rank: 1 },
  good: { label: 'Good', chip: 'chip--ok', icon: 'i-check', rank: 0 },
};
const POLL_MS = 3000;

let root = null;
let ctx = null;
let visible = false;
let report = null;
let loading = false;
let error = null;
let pollTimer = null;
let suggest = null;
const els = {};

const icon = (id) => `<svg class="icon" aria-hidden="true"><use href="#${id}"/></svg>`;
const text = (s) => escapeHtml(String(s ?? ''));

function errMsg(err) {
  if (typeof err === 'string') return err;
  if (err && typeof err.message === 'string') return err.message;
  return 'Something went wrong.';
}

/* ---------- data ---------- */

async function load() {
  if (loading) return;
  loading = true;
  setBusy(true);
  if (!report) render();
  try {
    report = await call('get_health_report');
    error = null;
  } catch (err) {
    error = errMsg(err);
    if (report) toast(error, 'error');
  } finally {
    loading = false;
    setBusy(false);
  }
  render();
  suggest?.load();
  schedulePoll();
}

function schedulePoll() {
  clearTimeout(pollTimer);
  pollTimer = null;
  if (!visible || report?.update?.state !== 'checking') return;
  pollTimer = setTimeout(poll, POLL_MS);
}

async function poll() {
  pollTimer = null;
  if (!visible || !report) return;
  try {
    const u = await call('get_update_status', { refresh: false });
    if (u && u.state !== 'checking') {
      load();
      return;
    }
  } catch {
    /* keep the last report */
  }
  schedulePoll();
}

async function checkUpdatesAgain() {
  try {
    const u = await call('get_update_status', { refresh: true });
    if (report && u) {
      report.update = u;
      render();
      schedulePoll();
    }
  } catch (err) {
    toast(errMsg(err), 'error');
  }
}

/* ---------- actions ---------- */

async function runAction(a) {
  if (!a) return;
  if (a.kind === 'view') navigate(a.target);
  else if (a.kind === 'optimize') ctx.openTab(a.target);
  else if (a.kind === 'admin') restartAsAdmin();
  else if (a.kind === 'settings') {
    try {
      await call('open_settings_page', { page: a.target });
    } catch (err) {
      toast(errMsg(err), 'error');
    }
  }
}

/* ---------- render ---------- */

function setBusy(on) {
  if (!els.refresh) return;
  els.refresh.classList.toggle('is-busy', on);
  els.refresh.setAttribute('aria-busy', on ? 'true' : 'false');
  els.refresh.disabled = on;
}

function counts(items) {
  const c = { problem: 0, warning: 0, unknown: 0, good: 0 };
  for (const it of items) c[STATUS[it.status] ? it.status : 'unknown']++;
  return c;
}

function summaryHtml(items) {
  const c = counts(items);
  const issues = c.problem + c.warning;
  const kind = c.problem ? 'problem' : c.warning ? 'warning' : 'good';
  const title = issues === 0
    ? (c.unknown ? 'Nothing wrong so far' : 'Your PC looks healthy')
    : `${issues} ${issues === 1 ? 'thing' : 'things'} to look at`;
  const body = issues === 0
    ? (c.unknown ? `Orbit is still waiting on ${c.unknown === 1 ? 'one check' : `${c.unknown} checks`}.` : 'Every check passed.')
    : 'Start with the top of the list. Orbit only acts when you press a button.';
  const chips = ['problem', 'warning', 'good', 'unknown']
    .filter((k) => c[k])
    .map((k) => `<span class="chip lv-chip ${STATUS[k].chip}">${c[k]} ${STATUS[k].label.toLowerCase()}</span>`)
    .join('');
  return `<div class="hl-summary hl-summary--${kind}">
      <span class="hl-summary-icon">${icon(STATUS[kind].icon)}</span>
      <div class="hl-summary-text">
        <p class="hl-summary-title">${text(title)}</p>
        <p class="hl-summary-body">${text(body)}</p>
      </div>
      <div class="hl-summary-chips">${chips}</div>
    </div>`;
}

function itemHtml(it, i) {
  const s = STATUS[it.status] || STATUS.unknown;
  const checking = it.key === 'updates' && report?.update?.state === 'checking';
  const buttons = [];
  if (it.action) {
    buttons.push(`<button type="button" class="btn btn--sm${it.status === 'problem' || it.status === 'warning' ? ' btn--primary' : ' btn--ghost'}" data-hl-act="${i}">${text(it.action.label)}</button>`);
  }
  if (it.key === 'updates') {
    buttons.push(`<button type="button" class="btn btn--ghost btn--sm" data-hl-updates${checking ? ' disabled' : ''}
      title="Ask the update server now. Orbit only searches; it never downloads or installs.">${checking ? 'Checking' : 'Check again'}</button>`);
  }
  return `<li class="hl-item hl-item--${text(it.status)}">
      <span class="hl-dot" aria-hidden="true">${icon(checking ? 'i-clock' : s.icon)}</span>
      <div class="hl-item-text">
        <p class="hl-item-title">${text(it.title)} <span class="chip lv-chip ${s.chip}">${checking ? 'Checking' : s.label}</span></p>
        <p class="hl-item-line">${text(it.text)}</p>
        ${it.detail ? `<p class="hl-item-detail">${text(it.detail)}</p>` : ''}
      </div>
      ${buttons.length ? `<div class="hl-item-acts">${buttons.join('')}</div>` : ''}
    </li>`;
}

function driveHtml(d) {
  const used = Math.max(0, d.total - d.available);
  const pct = d.total ? Math.min(100, (used / d.total) * 100) : 0;
  const letter = d.mount.replace(/\\$/, '');
  const label = d.name ? `${letter} ${d.name}` : letter;
  return `<li class="hl-drive hl-drive--${text(d.status)}">
      <div class="hl-drive-head">
        <span class="hl-drive-name">${text(label)}</span>
        <span class="hl-drive-free">${text(bytes(d.available))} free of ${text(bytes(d.total))}${d.removable ? ' <span class="lv-faint">Removable</span>' : ''}</span>
      </div>
      <div class="hl-bar" role="img" aria-label="${text(`${letter} ${Math.round(pct)}% used`)}"><span class="hl-bar-fill" style="width:${pct.toFixed(1)}%"></span></div>
    </li>`;
}

function diskFacts(d) {
  const f = [];
  if (d.wearPct != null) f.push(`Wear ${d.wearPct}%`);
  if (d.temperatureC != null) f.push(`${d.temperatureC} °C`);
  if (d.powerOnHours != null) f.push(`${Math.round(d.powerOnHours / 24).toLocaleString()} days powered on`);
  const errs = (d.readErrors || 0) + (d.writeErrors || 0);
  if (d.readErrors != null || d.writeErrors != null) f.push(errs ? `${errs} uncorrected errors` : 'No read or write errors');
  return f;
}

const DISK_HEALTH = {
  healthy: ['Healthy', 'chip--ok'],
  warning: ['Warning', 'chip--warn'],
  unhealthy: ['Unhealthy', 'chip--danger'],
  unknown: ['Unknown', ''],
};

function diskHtml(d) {
  const [label, chip] = DISK_HEALTH[d.health] || DISK_HEALTH.unknown;
  const kind = [d.mediaType !== 'Unknown' ? d.mediaType : '', d.busType].filter(Boolean).join(' / ');
  const facts = diskFacts(d);
  return `<li class="hl-disk">
      <div class="hl-disk-main">
        <span class="hl-disk-name">${text(d.name)}</span>
        <span class="hl-disk-sub">${text([kind, d.size ? bytes(d.size) : '', d.drives.length ? d.drives.join(' ') : ''].filter(Boolean).join(' · '))}</span>
        ${d.problems.length ? `<span class="hl-disk-problem">${text(d.problems.join('. '))}</span>` : ''}
        ${facts.length ? `<span class="hl-disk-facts">${text(facts.join(' · '))}</span>` : ''}
      </div>
      <span class="chip lv-chip ${chip}">${label}</span>
    </li>`;
}

function render() {
  if (!root) return;
  if (!report) {
    els.body.innerHTML = error
      ? `<div class="empty">${icon('i-x')}<strong>Could not check this PC</strong><p>${text(error)}</p>
           <button type="button" class="btn btn--ghost btn--sm" data-hl-retry>Try again</button></div>`
      : `<div class="empty">${icon('i-clock')}<strong>Checking this PC</strong>
           <p>Reading disks, Windows Update and the security scan. This takes a few seconds.</p></div>`;
    els.when.textContent = '';
    ctx.setBadge('');
    return;
  }
  const items = [...report.items].sort((a, b) => (STATUS[b.status]?.rank ?? 1) - (STATUS[a.status]?.rank ?? 1));
  report.sorted = items;
  const c = counts(items);
  ctx.setBadge(c.problem + c.warning || '', c.problem ? 'danger' : 'warn');
  els.when.textContent = `Checked ${time(report.generatedAt)}`;
  const drives = report.drives || [];
  const disks = report.disks || [];
  els.body.innerHTML = `
    ${summaryHtml(items)}
    <ul class="hl-items" aria-label="Health checks">${items.map(itemHtml).join('')}</ul>
    <div data-hl-suggest></div>
    <div class="hl-grid">
      <section class="panel hl-panel" aria-labelledby="hl-drives-title">
        <h2 class="micro hl-panel-title" id="hl-drives-title">Free space</h2>
        ${drives.length ? `<ul class="hl-drives">${drives.map(driveHtml).join('')}</ul>` : '<p class="lv-help">Orbit could not read your drives.</p>'}
        <button type="button" class="btn btn--ghost btn--sm hl-panel-btn" data-hl-cleanup>Go to Cleanup ${icon('i-arrow-right')}</button>
      </section>
      <section class="panel hl-panel" aria-labelledby="hl-disks-title">
        <h2 class="micro hl-panel-title" id="hl-disks-title">Disks</h2>
        ${disks.length ? `<ul class="hl-disks">${disks.map(diskHtml).join('')}</ul>` : ''}
        ${report.diskNote ? `<p class="lv-help hl-note">${text(report.diskNote)}</p>` : ''}
      </section>
    </div>`;
  // The suggestions card keeps its own state; move its element into the fresh layout.
  els.body.querySelector('[data-hl-suggest]').replaceWith(els.suggest);
}

/* ---------- mount ---------- */

export function mount(paneEl, context) {
  root = paneEl;
  ctx = context;
  root.innerHTML = `
    <div class="toolbar hl-toolbar">
      <p class="hl-lede">A quick check of the things that most often slow a PC down or put files at risk. Read only: nothing changes until you press a button.</p>
      <span class="micro lv-count" data-hl-when aria-live="polite"></span>
      <button type="button" class="icon-btn lv-refresh" data-hl-refresh aria-label="Check again" title="Check again">${icon('i-refresh')}</button>
    </div>
    <div class="hl-body" data-hl-body></div>`;
  els.body = root.querySelector('[data-hl-body]');
  els.when = root.querySelector('[data-hl-when]');
  els.refresh = root.querySelector('[data-hl-refresh]');
  els.suggest = document.createElement('div');
  suggest = createSuggestions(els.suggest, { onChange: () => load() });

  els.refresh.addEventListener('click', () => load());
  root.addEventListener('click', (ev) => {
    const t = ev.target;
    const actBtn = t.closest('[data-hl-act]');
    if (actBtn) {
      runAction(report?.sorted?.[Number(actBtn.dataset.hlAct)]?.action);
      return;
    }
    if (t.closest('[data-hl-updates]')) checkUpdatesAgain();
    else if (t.closest('[data-hl-cleanup]')) ctx.openTab('cleanup');
    else if (t.closest('[data-hl-retry]')) load();
  });
  render();
}

export function onShow() {
  visible = true;
  if (!report && !loading) load();
  else {
    render();
    schedulePoll();
  }
}

export function onHide() {
  visible = false;
  clearTimeout(pollTimer);
  pollTimer = null;
}
