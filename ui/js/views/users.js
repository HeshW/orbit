// 05 / Users: everyone signed in to this PC, with the CPU, memory and I/O of each session.
//
// Sessions come from `list_sessions` (no admin needed) and refresh every 5 s while the view
// is visible. The numbers are sums over `store.snapshot.processes` by `sessionId`, updated
// every snapshot in place (the rows are only rebuilt when the list itself changes, and never
// while a pointer is down on them, so clicks are never lost). A row expands to show that
// session's busiest processes; a click opens Details. Actions: Send message, Disconnect and
// Sign out (danger confirm). Windows allows them on other users' sessions only for admins.

import { call } from '../api.js';
import { store, on, selectProcess } from '../store.js';
import { bytes, rate, pct, time, duration, heat, escapeHtml, fuzzy, IO_TITLE } from '../format.js';
import { toast, navigate, confirmDialog, formDialog, showMenu, restartAsAdmin } from '../app.js';
import { registerCommands } from '../palette.js';

const REFRESH_MS = 5000;
const TOP_N = 8;
const MAX_MESSAGE = 1000;
const MEM_TITLE = "Sum of the working sets of the session's processes. Shared memory counts once per process.";

const STATES = {
  active: { label: 'Active', cls: 'chip--ok' },
  connected: { label: 'Connected', cls: 'usr-state--conn' },
  disconnected: { label: 'Disconnected', cls: 'lv-chip--muted' },
  idle: { label: 'Idle', cls: 'lv-chip--muted' },
  listen: { label: 'Listening', cls: 'lv-chip--muted' },
  other: { label: 'Other', cls: 'lv-chip--muted' },
};

let root = null;
const els = {};
let visible = false;
let sessions = [];
let loaded = false;
let loading = false;
let loadError = null;
let errorToasted = false;
let timer = 0;
let sort = { key: 'default', dir: 1 };
const expanded = new Set();
let expandedInit = false;
let lastSig = '';
let pointerDown = false;
let pendingRender = false;
let shown = []; // sessions in the table, in order
const icons = new Map(); // exe -> data URL, '' while asked or when Windows has none
let unregister = null;

const icon = (id) => `<svg class="icon" aria-hidden="true"><use href="#${id}"/></svg>`;
const text = (s) => escapeHtml(s == null ? '' : String(s));

function errText(err) {
  if (typeof err === 'string' && err) return err;
  if (err && typeof err.message === 'string' && err.message) return err.message;
  return 'Something went wrong. Try again.';
}

function isAdmin() {
  return !!(store.staticInfo && store.staticInfo.isAdmin);
}

function fullName(s) {
  return s.domain ? `${s.domain}\\${s.userName}` : s.userName;
}

// ------------------------------------------------------------------ usage per session

function usage() {
  const snap = store.snapshot;
  const rows = snap && Array.isArray(snap.processes) ? snap.processes : [];
  const map = new Map();
  for (const r of rows) {
    if (r.sessionId == null) continue;
    let u = map.get(r.sessionId);
    if (!u) {
      u = { cpu: 0, memory: 0, io: 0, count: 0, procs: [] };
      map.set(r.sessionId, u);
    }
    u.cpu += Number(r.cpu) || 0;
    u.memory += Number(r.memory) || 0;
    u.io += (Number(r.diskReadBps) || 0) + (Number(r.diskWriteBps) || 0);
    u.count++;
    u.procs.push(r);
  }
  return map;
}

const EMPTY_USE = { cpu: 0, memory: 0, io: 0, count: 0, procs: [] };

function topProcs(u) {
  // Rounded CPU keeps the order from jumping on tiny changes; memory breaks ties.
  return [...u.procs]
    .sort((a, b) => Math.round((b.cpu || 0) * 10) - Math.round((a.cpu || 0) * 10) || (b.memory || 0) - (a.memory || 0) || a.pid - b.pid)
    .slice(0, TOP_N);
}

function memHeatMax() {
  const total = store.staticInfo && store.staticInfo.totalMemory;
  return total ? total * 0.5 : 8 * 1024 ** 3;
}

function heats(u) {
  return {
    cpu: Math.round(heat(u.cpu, 60) * 40) / 40,
    memory: Math.round(heat(u.memory, memHeatMax()) * 40) / 40,
    io: Math.round(heat(u.io, 25 * 1024 * 1024) * 40) / 40,
  };
}

// ------------------------------------------------------------------ data

async function refresh() {
  if (loading || document.hidden) return;
  loading = true;
  if (els.refresh) els.refresh.classList.add('is-busy');
  try {
    const res = await call('list_sessions');
    sessions = Array.isArray(res) ? res : [];
    loaded = true;
    loadError = null;
    errorToasted = false;
    if (!expandedInit) {
      // Start with your own session open, as a first look at what it runs.
      expandedInit = true;
      const me = sessions.find((s) => s.isCurrent);
      if (me) expanded.add(me.sessionId);
    }
    for (const id of [...expanded]) if (!sessions.some((s) => s.sessionId === id)) expanded.delete(id);
  } catch (err) {
    loadError = errText(err);
    if (loaded && !errorToasted && isVisible()) {
      errorToasted = true;
      toast(`Could not refresh users. ${loadError}`, 'error');
    }
  }
  loading = false;
  if (els.refresh) els.refresh.classList.remove('is-busy');
  if (isVisible()) render();
}

function startTimer() {
  stopTimer();
  timer = setInterval(() => { if (isVisible()) refresh(); }, REFRESH_MS);
}

function stopTimer() {
  if (timer) clearInterval(timer);
  timer = 0;
}

async function loadIcons(paths) {
  const want = paths.filter((p) => p && !icons.has(p));
  if (!want.length) return;
  for (const p of want) icons.set(p, '');
  try {
    const map = (await call('get_icons', { paths: want })) || {};
    let got = false;
    for (const p of want) {
      if (typeof map[p] === 'string' && map[p].startsWith('data:image/')) {
        icons.set(p, map[p]);
        got = true;
      }
    }
    if (got) {
      lastSig = '';
      if (isVisible()) render();
    }
  } catch (_) { /* keep the blank tiles */ }
}

function iconCell(exe) {
  const src = exe ? icons.get(exe) : '';
  if (src) return `<img class="lv-ico" src="${src}" alt="" width="18" height="18" decoding="async">`;
  return `<span class="lv-ico lv-ico--blank" aria-hidden="true">${icon('i-grid')}</span>`;
}

// ------------------------------------------------------------------ filtering and sorting

function visibleSessions(use) {
  const q = (store.query || '').trim();
  const list = sessions.filter((s) => {
    if (!q) return true;
    const st = (STATES[s.state] || STATES.other).label;
    return fuzzy(`${fullName(s)} ${s.clientName || ''} ${st} session ${s.sessionId}`, q) >= 1000;
  });
  const u = (s) => use.get(s.sessionId) || EMPTY_USE;
  const d = sort.dir;
  list.sort((a, b) => {
    let r = 0;
    switch (sort.key) {
      case 'user': r = a.userName.localeCompare(b.userName); break;
      case 'session': r = a.sessionId - b.sessionId; break;
      case 'state': r = (STATES[a.state] || STATES.other).label.localeCompare((STATES[b.state] || STATES.other).label); break;
      case 'logon': r = (a.logonTime || 0) - (b.logonTime || 0); break;
      case 'procs': r = u(a).count - u(b).count; break;
      case 'cpu': r = u(a).cpu - u(b).cpu; break;
      case 'memory': r = u(a).memory - u(b).memory; break;
      case 'io': r = u(a).io - u(b).io; break;
      default: return (b.isCurrent - a.isCurrent) || a.sessionId - b.sessionId;
    }
    return (r || a.sessionId - b.sessionId) * d;
  });
  return list;
}

// ------------------------------------------------------------------ render

function stateChip(s) {
  const st = STATES[s.state] || STATES.other;
  return `<span class="chip lv-chip ${st.cls}">${text(st.label)}</span>`;
}

function logonCell(s) {
  if (!s.logonTime) return '<span class="lv-faint">-</span>';
  const ago = Math.max(0, Math.floor(Date.now() / 1000) - s.logonTime);
  return `<span class="lv-num" title="Signed in ${text(duration(ago))} ago">${text(time(s.logonTime))}</span>`;
}

function numCells(u) {
  const h = heats(u);
  return `
    <td class="c-procs cell-num"><span class="lv-num" data-u="count">${u.count}</span></td>
    <td class="c-cpu cell-num heat" data-h="cpu" style="--h:${h.cpu}"><span class="lv-num" data-u="cpu">${pct(u.cpu)}</span></td>
    <td class="c-mem cell-num heat" data-h="memory" style="--h:${h.memory}"><span class="lv-num" data-u="memory">${bytes(u.memory)}</span></td>
    <td class="c-io cell-num heat" data-h="io" style="--h:${h.io}"><span class="lv-num" data-u="io">${rate(u.io)}</span></td>`;
}

function rowHtml(s, i, u) {
  const open = expanded.has(s.sessionId);
  const name = s.userName;
  const initial = (name.trim()[0] || '?').toUpperCase();
  return `<tr class="row usr-row${open ? ' is-open' : ''}${s.isCurrent ? ' is-current' : ''}" data-i="${i}" data-sid="${s.sessionId}">
    <td class="c-user"><div class="lv-namecell">
      <button type="button" class="icon-btn lv-more usr-toggle" data-act="toggle" aria-expanded="${open}" aria-controls="usr-detail-${s.sessionId}" aria-label="${open ? 'Hide' : 'Show'} processes of ${text(name)}" title="${open ? 'Hide processes' : 'Show processes'}">${icon('i-chevron')}</button>
      <span class="usr-avatar" aria-hidden="true">${text(initial)}</span>
      <span class="lv-primary" title="${text(fullName(s))}">${text(name)}</span>
      ${s.isCurrent ? '<span class="chip lv-chip usr-you">You</span>' : ''}
    </div></td>
    <td class="c-session cell-num"><span class="lv-num">${s.sessionId}</span></td>
    <td class="c-state">${stateChip(s)}</td>
    <td class="c-client">${s.clientName ? `<span class="lv-desc" title="Remote client">${text(s.clientName)}</span>` : '<span class="lv-faint">This PC</span>'}</td>
    <td class="c-logon">${logonCell(s)}</td>
    ${numCells(u)}
    <td class="c-actions"><button type="button" class="icon-btn lv-more" data-act="menu" aria-label="Actions for ${text(name)}" title="Actions">${icon('i-more')}</button></td>
  </tr>${open ? detailHtml(s, u) : ''}`;
}

function procItem(r) {
  return `<li><button type="button" class="usr-proc" data-pid="${r.pid}" title="Details for ${text(r.name)}">
    ${iconCell(r.exe)}
    <span class="usr-proc-name">${text(r.name)}</span>
    <span class="usr-proc-pid lv-num lv-faint">${r.pid}</span>
    <span class="usr-proc-num lv-num" data-p="cpu">${pct(r.cpu)}</span>
    <span class="usr-proc-num lv-num" data-p="memory">${bytes(r.memory)}</span>
    <span class="usr-proc-num lv-num" data-p="io">${rate((r.diskReadBps || 0) + (r.diskWriteBps || 0))}</span>
  </button></li>`;
}

/** Columns the narrow-window rules leave on screen: a wider colspan would add phantom columns. */
function visibleCols() {
  if (!els.table) return 10;
  let n = 0;
  for (const th of els.table.querySelectorAll('thead th')) if (getComputedStyle(th).display !== 'none') n++;
  return n || 10;
}

function detailHtml(s, u) {
  const top = topProcs(u);
  const more = u.count - top.length;
  const needsAdmin = !s.isCurrent && !isAdmin();
  const list = top.length
    ? `<div class="usr-procs-head micro" aria-hidden="true"><span></span><span>Process</span><span>PID</span><span>CPU</span><span>Memory</span><span title="${text(IO_TITLE)}">I/O</span></div>
       <ol class="usr-procs" aria-label="Busiest processes of ${text(s.userName)}">${top.map(procItem).join('')}</ol>
       ${more > 0 ? `<p class="usr-more lv-faint">And ${more} more process${more === 1 ? '' : 'es'}.</p>` : ''}`
    : `<p class="usr-more lv-faint">No processes in this session right now.</p>`;
  return `<tr class="usr-detail" id="usr-detail-${s.sessionId}" data-sid="${s.sessionId}"><td colspan="${visibleCols()}">
    <div class="usr-detail-inner">
      <section class="usr-detail-procs">
        <p class="micro micro--bright usr-detail-label">Busiest processes</p>
        ${list}
      </section>
      <section class="usr-detail-acts" aria-label="Session actions">
        <p class="micro usr-detail-label">${[`Session ${s.sessionId}`, s.clientName ? text(s.clientName) : null, s.logonTime ? `Signed in ${text(time(s.logonTime))}` : null].filter(Boolean).join(' / ')}</p>
        <div class="usr-btns">
          <button type="button" class="btn btn--ghost btn--sm" data-act="message">Send message</button>
          <button type="button" class="btn btn--ghost btn--sm" data-act="disconnect"${s.state === 'disconnected' ? ' disabled title="Already disconnected"' : ''}>Disconnect</button>
          <button type="button" class="btn btn--danger btn--sm" data-act="signout">Sign out</button>
        </div>
        ${needsAdmin ? `<p class="usr-admin-note">Windows lets only administrators change other users' sessions. <button type="button" class="lv-link" data-act="admin">Try Restart as administrator.</button></p>` : ''}
      </section>
    </div>
  </td></tr>`;
}

function renderHead(use) {
  const done = loaded;
  let active = 0;
  let disc = 0;
  let procs = 0;
  for (const s of sessions) {
    if (s.state === 'active') active++;
    if (s.state === 'disconnected') disc++;
    procs += (use.get(s.sessionId) || EMPTY_USE).count;
  }
  els.statUsers.textContent = done ? String(sessions.length) : '-';
  els.statActive.textContent = done ? String(active) : '-';
  els.statDisc.textContent = done ? String(disc) : '-';
  els.statProcs.textContent = done ? String(procs) : '-';
  for (const th of els.table.querySelectorAll('th[data-col]')) {
    const on = th.dataset.col === sort.key;
    th.classList.toggle('is-sorted-asc', on && sort.dir === 1);
    th.classList.toggle('is-sorted-desc', on && sort.dir === -1);
    th.setAttribute('aria-sort', on ? (sort.dir === 1 ? 'ascending' : 'descending') : 'none');
  }
}

function showEmpty(title, body, action) {
  els.table.classList.add('is-empty');
  els.empty.hidden = false;
  els.empty.innerHTML = `${icon('i-users')}
    <p class="empty-title lv-empty-title">${text(title)}</p>
    ${body ? `<p class="lv-empty-body">${text(body)}</p>` : ''}
    ${action ? `<button type="button" class="btn btn--ghost btn--sm" data-empty-act="${action.id}">${text(action.label)}</button>` : ''}`;
}

function hideEmpty() {
  els.table.classList.remove('is-empty');
  if (els.empty.hidden) return;
  els.empty.hidden = true;
  els.empty.innerHTML = '';
}

/** Numbers only: no DOM rebuild, so hover, focus and clicks are untouched. */
function updateNumbers(use) {
  for (const tr of els.tbody.querySelectorAll('tr.usr-row')) {
    const u = use.get(Number(tr.dataset.sid)) || EMPTY_USE;
    const h = heats(u);
    tr.querySelector('[data-u="count"]').textContent = String(u.count);
    tr.querySelector('[data-u="cpu"]').textContent = pct(u.cpu);
    tr.querySelector('[data-u="memory"]').textContent = bytes(u.memory);
    tr.querySelector('[data-u="io"]').textContent = rate(u.io);
    for (const td of tr.querySelectorAll('[data-h]')) td.style.setProperty('--h', String(h[td.dataset.h]));
  }
  const byPid = new Map();
  for (const u of use.values()) for (const r of u.procs) byPid.set(r.pid, r);
  for (const btn of els.tbody.querySelectorAll('.usr-proc[data-pid]')) {
    const r = byPid.get(Number(btn.dataset.pid));
    if (!r) continue;
    btn.querySelector('[data-p="cpu"]').textContent = pct(r.cpu);
    btn.querySelector('[data-p="memory"]').textContent = bytes(r.memory);
    btn.querySelector('[data-p="io"]').textContent = rate((r.diskReadBps || 0) + (r.diskWriteBps || 0));
  }
}

function renderTable(use) {
  if (!loaded) {
    shown = [];
    els.tbody.innerHTML = '';
    lastSig = '';
    els.count.textContent = '';
    if (loadError) showEmpty('Could not read the signed-in users.', loadError, { id: 'retry', label: 'Try again' });
    else showEmpty('Reading signed-in users.', 'Listing every Windows session on this PC.');
    return;
  }
  const list = visibleSessions(use);
  els.count.textContent = `${list.length} of ${sessions.length}`;
  if (!list.length) {
    shown = [];
    els.tbody.innerHTML = '';
    lastSig = '';
    const q = (store.query || '').trim();
    if (!sessions.length) showEmpty('Nobody is signed in.', 'Windows has no user sessions right now.');
    else showEmpty(`No users match “${q}”.`, 'Clear the search to see everyone.');
    return;
  }
  hideEmpty();
  const iconsWanted = [];
  const sig = `${isAdmin()}|${list.map((s) => {
    let part = `${s.sessionId}#${s.userName}#${s.domain}#${s.state}#${s.clientName}#${s.logonTime}#${s.isCurrent}`;
    if (expanded.has(s.sessionId)) {
      const top = topProcs(use.get(s.sessionId) || EMPTY_USE);
      for (const r of top) if (r.exe) iconsWanted.push(r.exe);
      part += `#open:${top.map((r) => `${r.pid}${r.exe && icons.get(r.exe) ? '+' : ''}`).join(',')}:${(use.get(s.sessionId) || EMPTY_USE).count}`;
    }
    return part;
  }).join(';')}`;
  if (iconsWanted.length) loadIcons(iconsWanted);
  if (sig === lastSig) {
    updateNumbers(use);
    return;
  }
  if (pointerDown) {
    // A rebuild between pointerdown and pointerup would swallow the click: wait for it.
    pendingRender = true;
    updateNumbers(use);
    return;
  }
  lastSig = sig;

  const focused = document.activeElement;
  let focus = null;
  if (focused && els.tbody.contains(focused)) {
    focus = {
      sid: focused.closest('[data-sid]')?.dataset.sid,
      act: focused.dataset.act || null,
      pid: focused.dataset.pid || null,
    };
  }
  shown = list;
  els.tbody.innerHTML = list.map((s, i) => rowHtml(s, i, use.get(s.sessionId) || EMPTY_USE)).join('');
  if (focus && focus.sid != null) {
    let again = null;
    if (focus.pid) again = els.tbody.querySelector(`.usr-proc[data-pid="${focus.pid}"]`);
    else if (focus.act) again = els.tbody.querySelector(`[data-sid="${focus.sid}"] [data-act="${focus.act}"]`);
    if (again && !again.disabled) again.focus({ preventScroll: true });
  }
}

function render() {
  if (!root) return;
  const use = usage();
  renderHead(use);
  renderTable(use);
}

// ------------------------------------------------------------------ actions

function sessionById(id) {
  return sessions.find((s) => s.sessionId === id) || null;
}

function actionError(err) {
  const msg = errText(err);
  const admin = /administrator/i.test(msg) && !isAdmin();
  toast(msg, 'error', admin ? { label: 'Restart as admin', run: restartAsAdmin } : null);
}

async function runAction(s, action, done) {
  try {
    await call('session_action', { sessionId: s.sessionId, action });
    toast(done, 'success');
    setTimeout(() => refresh(), 800);
    return true;
  } catch (err) {
    actionError(err);
    return false;
  }
}

async function disconnectSession(s) {
  if (!s) return;
  if (s.state === 'disconnected') {
    toast(`${s.userName} is already disconnected.`, 'info');
    return;
  }
  const ok = s.isCurrent
    ? await confirmDialog({
      title: 'Disconnect your session?',
      body: 'Windows shows the sign-in screen. Your apps, Orbit included, keep running until you sign back in.',
      confirmLabel: 'Disconnect',
      danger: true,
    })
    : await confirmDialog({
      title: `Disconnect ${s.userName}?`,
      body: `${s.userName}'s apps keep running. They can sign back in and pick up where they left off.`,
      confirmLabel: 'Disconnect',
    });
  if (!ok) return;
  await runAction(s, 'disconnect', s.isCurrent ? 'Disconnecting your session.' : `Disconnected ${s.userName}.`);
}

async function signOutSession(s) {
  if (!s) return;
  const ok = await confirmDialog(s.isCurrent
    ? {
      title: 'Sign out of Windows?',
      body: 'Windows closes every app in your session, Orbit included. Unsaved work in them is lost.',
      confirmLabel: 'Sign out',
      danger: true,
    }
    : {
      title: `Sign out ${s.userName}?`,
      body: `Windows closes every app ${s.userName} has open. Their unsaved work is lost.`,
      confirmLabel: 'Sign out',
      danger: true,
    });
  if (!ok) return;
  await runAction(s, 'signout', s.isCurrent ? 'Signing out.' : `Signed out ${s.userName}.`);
}

function sendMessage(s) {
  if (!s) return Promise.resolve(false);
  let area = null;
  return formDialog({
    label: 'Send message',
    title: s.isCurrent ? 'Send a message to this screen' : `Send a message to ${s.userName}`,
    body: `It pops up on ${s.isCurrent ? 'your' : `${s.userName}'s`} screen as "Message from Orbit". Orbit doesn't wait for a reply.`,
    okLabel: 'Send',
    render(fields, ctl) {
      fields.innerHTML = `
        <label class="dialog__field">
          <span class="dialog__field-label micro">Message</span>
          <textarea class="input dialog__input usr-msg" rows="4" maxlength="${MAX_MESSAGE}" autofocus placeholder="Saving work, back in 5 minutes."></textarea>
        </label>
        <p class="usr-msg-count micro" aria-live="polite"></p>`;
      area = fields.querySelector('textarea');
      const count = fields.querySelector('.usr-msg-count');
      const sync = () => {
        const n = area.value.length;
        count.textContent = `${n} / ${MAX_MESSAGE}`;
        ctl.setOk(!!area.value.trim());
        ctl.error('');
      };
      area.addEventListener('input', sync);
      sync();
    },
    async onSubmit() {
      const message = area.value.trim();
      if (!message) return 'Type a message first.';
      try {
        await call('session_action', { sessionId: s.sessionId, action: 'message', message });
      } catch (err) {
        const msg = errText(err);
        if (/administrator/i.test(msg) && !isAdmin()) {
          return "Access denied. Windows lets only administrators message other users. Try Restart as administrator.";
        }
        return msg;
      }
      toast(s.isCurrent ? 'Message sent.' : `Message sent to ${s.userName}.`, 'success');
      return true;
    },
  });
}

function toggle(id) {
  if (expanded.has(id)) expanded.delete(id);
  else expanded.add(id);
  lastSig = '';
  render();
}

function menuItems(s) {
  const open = expanded.has(s.sessionId);
  return [
    { label: open ? 'Hide processes' : 'Show processes', icon: 'i-list', action: () => toggle(s.sessionId) },
    { separator: true },
    { label: 'Send message', icon: 'i-user', action: () => sendMessage(s) },
    { label: 'Disconnect', icon: 'i-x', disabled: s.state === 'disconnected', action: () => disconnectSession(s) },
    { label: 'Sign out', icon: 'i-stop', danger: true, action: () => signOutSession(s) },
  ];
}

function onTableClick(e) {
  const t = e.target instanceof Element ? e.target : null;
  if (!t) return;
  const proc = t.closest('.usr-proc[data-pid]');
  if (proc) {
    const pid = Number(proc.dataset.pid);
    const snap = store.snapshot;
    if (snap && snap.processes.some((r) => r.pid === pid)) selectProcess(pid);
    else toast('That process has exited.', 'info');
    return;
  }
  const holder = t.closest('[data-sid]');
  const s = holder ? sessionById(Number(holder.dataset.sid)) : null;
  const btn = t.closest('[data-act]');
  if (!s) return;
  if (!btn) {
    // A click anywhere else on the main row opens or closes it.
    if (t.closest('tr.usr-row')) toggle(s.sessionId);
    return;
  }
  if (btn.disabled) return;
  switch (btn.dataset.act) {
    case 'toggle': toggle(s.sessionId); break;
    case 'menu': {
      const r = btn.getBoundingClientRect();
      showMenu(r.right - 4, r.bottom + 4, menuItems(s));
      break;
    }
    case 'message': sendMessage(s); break;
    case 'disconnect': disconnectSession(s); break;
    case 'signout': signOutSession(s); break;
    case 'admin': restartAsAdmin(); break;
    default: break;
  }
}

function onContextMenu(e) {
  const tr = e.target instanceof Element ? e.target.closest('tr.usr-row') : null;
  const s = tr ? sessionById(Number(tr.dataset.sid)) : null;
  if (!s) return;
  e.preventDefault();
  showMenu(e.clientX, e.clientY, menuItems(s));
}

function onSnapshot() {
  if (!isVisible() || document.hidden) return;
  render();
}

// ------------------------------------------------------------------ palette

function paletteQuery() {
  const el = document.getElementById('palette-input');
  return el ? el.value.trim() : '';
}

function paletteItems() {
  const items = [{
    label: 'Signed-in users',
    search: 'users sessions signed in accounts who is logged on',
    icon: 'i-users',
    hint: 'Users',
    run: () => navigate('users'),
  }];
  // Per-user actions only once something is typed, so the empty palette stays short.
  if (paletteQuery().length < 2) return items;
  for (const s of sessions) {
    const who = s.isCurrent ? `${s.userName} (you)` : s.userName;
    items.push({
      label: `Send message to ${who}`,
      search: `send message to ${s.userName} user session notify`,
      icon: 'i-user',
      hint: 'Users',
      run: () => sendMessage(s),
    });
    if (s.state !== 'disconnected') {
      items.push({
        label: `Disconnect ${who}`,
        search: `disconnect ${s.userName} user session lock`,
        icon: 'i-x',
        hint: 'Users',
        run: () => disconnectSession(s),
      });
    }
    items.push({
      label: `Sign out ${who}`,
      search: `sign out ${s.userName} log off logoff user session`,
      icon: 'i-stop',
      hint: 'Users',
      danger: true,
      run: () => signOutSession(s),
    });
  }
  return items;
}

// ------------------------------------------------------------------ view API

export function mount(rootEl) {
  root = rootEl;
  root.classList.add('view--fill');
  root.innerHTML = `
  <div class="lv usr">
    <header class="view-head">
      <div class="lv-head-text">
        <p class="micro">05 / Users</p>
        <h1 class="view-title">Who is on. <em>What they use.</em></h1>
      </div>
      <div class="lv-stats" aria-live="polite">
        <div class="stat lv-stat"><span class="micro stat-label">Signed in</span><span class="stat-value lv-stat-value" data-stat="users">-</span></div>
        <div class="stat lv-stat"><span class="micro stat-label">Active</span><span class="stat-value lv-stat-value" data-stat="active">-</span></div>
        <div class="stat lv-stat"><span class="micro stat-label">Disconnected</span><span class="stat-value lv-stat-value" data-stat="disc">-</span></div>
        <div class="stat lv-stat"><span class="micro stat-label">Processes</span><span class="stat-value lv-stat-value" data-stat="procs">-</span></div>
      </div>
    </header>
    <div class="toolbar lv-toolbar">
      <p class="usr-note">CPU, memory and I/O add up every process in each session. Memory shared between processes counts once for each, so it can read high.</p>
      <span class="lv-toolbar-spacer"></span>
      <span class="micro lv-count" data-count aria-live="polite"></span>
      <button type="button" class="icon-btn lv-refresh" data-refresh aria-label="Refresh users" title="Refresh (updates every 5 seconds)">${icon('i-refresh')}</button>
    </div>
    <div class="panel lv-table-panel">
      <div class="lv-scroll">
        <table class="table lv-table usr-table">
          <thead><tr>
            <th class="c-user" scope="col" data-col="user" aria-sort="none"><button type="button" class="lv-sort" data-sort="user">User</button></th>
            <th class="c-session cell-num" scope="col" data-col="session" aria-sort="none"><button type="button" class="lv-sort" data-sort="session">Session</button></th>
            <th class="c-state" scope="col" data-col="state" aria-sort="none"><button type="button" class="lv-sort" data-sort="state">Status</button></th>
            <th class="c-client" scope="col"><span>Client</span></th>
            <th class="c-logon" scope="col" data-col="logon" aria-sort="none"><button type="button" class="lv-sort" data-sort="logon">Signed in</button></th>
            <th class="c-procs cell-num" scope="col" data-col="procs" aria-sort="none"><button type="button" class="lv-sort" data-sort="procs">Processes</button></th>
            <th class="c-cpu cell-num" scope="col" data-col="cpu" aria-sort="none"><button type="button" class="lv-sort" data-sort="cpu">CPU</button></th>
            <th class="c-mem cell-num" scope="col" data-col="memory" aria-sort="none" title="${text(MEM_TITLE)}"><button type="button" class="lv-sort" data-sort="memory">Memory</button></th>
            <th class="c-io cell-num" scope="col" data-col="io" aria-sort="none" title="${text(IO_TITLE)}"><button type="button" class="lv-sort" data-sort="io">I/O</button></th>
            <th class="c-actions" scope="col"><span class="lv-sr">Actions</span></th>
          </tr></thead>
          <tbody></tbody>
        </table>
        <div class="empty lv-empty" hidden></div>
      </div>
    </div>
  </div>`;

  els.table = root.querySelector('.usr-table');
  els.tbody = els.table.querySelector('tbody');
  els.empty = root.querySelector('.lv-empty');
  els.count = root.querySelector('[data-count]');
  els.refresh = root.querySelector('[data-refresh]');
  els.statUsers = root.querySelector('[data-stat="users"]');
  els.statActive = root.querySelector('[data-stat="active"]');
  els.statDisc = root.querySelector('[data-stat="disc"]');
  els.statProcs = root.querySelector('[data-stat="procs"]');

  els.tbody.addEventListener('click', onTableClick);
  els.tbody.addEventListener('contextmenu', onContextMenu);
  els.tbody.addEventListener('pointerdown', () => { pointerDown = true; });
  const release = () => {
    if (!pointerDown) return;
    pointerDown = false;
    if (pendingRender) {
      pendingRender = false;
      // After the click handler has run.
      setTimeout(() => { if (isVisible()) render(); }, 0);
    }
  };
  window.addEventListener('pointerup', release, true);
  window.addEventListener('pointercancel', release, true);
  els.table.querySelector('thead').addEventListener('click', (e) => {
    const btn = e.target instanceof Element ? e.target.closest('[data-sort]') : null;
    if (!btn) return;
    const key = btn.dataset.sort;
    const numeric = ['procs', 'cpu', 'memory', 'io', 'logon'].includes(key);
    sort = sort.key === key ? { key, dir: -sort.dir } : { key, dir: numeric ? -1 : 1 };
    lastSig = '';
    render();
  });
  els.refresh.addEventListener('click', () => refresh());
  els.empty.addEventListener('click', (e) => {
    const btn = e.target instanceof Element ? e.target.closest('[data-empty-act]') : null;
    if (btn && btn.dataset.emptyAct === 'retry') refresh();
  });

  let cols = 0;
  window.addEventListener('resize', () => {
    // The media rules hide columns: rebuild so the expanded rows span the right count.
    const n = visibleCols();
    if (n === cols) return;
    cols = n;
    lastSig = '';
    if (isVisible()) render();
  });
  on('snapshot', onSnapshot);
  on('query', () => { if (isVisible()) { lastSig = ''; render(); } });
  document.addEventListener('visibilitychange', () => {
    if (isVisible() && !document.hidden) refresh();
  });
  if (!unregister) unregister = registerCommands(paletteItems);
  // One quiet read at boot so the palette can name the users before the view is opened.
  refresh().catch(() => {});
  render();
}

export function onShow() {
  visible = true;
  lastSig = '';
  render();
  refresh();
  startTimer();
}

export function onHide() {
  visible = false;
  stopTimer();
}

export function isVisible() {
  return visible && !!root;
}
