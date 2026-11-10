// Optimize > Drivers (0.5, feature 7e): every present device with its driver (maker,
// provider, version, date, INF, signature), worst first: problem codes (the yellow "!"),
// unsigned drivers, old third-party drivers, then turned-off devices. Updates come only from
// Windows Update (search; install one after a confirm, as administrator) or the maker's own
// support page (fixed https links from drivers.rs, opened with open_url).
// Sub-tab API (see ../optimize.js): mount(paneEl, ctx), onShow(), onHide().
import { call } from '../../api.js';
import { escapeHtml, time } from '../../format.js';
import { toast, confirmDialog, restartAsAdmin } from '../../app.js';

const STATUS = {
  problem: { label: 'Problem', chip: 'chip--danger', icon: 'i-x' },
  unsigned: { label: 'Unsigned', chip: 'chip--warn', icon: 'i-shield' },
  old: { label: 'Old driver', chip: 'chip--warn', icon: 'i-clock' },
  off: { label: 'Turned off', chip: '', icon: 'i-pause' },
  ok: { label: 'OK', chip: 'chip--ok', icon: 'i-check' },
};
const RANK = ['problem', 'unsigned', 'old', 'off', 'ok'];
const FILTERS = [
  ['attention', 'Needs a look'],
  ['all', 'All devices'],
  ['problem', 'Problems'],
  ['unsigned', 'Unsigned'],
  ['old', 'Old drivers'],
  ['off', 'Turned off'],
  ['update', 'Windows Update has one'],
];
const SIGNATURE = {
  windows: 'Came with Windows',
  whql: 'Signed, tested by Microsoft (WHQL)',
  authenticode: 'Signed by the maker',
  signed: 'Signed',
  unsigned: 'Not signed',
};
const POLL_MS = 3000;

let root = null;
let ctx = null;
let visible = false;
let data = null;
let loading = false;
let error = null;
let pollTimer = null;
let installing = null;
let query = '';
let filter = 'all';
let grouped = true;
const collapsed = new Set();
const els = {};

const icon = (id) => `<svg class="icon" aria-hidden="true"><use href="#${id}"/></svg>`;
const text = (s) => escapeHtml(String(s ?? ''));
const plural = (n, one, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

function errMsg(err) {
  if (typeof err === 'string') return err;
  if (err && typeof err.message === 'string') return err.message;
  return 'Something went wrong.';
}

function dateText(secs) {
  if (!secs) return '';
  return new Date(secs * 1000).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' });
}

function ageText(secs) {
  if (!secs) return '';
  const years = (Date.now() / 1000 - secs) / (365.25 * 86400);
  if (years < 1) return 'less than a year old';
  return `${plural(Math.floor(years), 'year')} old`;
}

/* ---------- data ---------- */

async function load() {
  if (loading) return;
  loading = true;
  setBusy(true);
  if (!data) render();
  try {
    data = await call('list_drivers');
    error = null;
  } catch (err) {
    error = errMsg(err);
    if (data) toast(error, 'error');
  } finally {
    loading = false;
    setBusy(false);
  }
  render();
  schedulePoll();
}

function schedulePoll() {
  clearTimeout(pollTimer);
  pollTimer = null;
  if (!visible || data?.updates?.state !== 'checking') return;
  pollTimer = setTimeout(poll, POLL_MS);
}

async function poll() {
  pollTimer = null;
  if (!visible || !data) return;
  try {
    const u = await call('get_driver_updates', { refresh: false });
    if (u && u.state !== 'checking') {
      load();
      return;
    }
  } catch {
    /* keep the last list */
  }
  schedulePoll();
}

async function checkAgain() {
  try {
    const u = await call('get_driver_updates', { refresh: true });
    if (data && u) {
      data.updates = u;
      renderUpdates();
      schedulePoll();
    }
  } catch (err) {
    toast(errMsg(err), 'error');
  }
}

/* ---------- actions ---------- */

async function openMaker(url, name) {
  try {
    await call('open_url', { url });
  } catch (err) {
    toast(errMsg(err) || `Could not open ${name}'s page.`, 'error');
  }
}

async function install(id) {
  const offer = data?.updates?.offers?.find((o) => o.id === id);
  if (!offer || installing) return;
  if (!data.isAdmin) {
    const ok = await confirmDialog({
      title: 'Restart as administrator?',
      body: 'Installing a driver needs administrator rights. Orbit restarts as administrator; then choose the driver again.',
      confirmLabel: 'Restart as administrator',
    });
    if (ok) {
      await restartAsAdmin();
      load(); // the browser preview stays in this page; the real app restarts
    }
    return;
  }
  const forWhat = offer.deviceName ? ` for ${offer.deviceName}` : '';
  const ok = await confirmDialog({
    title: `Install ${offer.title}?`,
    body: `Windows Update downloads and installs this driver${forWhat}. The device may stop working for a moment and Windows may ask you to restart. You can roll a driver back in Device Manager.`,
    confirmLabel: 'Install',
  });
  if (!ok) return;
  installing = id;
  renderUpdates();
  try {
    const r = await call('install_driver_update', { id });
    data.updates.lastInstall = r;
    toast(`${r.title}: ${r.message}`, r.ok ? (r.restartRequired ? 'warn' : 'success') : 'error');
  } catch (err) {
    toast(errMsg(err), 'error');
  } finally {
    installing = null;
  }
  load();
}

/* ---------- render ---------- */

function setBusy(on) {
  if (!els.refresh) return;
  els.refresh.classList.toggle('is-busy', on);
  els.refresh.setAttribute('aria-busy', on ? 'true' : 'false');
  els.refresh.disabled = on;
}

function needsLook(d) {
  return d.status === 'problem' || d.status === 'unsigned' || d.status === 'old';
}

function matches(d) {
  if (filter === 'attention' && !needsLook(d)) return false;
  if (filter === 'update' && !d.updateId) return false;
  if (['problem', 'unsigned', 'old', 'off'].includes(filter) && !d.flags.includes(filter)) return false;
  if (!query) return true;
  const hay = [d.name, d.class, d.manufacturer, d.provider, d.version, d.inf, d.signer].join(' ').toLowerCase();
  return query.split(/\s+/).every((w) => hay.includes(w));
}

function summaryHtml() {
  const c = data.counts;
  const look = data.devices.filter(needsLook).length;
  const kind = c.problem ? 'problem' : look ? 'warning' : 'good';
  const title = look ? `${plural(look, 'device needs', 'devices need')} a look` : `All ${c.total} devices look fine`;
  const body = c.problem
    ? 'Devices with a problem are at the top, with the reason. Orbit only installs a driver when you choose one and confirm.'
    : look
      ? 'Nothing is broken. Old or unsigned drivers are worth checking on the maker\'s page or Windows Update.'
      : 'No device reports a problem, and every third-party driver is signed and recent.';
  const chips = [
    ['problem', c.problem, 'chip--danger', 'with a problem'],
    ['unsigned', c.unsigned, 'chip--warn', 'unsigned'],
    ['old', c.old, 'chip--warn', `older than ${data.oldYears} years`],
    ['off', c.off, '', 'turned off'],
  ].filter(([, n]) => n)
    .map(([k, n, chip, label]) => `<button type="button" class="chip lv-chip ${chip} dr-chip-btn" data-dr-filter="${k}" title="Show only these">${n} ${text(label)}</button>`)
    .join('');
  const sys = data.system || {};
  const sysName = [sys.manufacturer, sys.model].filter(Boolean).join(' ');
  const sysBtn = sys.vendor
    ? `<button type="button" class="btn btn--ghost btn--sm" data-dr-url="${text(sys.vendor.url)}" data-dr-maker="${text(sys.vendor.name)}"
         title="${text(sys.vendor.url)}">${icon('i-globe')}${text(sys.vendor.name)} support page</button>`
    : '';
  const statusIcon = { problem: 'i-x', warning: 'i-bolt', good: 'i-check' }[kind];
  return `<div class="dr-summary dr-summary--${kind}">
      <span class="dr-summary-icon">${icon(statusIcon)}</span>
      <div class="dr-summary-text">
        <p class="dr-summary-title">${text(title)}</p>
        <p class="dr-summary-body">${text(body)}</p>
        ${chips ? `<div class="dr-summary-chips">${chips}</div>` : ''}
      </div>
      ${sysName || sysBtn ? `<div class="dr-system">
        ${sysName ? `<span class="micro">This PC</span><span class="dr-system-name">${text(sysName)}</span>` : ''}
        ${sysBtn}
      </div>` : ''}
    </div>`;
}

function offerHtml(o) {
  const busy = installing === o.id || data.updates.installing === o.id;
  const sub = [o.deviceName || o.model || o.class, o.provider, o.date ? dateText(o.date) : ''].filter(Boolean).join(' · ');
  const label = busy ? 'Installing' : data.isAdmin ? 'Install' : 'Install as admin';
  const title = data.isAdmin ? 'Download and install through Windows Update' : 'Needs administrator rights: Orbit restarts as administrator first';
  return `<li class="dr-offer">
      <div class="dr-offer-text">
        <span class="dr-offer-title">${text(o.title)}${o.optional ? ' <span class="chip lv-chip">Optional</span>' : ''}</span>
        ${sub ? `<span class="dr-offer-sub">${text(sub)}</span>` : ''}
      </div>
      <button type="button" class="btn btn--sm ${busy ? 'btn--ghost' : 'btn--primary'}" data-dr-install="${text(o.id)}"
        ${busy || installing ? 'disabled' : ''} title="${text(title)}">${busy ? icon('i-clock') : data.isAdmin ? icon('i-download') : icon('i-shield')}${label}</button>
    </li>`;
}

function updatesHtml() {
  const u = data.updates || { state: 'idle', offers: [] };
  const checking = u.state === 'checking' || u.state === 'idle';
  let line;
  if (checking) line = 'Asking Windows Update which drivers it has for this PC. This can take a minute.';
  else if (u.state === 'error') line = u.error || 'Windows Update did not answer.';
  else if (!u.offers.length) line = `Windows Update has no new drivers for this PC.${u.checkedAt ? ` Checked ${time(u.checkedAt)}${u.online ? '' : ' (from Windows\' last check)'}.` : ''}`;
  else line = `Windows Update offers ${plural(u.offers.length, 'driver')} for this PC.${u.online ? '' : ' From Windows\' last check; Check again asks the server.'}`;
  const last = u.lastInstall
    ? `<p class="dr-last dr-last--${u.lastInstall.ok ? 'ok' : 'bad'}">${icon(u.lastInstall.ok ? 'i-check' : 'i-x')}<span>${text(u.lastInstall.title)}: ${text(u.lastInstall.message)}</span></p>`
    : '';
  return `<div class="dr-panel-head">
      <h2 class="micro dr-panel-title" id="dr-wu-title">Drivers from Windows Update</h2>
      <button type="button" class="btn btn--ghost btn--sm" data-dr-check ${checking || installing ? 'disabled' : ''}
        title="Ask the update server now. Searching changes nothing.">${checking ? 'Checking' : 'Check again'}</button>
    </div>
    <p class="dr-wu-line${u.state === 'error' ? ' dr-wu-line--error' : ''}">${checking ? icon('i-clock') : ''}${text(line)}</p>
    ${u.offers.length ? `<ul class="dr-offers">${u.offers.map(offerHtml).join('')}</ul>` : ''}
    ${last}
    <p class="dr-foot">Orbit gets drivers only from Windows Update or the maker's own site, never from anywhere else, and never installs one without asking.</p>`;
}

function deviceHtml(d) {
  const s = STATUS[d.status] || STATUS.ok;
  const chips = d.flags.map((f) => `<span class="chip lv-chip ${STATUS[f].chip}">${text(f === 'old' ? ageText(d.date) || STATUS.old.label : STATUS[f].label)}</span>`);
  if (d.updateId) chips.push('<span class="chip lv-chip chip--on">Windows Update has a newer one</span>');
  const facts = d.inf
    ? [d.provider, d.version ? `Version ${d.version}` : '', dateText(d.date), d.inf].filter(Boolean)
    : ['No driver installed', d.id];
  const maker = d.manufacturer && d.manufacturer !== d.provider ? `Made by ${d.manufacturer}` : '';
  const sig = d.signature ? SIGNATURE[d.signature] : '';
  const sigLine = [sig, d.signer && d.signature !== 'windows' ? d.signer : ''].filter(Boolean).join(': ');
  const buttons = [];
  if (d.updateId) {
    buttons.push(`<button type="button" class="btn btn--primary btn--sm" data-dr-install="${text(d.updateId)}"${installing ? ' disabled' : ''}
      title="${data.isAdmin ? 'Install the driver Windows Update offers' : 'Needs administrator rights: Orbit restarts as administrator first'}">${icon(data.isAdmin ? 'i-download' : 'i-shield')}Update</button>`);
  }
  if (d.vendor) {
    buttons.push(`<button type="button" class="btn btn--ghost btn--sm" data-dr-url="${text(d.vendor.url)}" data-dr-maker="${text(d.vendor.name)}"
      title="Open ${text(d.vendor.name)}'s driver page: ${text(d.vendor.url)}">${icon('i-globe')}${text(d.vendor.name)} page</button>`);
  }
  return `<li class="dr-dev dr-dev--${text(d.status)}">
      <span class="dr-dot" aria-hidden="true">${icon(s.icon)}</span>
      <div class="dr-dev-text">
        <p class="dr-dev-name"><span>${text(d.name)}</span>${chips.join('')}</p>
        ${d.problemText ? `<p class="dr-dev-problem">${text(d.problemText)}${d.problem != null ? ` <span class="lv-faint">Code ${d.problem}</span>` : ''}</p>` : ''}
        <p class="dr-dev-facts">${text(facts.join(' · '))}</p>
        ${maker || sigLine ? `<p class="dr-dev-facts lv-faint">${text([maker, sigLine].filter(Boolean).join(' · '))}</p>` : ''}
      </div>
      ${buttons.length ? `<div class="dr-dev-acts">${buttons.join('')}</div>` : ''}
    </li>`;
}

function listHtml() {
  const shown = data.devices.filter(matches);
  els.count.textContent = shown.length === data.devices.length
    ? plural(data.devices.length, 'device')
    : `${shown.length} of ${data.devices.length}`;
  if (!shown.length) {
    const why = query ? `Nothing matches "${query}".` : filter === 'attention' ? 'No device needs a look.' : 'No device in this group.';
    return `<div class="empty">${icon('i-check')}<strong>Nothing to show</strong><p>${text(why)}</p>
      ${filter !== 'all' || query ? '<button type="button" class="btn btn--ghost btn--sm" data-dr-all>Show all devices</button>' : ''}</div>`;
  }
  if (!grouped) return `<ul class="dr-devs">${shown.map(deviceHtml).join('')}</ul>`;
  // Groups in the order of their worst device (the list is already worst first).
  const groups = new Map();
  for (const d of shown) {
    if (!groups.has(d.class)) groups.set(d.class, []);
    groups.get(d.class).push(d);
  }
  return [...groups.entries()].map(([cls, list]) => {
    const worst = RANK[Math.min(...list.map((d) => RANK.indexOf(d.status)))];
    const flagged = list.filter((d) => d.status !== 'ok').length;
    const open = !collapsed.has(cls);
    return `<details class="dr-group dr-group--${worst}"${open ? ' open' : ''} data-dr-group="${text(cls)}">
        <summary class="dr-group-head">
          ${icon('i-chevron')}
          <span class="dr-group-name">${text(cls)}</span>
          <span class="lv-faint">${list.length}</span>
          ${worst !== 'ok' ? `<span class="chip lv-chip ${STATUS[worst].chip}">${text(STATUS[worst].label)}</span>` : ''}
          ${flagged ? `<span class="lv-faint dr-group-flagged">${flagged} flagged</span>` : ''}
        </summary>
        <ul class="dr-devs">${list.map(deviceHtml).join('')}</ul>
      </details>`;
  }).join('');
}

function renderUpdates() {
  if (els.wu && data) els.wu.innerHTML = updatesHtml();
}

function renderList() {
  if (els.list && data) els.list.innerHTML = listHtml();
}

function render() {
  if (!root) return;
  if (!data) {
    els.body.innerHTML = error
      ? `<div class="empty">${icon('i-x')}<strong>Could not list the drivers</strong><p>${text(error)}</p>
           <button type="button" class="btn btn--ghost btn--sm" data-dr-retry>Try again</button></div>`
      : `<div class="empty">${icon('i-clock')}<strong>Reading devices</strong><p>Listing every device and checking driver signatures. This takes a few seconds.</p></div>`;
    els.when.textContent = '';
    ctx.setBadge('');
    return;
  }
  const c = data.counts;
  const look = data.devices.filter(needsLook).length;
  ctx.setBadge(look || '', c.problem ? 'danger' : 'warn');
  els.when.textContent = `Checked ${time(data.scannedAt)}`;
  els.body.innerHTML = `
    ${summaryHtml()}
    <section class="panel dr-panel" aria-labelledby="dr-wu-title" data-dr-wu></section>
    <section class="panel dr-panel" aria-labelledby="dr-list-title">
      <div class="dr-panel-head">
        <h2 class="micro dr-panel-title" id="dr-list-title">Devices</h2>
        <span class="micro lv-count" data-dr-count></span>
      </div>
      <div class="dr-filters">
        <input type="search" class="input dr-search" data-dr-search placeholder="Search devices, makers, versions" aria-label="Search devices" spellcheck="false" autocomplete="off" value="${text(query)}">
        <select class="select lv-select" data-dr-show aria-label="Show">
          ${FILTERS.map(([v, l]) => `<option value="${v}"${v === filter ? ' selected' : ''}>${text(l)}</option>`).join('')}
        </select>
        <label class="check"><input type="checkbox" data-dr-grouped${grouped ? ' checked' : ''}><span>Group by type</span></label>
      </div>
      <div data-dr-list></div>
      ${data.hidden ? `<p class="dr-foot">${plural(data.hidden, 'hidden system device is', 'hidden system devices are')} left out, as in Device Manager. Hidden devices with a problem are always shown.</p>` : ''}
    </section>`;
  els.wu = els.body.querySelector('[data-dr-wu]');
  els.list = els.body.querySelector('[data-dr-list]');
  els.count = els.body.querySelector('[data-dr-count]');
  renderUpdates();
  renderList();
}

/* ---------- mount ---------- */

export function mount(paneEl, context) {
  root = paneEl;
  ctx = context;
  root.innerHTML = `
    <div class="toolbar dr-toolbar">
      <p class="dr-lede">Every device in this PC and its driver. Read only: a driver is installed only when you pick one from Windows Update and confirm.</p>
      <span class="micro lv-count" data-dr-when aria-live="polite"></span>
      <button type="button" class="icon-btn lv-refresh" data-dr-refresh aria-label="Read devices again" title="Read devices again">${icon('i-refresh')}</button>
    </div>
    <div class="dr-main" data-dr-main></div>`;
  els.body = root.querySelector('[data-dr-main]');
  els.when = root.querySelector('[data-dr-when]');
  els.refresh = root.querySelector('[data-dr-refresh]');

  els.refresh.addEventListener('click', () => load());
  root.addEventListener('click', (ev) => {
    const t = ev.target;
    const url = t.closest('[data-dr-url]');
    if (url) {
      openMaker(url.dataset.drUrl, url.dataset.drMaker);
      return;
    }
    const inst = t.closest('[data-dr-install]');
    if (inst) {
      install(inst.dataset.drInstall);
      return;
    }
    const chip = t.closest('[data-dr-filter]');
    if (chip) {
      filter = chip.dataset.drFilter;
      const sel = root.querySelector('[data-dr-show]');
      if (sel) sel.value = filter;
      renderList();
      return;
    }
    if (t.closest('[data-dr-check]')) checkAgain();
    else if (t.closest('[data-dr-retry]')) load();
    else if (t.closest('[data-dr-all]')) {
      filter = 'all';
      query = '';
      render();
    }
  });
  root.addEventListener('input', (ev) => {
    if (ev.target.matches('[data-dr-search]')) {
      query = ev.target.value.trim().toLowerCase();
      renderList();
    }
  });
  root.addEventListener('change', (ev) => {
    if (ev.target.matches('[data-dr-show]')) {
      filter = ev.target.value;
      renderList();
    } else if (ev.target.matches('[data-dr-grouped]')) {
      grouped = ev.target.checked;
      renderList();
    }
  });
  // <details> toggle does not bubble; capture it to remember collapsed groups.
  root.addEventListener('toggle', (ev) => {
    const g = ev.target.closest?.('[data-dr-group]');
    if (!g) return;
    if (g.open) collapsed.delete(g.dataset.drGroup);
    else collapsed.add(g.dataset.drGroup);
  }, true);
  render();
}

export function onShow() {
  visible = true;
  if (!data && !loading) load();
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
