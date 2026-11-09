// 07 / Network: open connections and listening ports, with the process behind each.
//
// The connection table comes from `list_connections` (no admin needed) and refreshes every
// 2 s while the view is visible. "Find port" answers "What's using port 3000?": it filters
// the table to that local port and names the apps holding it. Per-app download and upload
// rates come from the snapshot rows (`netRxBps` / `netTxBps`) when `network.perApp` is
// available, which needs Orbit to run as administrator.

import { call } from '../api.js';
import { store, on, selectProcess } from '../store.js';
import { escapeHtml, fuzzy, rate } from '../format.js';
import { toast, navigate, restartAsAdmin } from '../app.js';
import { registerCommands } from '../palette.js';
import { endTask, copyText } from './processes.js';

const REFRESH_MS = 2000;
const ICON_RETRY_MS = 4000;
const TOP_N = 5;

const STATES = {
  listen: 'Listening',
  established: 'Established',
  synSent: 'Connecting',
  synReceived: 'SYN received',
  finWait1: 'Closing (FIN wait 1)',
  finWait2: 'Closing (FIN wait 2)',
  closeWait: 'Close wait',
  closing: 'Closing',
  lastAck: 'Last ACK',
  timeWait: 'Time wait',
  closed: 'Closed',
  deleteTcb: 'Deleted',
  unknown: 'Unknown',
};

const STATE_ORDER = ['listen', 'established', 'synSent', 'synReceived', 'closeWait', 'finWait1', 'finWait2', 'closing', 'lastAck', 'timeWait', 'closed', 'deleteTcb', 'unknown'];

const VIEWS = [
  { id: 'all', label: 'All', test: () => true },
  { id: 'listening', label: 'Listening', title: 'TCP ports waiting for connections, and open UDP ports', test: (c) => c.state === 'listen' || c.protocol === 'udp' },
  { id: 'established', label: 'Established', title: 'Open TCP connections', test: (c) => c.state === 'established' },
];

let root = null;
const els = {};
let visible = false;
let conns = [];
let loaded = false;
let loading = false;
let loadError = null;
let errorToasted = false;
let timer = 0;
let view = 'all';
const proto = { tcp: true, udp: true };
let portText = '';
let sort = { key: 'local', dir: 1 };
let lastSig = '';
let lastAppsSig = '';
let shown = []; // connections in the table, in order
const icons = new Map(); // exe -> data URL, or '' when Windows has none
let iconTimer = 0;
let unregister = null;
let statusCache = null; // per_app_network_status fallback when the snapshot has no perApp

const icon = (id) => `<svg class="icon" aria-hidden="true"><use href="#${id}"/></svg>`;
const text = (s) => escapeHtml(s == null ? '' : String(s));

function errText(err) {
  if (typeof err === 'string' && err) return err;
  if (err && typeof err.message === 'string' && err.message) return err.message;
  return 'Something went wrong. Try again.';
}

// ------------------------------------------------------------------ helpers

function procRows() {
  const snap = store.snapshot;
  return snap && Array.isArray(snap.processes) ? snap.processes : [];
}

function procMap() {
  const m = new Map();
  for (const r of procRows()) m.set(r.pid, r);
  return m;
}

function procName(pid, row) {
  if (row && row.name) return row.name;
  if (pid === 0) return 'System Idle Process';
  if (pid === 4) return 'System';
  return `PID ${pid}`;
}

function ownPid() {
  return store.staticInfo && Number.isFinite(store.staticInfo.pid) ? store.staticInfo.pid : null;
}

function isAdmin() {
  return !!(store.staticInfo && store.staticInfo.isAdmin);
}

/** "1.2.3.4:80" or "[::1]:5173". */
function hostPort(addr, port, family) {
  if (addr == null) return '';
  const host = family === 'ipv6' ? `[${addr}]` : addr;
  return port == null ? host : `${host}:${port}`;
}

/** Remote hosts worth a web search: not unspecified, loopback, link-local or private. */
function isPublicHost(addr) {
  if (!addr) return false;
  const a = String(addr).toLowerCase();
  if (a === '0.0.0.0' || a === '::' || a === '::1') return false;
  if (a.includes(':')) {
    if (a.startsWith('fe8') || a.startsWith('fe9') || a.startsWith('fea') || a.startsWith('feb')) return false;
    if (a.startsWith('fc') || a.startsWith('fd')) return false;
    if (a.startsWith('::ffff:')) return isPublicHost(a.slice(7));
    return true;
  }
  const p = a.split('.').map(Number);
  if (p.length !== 4 || p.some((n) => !Number.isFinite(n))) return false;
  if (p[0] === 127 || p[0] === 10 || p[0] === 0) return false;
  if (p[0] === 169 && p[1] === 254) return false;
  if (p[0] === 172 && p[1] >= 16 && p[1] <= 31) return false;
  if (p[0] === 192 && p[1] === 168) return false;
  if (p[0] === 100 && p[1] >= 64 && p[1] <= 127) return false;
  return true;
}

function keyOf(c) {
  return `${c.protocol}|${c.family}|${c.localAddress}|${c.localPort}|${c.remoteAddress}|${c.remotePort}|${c.pid}`;
}

/** The port number in "3000", ":3000", "port 3000", "localhost:3000"; null if empty, NaN if not a port. */
function parsePort(s) {
  const t = String(s || '').trim();
  if (!t) return null;
  const m = t.match(/(\d{1,6})\s*$/);
  if (!m) return NaN;
  const n = Number(m[1]);
  return n >= 1 && n <= 65535 ? n : NaN;
}

function stateLabel(c) {
  if (c.protocol === 'udp' || !c.state) return null;
  return STATES[c.state] || c.state;
}

// ------------------------------------------------------------------ data

async function refresh() {
  if (loading || document.hidden) return;
  loading = true;
  if (els.refresh) els.refresh.classList.add('is-busy');
  try {
    const res = await call('list_connections');
    conns = Array.isArray(res) ? res : [];
    loaded = true;
    loadError = null;
    errorToasted = false;
  } catch (err) {
    loadError = errText(err);
    if (loaded && !errorToasted) {
      errorToasted = true;
      toast(`Could not refresh connections. ${loadError}`, 'error');
    }
  }
  loading = false;
  if (els.refresh) els.refresh.classList.remove('is-busy');
  if (isVisible()) {
    loadIcons();
    render();
  }
}

function startTimer() {
  stopTimer();
  timer = setInterval(() => { if (isVisible()) refresh(); }, REFRESH_MS);
}

function stopTimer() {
  if (timer) clearInterval(timer);
  timer = 0;
}

async function loadIcons() {
  const procs = procMap();
  const want = new Set();
  for (const c of conns) {
    const r = procs.get(c.pid);
    if (r && r.exe && !icons.has(r.exe)) want.add(r.exe);
  }
  for (const r of topTalkers()) if (r.exe && !icons.has(r.exe)) want.add(r.exe);
  const list = [...want];
  if (!list.length) return;
  for (const p of list) icons.set(p, ''); // asked for; blank until the answer arrives
  try {
    const map = (await call('get_icons', { paths: list })) || {};
    const missing = [];
    for (const p of list) {
      const src = map[p];
      if (typeof src === 'string' && src.startsWith('data:image/')) icons.set(p, src);
      else missing.push(p);
    }
    if (missing.length && !iconTimer) {
      // Windows sometimes needs a moment for an icon; ask once more.
      iconTimer = setTimeout(async () => {
        iconTimer = 0;
        try {
          const m2 = (await call('get_icons', { paths: missing })) || {};
          for (const p of missing) if (typeof m2[p] === 'string' && m2[p].startsWith('data:image/')) icons.set(p, m2[p]);
        } catch (_) { /* keep the blank tiles */ }
        lastSig = '';
        lastAppsSig = '';
        if (isVisible()) render();
      }, ICON_RETRY_MS);
    }
  } catch (_) { /* keep the blank tiles */ }
  lastSig = '';
  lastAppsSig = '';
  if (isVisible()) render();
}

function iconCell(exe) {
  const src = exe ? icons.get(exe) : '';
  if (src) return `<img class="lv-ico" src="${src}" alt="" width="18" height="18" decoding="async">`;
  return `<span class="lv-ico lv-ico--blank" aria-hidden="true">${icon('i-grid')}</span>`;
}

// ------------------------------------------------------------------ filtering

function matchesQuery(c, name, q) {
  if (!q) return true;
  const st = stateLabel(c) || '';
  const hay = `${name} ${c.pid} ${c.protocol} ${hostPort(c.localAddress, c.localPort, c.family)} ${c.remoteAddress ? hostPort(c.remoteAddress, c.remotePort, c.family) : ''} ${st}`;
  return fuzzy(hay, q) >= 1000; // substring matches, as the other lists do
}

function baseFilter(c) {
  if (!proto[c.protocol]) return false;
  const v = VIEWS.find((x) => x.id === view) || VIEWS[0];
  return v.test(c);
}

function compare(a, b, procs) {
  const d = sort.dir;
  const name = (c) => procName(c.pid, procs.get(c.pid)).toLowerCase();
  let r = 0;
  switch (sort.key) {
    case 'proc': r = name(a).localeCompare(name(b)) || a.pid - b.pid; break;
    case 'pid': r = a.pid - b.pid; break;
    case 'proto': r = a.protocol.localeCompare(b.protocol) || a.family.localeCompare(b.family); break;
    case 'remote': {
      const ra = a.remoteAddress || '';
      const rb = b.remoteAddress || '';
      if (!ra !== !rb) return ra ? -1 : 1; // rows without a remote end last, either direction
      r = ra.localeCompare(rb, undefined, { numeric: true }) || (a.remotePort || 0) - (b.remotePort || 0);
      break;
    }
    case 'state': {
      const ia = a.protocol === 'udp' ? 99 : STATE_ORDER.indexOf(a.state);
      const ib = b.protocol === 'udp' ? 99 : STATE_ORDER.indexOf(b.state);
      r = ia - ib;
      break;
    }
    case 'local':
    default:
      r = a.localPort - b.localPort || a.localAddress.localeCompare(b.localAddress, undefined, { numeric: true });
      break;
  }
  if (r === 0) r = a.localPort - b.localPort || a.pid - b.pid;
  return r * d;
}

function visibleConns(procs) {
  const q = (store.query || '').trim();
  const port = parsePort(portText);
  const out = conns.filter((c) => {
    if (Number.isFinite(port) && c.localPort !== port) return false;
    if (!baseFilter(c)) return false;
    return matchesQuery(c, procName(c.pid, procs.get(c.pid)), q);
  });
  out.sort((a, b) => compare(a, b, procs));
  return out;
}

// ------------------------------------------------------------------ render: head, filters

function counts() {
  const c = { total: conns.length, listen: 0, established: 0, tcp: 0, udp: 0, apps: 0, listening: 0 };
  const pids = new Set();
  for (const x of conns) {
    if (x.state === 'listen') c.listen++;
    if (x.state === 'established') c.established++;
    if (x.state === 'listen' || x.protocol === 'udp') c.listening++;
    if (x.protocol === 'tcp') c.tcp++;
    else c.udp++;
    if (x.pid) pids.add(x.pid);
  }
  c.apps = pids.size;
  return c;
}

function renderHead() {
  const c = counts();
  const done = loaded;
  els.statTotal.textContent = done ? String(c.total) : '-';
  els.statListen.textContent = done ? String(c.listen) : '-';
  els.statEst.textContent = done ? String(c.established) : '-';
  els.statApps.textContent = done ? String(c.apps) : '-';

  for (const btn of els.views.querySelectorAll('[data-net-view]')) {
    const on = btn.dataset.netView === view;
    btn.classList.toggle('chip--on', on);
    btn.setAttribute('aria-pressed', on ? 'true' : 'false');
    const n = btn.dataset.netView === 'all' ? c.total : btn.dataset.netView === 'listening' ? c.listening : c.established;
    btn.querySelector('.lv-chip-count').textContent = done ? String(n) : '';
  }
  for (const btn of els.protos.querySelectorAll('[data-proto]')) {
    const on = proto[btn.dataset.proto];
    btn.classList.toggle('chip--on', on);
    btn.setAttribute('aria-pressed', on ? 'true' : 'false');
    btn.querySelector('.lv-chip-count').textContent = done ? String(c[btn.dataset.proto]) : '';
  }
  for (const th of els.table.querySelectorAll('th[data-col]')) {
    const active = th.dataset.col === sort.key;
    th.classList.toggle('is-sorted-asc', active && sort.dir === 1);
    th.classList.toggle('is-sorted-desc', active && sort.dir === -1);
    th.setAttribute('aria-sort', active ? (sort.dir === 1 ? 'ascending' : 'descending') : 'none');
  }
}

// ------------------------------------------------------------------ render: find port

function renderFind(procs) {
  const port = parsePort(portText);
  els.findClear.hidden = !portText;
  els.findBox.classList.toggle('is-active', Number.isFinite(port));
  if (port === null) {
    els.findResult.innerHTML = '<p class="net-find-hint">Type a port number to see which app holds it.</p>';
    return;
  }
  if (!Number.isFinite(port)) {
    els.findResult.innerHTML = '<p class="net-find-hint net-find-hint--warn">Ports are numbers from 1 to 65535.</p>';
    return;
  }
  if (!loaded) {
    els.findResult.innerHTML = `<p class="net-find-hint">${loadError ? text(loadError) : 'Reading connections.'}</p>`;
    return;
  }
  const hits = conns.filter((c) => c.localPort === port);
  // Apps on this PC connected to that port here (a browser tab on a local dev server).
  const local = new Set(hits.map((c) => c.localAddress));
  const clients = conns.filter((c) => c.remotePort === port && c.remoteAddress && (local.has(c.remoteAddress) || !isPublicHost(c.remoteAddress)) && c.localPort !== port);
  if (!hits.length) {
    els.findResult.innerHTML = `<p class="net-find-answer"><strong>Port ${port} is free.</strong> Nothing on this PC is using it right now.</p>`;
    return;
  }
  const byPid = new Map();
  for (const c of hits) {
    if (!byPid.has(c.pid)) byPid.set(c.pid, []);
    byPid.get(c.pid).push(c);
  }
  const own = ownPid();
  const items = [...byPid.entries()].map(([pid, list]) => {
    const row = procs.get(pid);
    const name = procName(pid, row);
    const listens = list.filter((c) => c.state === 'listen');
    const udp = list.filter((c) => c.protocol === 'udp');
    const other = list.length - listens.length - udp.length;
    const bits = [];
    if (listens.length) bits.push(`Listening on ${[...new Set(listens.map((c) => hostPort(c.localAddress, null, c.family)))].join(', ')} (TCP)`);
    if (udp.length) bits.push(`Open on ${[...new Set(udp.map((c) => hostPort(c.localAddress, null, c.family)))].join(', ')} (UDP)`);
    if (other > 0) bits.push(`${other} connection${other === 1 ? '' : 's'}`);
    const canEnd = !!row && pid !== 0 && pid !== 4 && pid !== own;
    return `<li class="net-hit" data-pid="${pid}">
      ${iconCell(row && row.exe)}
      <div class="net-hit-main">
        <span class="net-hit-name">${text(name)} <span class="lv-faint lv-num">PID ${pid}</span></span>
        <span class="net-hit-sub">${text(bits.join('. '))}</span>
      </div>
      <button type="button" class="btn btn--ghost btn--sm" data-hit="details"${row ? '' : ' disabled'}>Details</button>
      <button type="button" class="btn btn--danger btn--sm" data-hit="end"${canEnd ? '' : ' disabled'}>End task</button>
    </li>`;
  });
  const clientNote = clients.length
    ? `<p class="net-find-hint">${clients.length} connection${clients.length === 1 ? '' : 's'} from other apps on this PC ${clients.length === 1 ? 'goes' : 'go'} to it.</p>`
    : '';
  els.findResult.innerHTML = `<p class="net-find-answer"><strong>Port ${port}</strong> is used by ${byPid.size === 1 ? 'one app' : `${byPid.size} apps`}.</p>
    <ul class="net-hits">${items.join('')}</ul>${clientNote}`;
}

// ------------------------------------------------------------------ render: per-app rates

function perAppStatus() {
  const snap = store.snapshot;
  const s = snap && snap.network && snap.network.perApp;
  if (s && typeof s.available === 'boolean') return s;
  if (!statusCache) {
    statusCache = { available: false, reason: null };
    Promise.resolve(call('per_app_network_status')).then((r) => {
      if (r && typeof r.available === 'boolean') statusCache = r;
      lastAppsSig = '';
      if (isVisible()) renderApps();
    }).catch(() => {});
  }
  return statusCache;
}

function topTalkers() {
  const s = perAppStatus();
  if (!s.available) return [];
  return procRows()
    .map((r) => ({ pid: r.pid, name: r.name, exe: r.exe, rx: Number(r.netRxBps) || 0, tx: Number(r.netTxBps) || 0 }))
    .filter((r) => r.rx + r.tx > 0)
    .sort((a, b) => (b.rx + b.tx) - (a.rx + a.tx))
    .slice(0, TOP_N);
}

function renderApps() {
  const s = perAppStatus();
  if (!s.available) {
    const reason = s.reason || 'Needs administrator rights. Use Run as admin.';
    const showAdmin = !isAdmin();
    const sig = `off|${reason}|${showAdmin}`;
    if (sig === lastAppsSig) return;
    lastAppsSig = sig;
    els.apps.classList.add('is-off');
    els.appsBody.innerHTML = `<div class="net-apps-off">
      ${icon('i-shield')}
      <p><span class="net-apps-off-title">Download and upload per app</span> ${text(reason)}</p>
      ${showAdmin ? '<button type="button" class="btn btn--ghost btn--sm" data-admin>Run as admin</button>' : ''}
    </div>`;
    return;
  }
  els.apps.classList.remove('is-off');
  const top = topTalkers();
  const sig = `on|${top.map((r) => `${r.pid}:${Math.round(r.rx)}:${Math.round(r.tx)}:${icons.get(r.exe) ? 1 : 0}`).join(',')}`;
  if (sig === lastAppsSig) return;
  lastAppsSig = sig;
  if (!top.length) {
    els.appsBody.innerHTML = '<p class="net-find-hint">No app is sending or receiving right now.</p>';
    return;
  }
  const max = Math.max(...top.map((r) => r.rx + r.tx), 1);
  els.appsBody.innerHTML = `<ol class="net-talkers">${top.map((r) => {
    const w = Math.max(2, Math.round(((r.rx + r.tx) / max) * 100));
    return `<li class="net-talker">
      ${iconCell(r.exe)}
      <div class="net-talker-main">
        <span class="net-talker-name"><button type="button" class="lv-link" data-talker="${r.pid}" title="Details for ${text(r.name)}">${text(r.name)}</button><span class="lv-faint lv-num">${r.pid}</span></span>
        <span class="net-talker-bar" aria-hidden="true"><span style="width:${w}%"></span></span>
      </div>
      <span class="net-rate" title="Download">${icon('i-arrow-down')}<span class="lv-num">${rate(r.rx)}</span></span>
      <span class="net-rate" title="Upload">${icon('i-arrow-up')}<span class="lv-num">${rate(r.tx)}</span></span>
    </li>`;
  }).join('')}</ol>`;
}

// ------------------------------------------------------------------ render: table

function stateCell(c) {
  const label = stateLabel(c);
  if (!label) return '<span class="lv-faint">-</span>';
  const cls = c.state === 'listen' ? 'net-state--listen' : c.state === 'established' ? 'net-state--est' : 'lv-chip--muted';
  return `<span class="chip lv-chip ${cls}">${text(label)}</span>`;
}

function addrCell(addr, port, family, hit) {
  if (addr == null) return '<span class="lv-faint">-</span>';
  const host = family === 'ipv6' ? `[${addr}]` : addr;
  const full = hostPort(addr, port, family);
  const portHtml = port == null ? '' : `:<span class="net-port${hit ? ' net-port--hit' : ''}">${port}</span>`;
  return `<span class="lv-mono net-addr" title="${text(full)}">${text(host)}${portHtml}</span>`;
}

function rowHtml(c, i, procs, hitPort, own) {
  const row = procs.get(c.pid);
  const name = procName(c.pid, row);
  const hit = hitPort !== null && c.localPort === hitPort;
  const canEnd = !!row && c.pid !== 0 && c.pid !== 4 && c.pid !== own;
  const search = isPublicHost(c.remoteAddress);
  const copyWhat = c.remoteAddress ? 'remote' : 'local';
  const fam = c.family === 'ipv6' ? '<span class="net-fam">v6</span>' : '';
  return `<tr class="row net-row${hit ? ' is-hit' : ''}${row ? '' : ' is-dim'}" data-i="${i}">
    <td class="c-proc"><div class="lv-namecell">${iconCell(row && row.exe)}<span class="lv-primary" title="${text(row && row.exe ? row.exe : name)}">${text(name)}</span></div></td>
    <td class="c-pid cell-num"><span class="lv-num">${c.pid}</span></td>
    <td class="c-proto"><span class="net-proto">${c.protocol.toUpperCase()}</span>${fam}</td>
    <td class="c-local">${addrCell(c.localAddress, c.localPort, c.family, hit)}</td>
    <td class="c-remote">${addrCell(c.remoteAddress, c.remotePort, c.family, false)}</td>
    <td class="c-state">${stateCell(c)}</td>
    <td class="c-actions"><div class="net-actions">
      <button type="button" class="icon-btn lv-more" data-act="details" aria-label="Details for ${text(name)}" title="Details"${row ? '' : ' disabled'}>${icon('i-spark')}</button>
      <button type="button" class="icon-btn lv-more" data-act="copy" aria-label="Copy ${copyWhat} address" title="Copy ${copyWhat} address">${icon('i-copy')}</button>
      <button type="button" class="icon-btn lv-more" data-act="search" aria-label="Search online for ${text(c.remoteAddress || 'the remote host')}" title="${search ? 'Search online for the remote host' : 'No public remote host to search for'}"${search ? '' : ' disabled'}>${icon('i-globe')}</button>
      <button type="button" class="icon-btn lv-more net-act--danger" data-act="end" aria-label="End ${text(name)}" title="End task"${canEnd ? '' : ' disabled'}>${icon('i-stop')}</button>
    </div></td>
  </tr>`;
}

function showEmpty(title, body, action) {
  els.table.classList.add('is-empty');
  els.empty.hidden = false;
  els.empty.innerHTML = `${icon('i-plug')}
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

function renderTable(procs) {
  const port = parsePort(portText);
  const hitPort = Number.isFinite(port) ? port : null;
  if (!loaded) {
    shown = [];
    els.tbody.innerHTML = '';
    lastSig = '';
    els.count.textContent = '';
    if (loadError) showEmpty('Could not read connections.', loadError, { id: 'retry', label: 'Try again' });
    else showEmpty('Reading connections.', 'Listing every open port and connection on this PC.');
    return;
  }
  const list = visibleConns(procs);
  els.count.textContent = `${list.length} of ${conns.length}`;
  if (!list.length) {
    shown = [];
    els.tbody.innerHTML = '';
    lastSig = '';
    const q = (store.query || '').trim();
    if (!conns.length) showEmpty('No connections.', 'Nothing on this PC has a port open right now.');
    else if (hitPort !== null && conns.some((c) => c.localPort === hitPort)) showEmpty(`Port ${hitPort} is in use, but the filters hide it.`, 'Show all connections to see it.', { id: 'all', label: 'Show all' });
    else if (hitPort !== null) showEmpty(`Port ${hitPort} is free.`, 'Nothing on this PC is using it right now.', { id: 'clearport', label: 'Clear port' });
    else showEmpty(q ? `No connections match “${q}”.` : 'No connections in this group.', 'Pick another filter or clear the search.', { id: 'all', label: 'Show all' });
    return;
  }
  hideEmpty();
  const own = ownPid();
  // Skip the DOM work when nothing visible changed (the table refreshes every 2 s).
  const sig = `${hitPort}|${own}|${list.map((c) => {
    const r = procs.get(c.pid);
    return `${keyOf(c)}#${c.state}#${r ? r.name : ''}#${r && r.exe && icons.get(r.exe) ? 1 : 0}`;
  }).join(';')}`;
  if (sig === lastSig) return;
  lastSig = sig;

  const focused = document.activeElement;
  const focusAct = focused && els.tbody.contains(focused) ? focused.dataset.act : null;
  const focusKey = focusAct ? keyOf(shown[Number(focused.closest('tr')?.dataset.i)] || {}) : null;
  shown = list;
  els.tbody.innerHTML = list.map((c, i) => rowHtml(c, i, procs, hitPort, own)).join('');
  if (focusAct && focusKey) {
    const idx = list.findIndex((c) => keyOf(c) === focusKey);
    const again = idx >= 0 ? els.tbody.querySelector(`tr[data-i="${idx}"] [data-act="${focusAct}"]`) : null;
    if (again && !again.disabled) again.focus({ preventScroll: true });
  }
}

function render() {
  if (!root) return;
  const procs = procMap();
  renderHead();
  renderFind(procs);
  renderApps();
  renderTable(procs);
}

// ------------------------------------------------------------------ interaction

async function endFor(pid) {
  const row = procMap().get(pid);
  if (!row) {
    toast('That process has exited.', 'info');
    return;
  }
  if (await endTask(pid, row.name, row.startTime)) {
    setTimeout(() => { if (isVisible()) refresh(); }, 600);
  }
}

function details(pid) {
  if (procMap().has(pid)) selectProcess(pid);
  else toast('That process has exited.', 'info');
}

async function onTableClick(e) {
  const btn = e.target instanceof Element ? e.target.closest('[data-act]') : null;
  if (!btn || btn.disabled) return;
  const tr = btn.closest('tr[data-i]');
  const c = tr ? shown[Number(tr.dataset.i)] : null;
  if (!c) return;
  switch (btn.dataset.act) {
    case 'details':
      details(c.pid);
      break;
    case 'copy': {
      const addr = c.remoteAddress ? hostPort(c.remoteAddress, c.remotePort, c.family) : hostPort(c.localAddress, c.localPort, c.family);
      if (await copyText(addr)) toast(`Copied ${addr}.`, 'success');
      else toast('Could not copy the address.', 'error');
      break;
    }
    case 'search':
      try {
        await call('search_online', { query: c.remoteAddress });
      } catch (err) {
        toast(errText(err), 'error');
      }
      break;
    case 'end':
      btn.disabled = true;
      await endFor(c.pid);
      btn.disabled = false;
      break;
    default:
      break;
  }
}

function setPortText(v, { focus = false } = {}) {
  portText = String(v == null ? '' : v);
  if (els.findInput && els.findInput.value !== portText) els.findInput.value = portText;
  lastSig = '';
  render();
  if (focus && els.findInput) {
    els.findInput.focus();
    els.findInput.select();
  }
}

/** Open the Network view with the Find port box focused (and filled, when a port is given). */
export function findPort(port) {
  navigate('network');
  // navigate() runs onShow synchronously; focus after the view is painted.
  requestAnimationFrame(() => setPortText(port == null ? portText : String(port), { focus: true }));
}

function onSnapshot() {
  if (!isVisible() || document.hidden) return;
  renderApps();
}

// ------------------------------------------------------------------ palette

function paletteQuery() {
  const el = document.getElementById('palette-input');
  return el ? el.value.trim().toLowerCase() : '';
}

function paletteItems() {
  const q = paletteQuery();
  const m = q.match(/^(?:find\s+)?(?:port\s*)?:?(\d{1,5})$/) || q.match(/(?:port|using)\s*:?(\d{1,5})\b/);
  const n = m ? Number(m[1]) : NaN;
  const items = [];
  if (n >= 1 && n <= 65535) {
    items.push({
      label: `Find port ${n}`,
      search: `Find port ${n} what is using port ${n} ${q}`,
      icon: 'i-plug',
      hint: 'Network',
      run: () => findPort(n),
    });
  }
  items.push({
    label: 'Find port',
    search: 'find port what is using port listening localhost dev server',
    icon: 'i-search',
    hint: 'Network',
    run: () => findPort(),
  });
  items.push({
    label: 'Network connections',
    search: 'network connections ports sockets tcp udp internet established listening',
    icon: 'i-plug',
    hint: 'Network',
    run: () => navigate('network'),
  });
  return items;
}

// ------------------------------------------------------------------ view API

export function mount(rootEl) {
  root = rootEl;
  // The table runs its own scroller inside the view (app.css .view--fill).
  root.classList.add('view--fill');
  root.innerHTML = `
  <div class="lv net">
    <header class="view-head">
      <div class="lv-head-text">
        <p class="micro">07 / Network</p>
        <h1 class="view-title">Every connection. <em>Every port.</em></h1>
      </div>
      <div class="lv-stats" aria-live="polite">
        <div class="stat lv-stat"><span class="micro stat-label">Connections</span><span class="stat-value lv-stat-value" data-stat="total">-</span></div>
        <div class="stat lv-stat"><span class="micro stat-label">Listening</span><span class="stat-value lv-stat-value" data-stat="listen">-</span></div>
        <div class="stat lv-stat"><span class="micro stat-label">Established</span><span class="stat-value lv-stat-value" data-stat="est">-</span></div>
        <div class="stat lv-stat"><span class="micro stat-label">Apps</span><span class="stat-value lv-stat-value" data-stat="apps">-</span></div>
      </div>
    </header>
    <div class="net-top">
      <section class="panel net-find" aria-labelledby="net-find-label">
        <label class="micro micro--bright net-find-label" id="net-find-label" for="net-port">Find port</label>
        <div class="net-find-box">
          ${icon('i-search')}
          <input id="net-port" class="input net-find-input" type="text" inputmode="numeric" autocomplete="off" spellcheck="false"
            placeholder="What’s using port 3000?" aria-describedby="net-find-result">
          <button type="button" class="icon-btn net-find-clear" data-find-clear aria-label="Clear port" title="Clear port" hidden>${icon('i-close')}</button>
        </div>
        <div class="net-find-result" id="net-find-result" aria-live="polite"></div>
      </section>
      <section class="panel net-apps" aria-labelledby="net-apps-label">
        <p class="micro net-apps-label" id="net-apps-label">Top apps / Download and upload</p>
        <div class="net-apps-body"></div>
      </section>
    </div>
    <div class="toolbar lv-toolbar">
      <div class="lv-chips" role="group" aria-label="Show connections" data-views>
        ${VIEWS.map((v) => `<button type="button" class="chip" data-net-view="${v.id}" aria-pressed="false"${v.title ? ` title="${text(v.title)}"` : ''}>${v.label}<span class="lv-chip-count"></span></button>`).join('')}
      </div>
      <span class="net-sep" aria-hidden="true"></span>
      <div class="lv-chips" role="group" aria-label="Protocol" data-protos>
        <button type="button" class="chip" data-proto="tcp" aria-pressed="true">TCP<span class="lv-chip-count"></span></button>
        <button type="button" class="chip" data-proto="udp" aria-pressed="true">UDP<span class="lv-chip-count"></span></button>
      </div>
      <span class="lv-toolbar-spacer"></span>
      <span class="micro lv-count" data-count aria-live="polite"></span>
      <button type="button" class="icon-btn lv-refresh" data-refresh aria-label="Refresh connections" title="Refresh (updates every 2 seconds)">${icon('i-refresh')}</button>
    </div>
    <div class="panel lv-table-panel">
      <div class="lv-scroll">
        <table class="table lv-table net-table">
          <thead><tr>
            <th class="c-proc" scope="col" data-col="proc" aria-sort="none"><button type="button" class="lv-sort" data-sort="proc">Process</button></th>
            <th class="c-pid cell-num" scope="col" data-col="pid" aria-sort="none"><button type="button" class="lv-sort" data-sort="pid">PID</button></th>
            <th class="c-proto" scope="col" data-col="proto" aria-sort="none"><button type="button" class="lv-sort" data-sort="proto">Protocol</button></th>
            <th class="c-local" scope="col" data-col="local" aria-sort="none"><button type="button" class="lv-sort" data-sort="local">Local address</button></th>
            <th class="c-remote" scope="col" data-col="remote" aria-sort="none"><button type="button" class="lv-sort" data-sort="remote">Remote address</button></th>
            <th class="c-state" scope="col" data-col="state" aria-sort="none"><button type="button" class="lv-sort" data-sort="state">State</button></th>
            <th class="c-actions" scope="col"><span class="lv-sr">Actions</span></th>
          </tr></thead>
          <tbody></tbody>
        </table>
        <div class="empty lv-empty" hidden></div>
      </div>
    </div>
  </div>`;

  els.table = root.querySelector('.net-table');
  els.tbody = els.table.querySelector('tbody');
  els.empty = root.querySelector('.lv-empty');
  els.views = root.querySelector('[data-views]');
  els.protos = root.querySelector('[data-protos]');
  els.count = root.querySelector('[data-count]');
  els.refresh = root.querySelector('[data-refresh]');
  els.findBox = root.querySelector('.net-find');
  els.findInput = root.querySelector('#net-port');
  els.findClear = root.querySelector('[data-find-clear]');
  els.findResult = root.querySelector('.net-find-result');
  els.apps = root.querySelector('.net-apps');
  els.appsBody = root.querySelector('.net-apps-body');
  els.statTotal = root.querySelector('[data-stat="total"]');
  els.statListen = root.querySelector('[data-stat="listen"]');
  els.statEst = root.querySelector('[data-stat="est"]');
  els.statApps = root.querySelector('[data-stat="apps"]');

  els.findInput.addEventListener('input', () => setPortText(els.findInput.value));
  els.findInput.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && els.findInput.value) {
      e.preventDefault();
      e.stopPropagation();
      setPortText('');
    }
  });
  els.findClear.addEventListener('click', () => setPortText('', { focus: true }));
  els.findResult.addEventListener('click', (e) => {
    const btn = e.target instanceof Element ? e.target.closest('[data-hit]') : null;
    if (!btn || btn.disabled) return;
    const pid = Number(btn.closest('[data-pid]')?.dataset.pid);
    if (!Number.isFinite(pid)) return;
    if (btn.dataset.hit === 'details') details(pid);
    else if (btn.dataset.hit === 'end') endFor(pid);
  });
  els.appsBody.addEventListener('click', (e) => {
    const t = e.target instanceof Element ? e.target : null;
    if (!t) return;
    if (t.closest('[data-admin]')) {
      restartAsAdmin();
      return;
    }
    const link = t.closest('[data-talker]');
    if (link) details(Number(link.dataset.talker));
  });
  els.views.addEventListener('click', (e) => {
    const btn = e.target instanceof Element ? e.target.closest('[data-net-view]') : null;
    if (!btn || btn.dataset.netView === view) return;
    view = btn.dataset.netView;
    lastSig = '';
    render();
  });
  els.protos.addEventListener('click', (e) => {
    const btn = e.target instanceof Element ? e.target.closest('[data-proto]') : null;
    if (!btn) return;
    const p = btn.dataset.proto;
    const other = p === 'tcp' ? 'udp' : 'tcp';
    // Never both off: turning off the last one switches to the other.
    if (proto[p] && !proto[other]) {
      proto[p] = false;
      proto[other] = true;
    } else {
      proto[p] = !proto[p];
    }
    lastSig = '';
    render();
  });
  els.table.querySelector('thead').addEventListener('click', (e) => {
    const btn = e.target instanceof Element ? e.target.closest('[data-sort]') : null;
    if (!btn) return;
    const key = btn.dataset.sort;
    sort = sort.key === key ? { key, dir: -sort.dir } : { key, dir: 1 };
    lastSig = '';
    render();
  });
  els.refresh.addEventListener('click', () => refresh());
  els.tbody.addEventListener('click', onTableClick);
  els.empty.addEventListener('click', (e) => {
    const btn = e.target instanceof Element ? e.target.closest('[data-empty-act]') : null;
    if (!btn) return;
    const act = btn.dataset.emptyAct;
    if (act === 'retry') refresh();
    else if (act === 'clearport') setPortText('', { focus: true });
    else if (act === 'all') {
      view = 'all';
      proto.tcp = true;
      proto.udp = true;
      lastSig = '';
      render();
    }
  });

  on('snapshot', onSnapshot);
  on('query', () => { if (isVisible()) { lastSig = ''; render(); } });
  document.addEventListener('visibilitychange', () => {
    if (isVisible() && !document.hidden) refresh();
  });
  if (!unregister) unregister = registerCommands(paletteItems);
  render();
}

export function onShow() {
  visible = true;
  lastSig = '';
  lastAppsSig = '';
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
