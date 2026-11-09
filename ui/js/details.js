// Orbit / process details drawer (aside#details)
//
// Opens on store 'select' (pid), closes on Esc, the close button, or
// selectProcess(null). Live CPU and memory sparklines follow each snapshot;
// facts from get_process_details refresh every 2 s while the drawer is open.

import { call } from './api.js';
import { store, on, selectProcess } from './store.js';
import { bytes, pct, duration, time, escapeHtml, historySpan, IO_TITLE } from './format.js';
import { sparkline } from './charts.js';
import { toast } from './app.js';
import {
  endTask, endTree, toggleSuspend, setPriority, restartProcess, setAffinity, whenListed,
  PRIORITIES, statusLabel, isPinned, togglePin, copyDetails, copyText,
} from './views/processes.js';

const REFRESH_MS = 2000;
const MAX_CHILDREN = 200;
const DASH = '–';

const d = {
  el: null,
  r: null,
  open: false,
  pid: null,
  name: '',
  startTime: null,
  opener: null,
  timer: 0,
  token: 0,
  inflight: false,
  details: null,
  error: null,
  exited: false,
  cache: {},
  iconExe: undefined,
  priority: null,
  sec: null,
};

// Security parts (signature, modules, environment): loaded on demand per opened process.
const MAX_LIST_ITEMS = 1000;

function freshSec() {
  return {
    sigExe: null, sig: null, sigBusy: false,
    mods: null, modsErr: null, modsBusy: false,
    env: null, envErr: null, envBusy: false,
  };
}

function errText(err) {
  if (typeof err === 'string' && err) return err;
  if (err && typeof err.message === 'string' && err.message) return err.message;
  return 'Something went wrong. Try again.';
}

function icon(id) {
  return `<svg class="icon" aria-hidden="true"><use href="#${id}"/></svg>`;
}

function kindLabel(kind) {
  if (kind === 'app') return 'App';
  if (kind === 'windows') return 'Windows';
  return 'Background';
}

function findRow(pid) {
  const snap = store.snapshot;
  if (!snap || !Array.isArray(snap.processes)) return null;
  const list = snap.processes;
  for (let i = 0; i < list.length; i++) if (list[i].pid === pid) return list[i];
  return null;
}

function hasSnapshot() {
  const snap = store.snapshot;
  return !!(snap && Array.isArray(snap.processes) && snap.processes.length);
}

function set(key, el, value) {
  if (d.cache[key] !== value) {
    d.cache[key] = value;
    el.textContent = value;
  }
}

function setHtml(key, el, html) {
  if (d.cache[key] !== html) {
    d.cache[key] = html;
    el.innerHTML = html;
  }
}

function template() {
  const prio = PRIORITIES.map((p) => `<option value="${p.value}">${p.label}</option>`).join('');
  const fact = (key, label, extra = '') =>
    `<div class="dr-fact${extra}"><dt>${label}</dt><dd data-f="${key}"></dd></div>`;
  const monoFact = (key, label, copyLabel) =>
    `<div class="dr-fact dr-fact--mono"><dt>${label}</dt><dd>` +
      `<span class="dr-mono" data-f="${key}"></span>` +
      `<button type="button" class="icon-btn dr-copy" data-copy="${key}" aria-label="${copyLabel}" title="${copyLabel}">${icon('i-copy')}</button>` +
    '</dd></div>';
  return (
    '<div class="dr-inner">' +
      '<header class="dr-head">' +
        '<span class="dr-ico" data-r="ico" aria-hidden="true"></span>' +
        '<div class="dr-title">' +
          '<p class="micro dr-micro" data-r="micro">Details</p>' +
          '<h2 class="dr-name" id="dr-name" data-r="name"></h2>' +
          '<p class="dr-sub" data-r="sub" hidden></p>' +
          '<div class="dr-chips" data-r="chips"></div>' +
        '</div>' +
        `<button type="button" class="icon-btn dr-close" data-act="close" data-r="close" aria-label="Close details" title="Close (Esc)">${icon('i-close')}</button>` +
      '</header>' +
      '<div class="dr-body" data-r="body">' +
        '<section class="dr-section" aria-label="Live usage">' +
          '<h3 class="micro dr-h" data-r="liveH">Live / 60 s</h3>' +
          '<div class="dr-live">' +
            '<div class="tile dr-spark">' +
              '<div class="dr-spark-head"><span class="micro">CPU</span><span class="dr-spark-val" data-r="cpuVal" aria-live="off">' + DASH + '</span></div>' +
              '<canvas class="dr-canvas" data-r="cpuCanvas" aria-hidden="true"></canvas>' +
            '</div>' +
            '<div class="tile dr-spark">' +
              '<div class="dr-spark-head"><span class="micro">Memory</span><span class="dr-spark-val" data-r="memVal" aria-live="off">' + DASH + '</span></div>' +
              '<canvas class="dr-canvas" data-r="memCanvas" aria-hidden="true"></canvas>' +
            '</div>' +
          '</div>' +
        '</section>' +
        '<section class="dr-section" aria-label="Facts">' +
          '<h3 class="micro dr-h">Facts</h3>' +
          '<dl class="dr-facts">' +
            monoFact('path', 'Path', 'Copy path') +
            '<div class="dr-fact sec-sigfact"><dt>Signature</dt><dd>' +
              '<div class="sec-sigline">' +
                '<span class="sec-sig" data-r="sigLine">' + DASH + '</span>' +
                `<button type="button" class="btn btn--ghost btn--sm sec-verify" data-act="verify" data-r="verify"><span>Verify signature</span>${icon('i-check')}</button>` +
              '</div>' +
              '<p class="sec-verify-result" data-r="sigResult" role="status" hidden></p>' +
            '</dd></div>' +
            monoFact('cmd', 'Command line', 'Copy command line') +
            monoFact('cwd', 'Working dir', 'Copy working directory') +
            fact('user', 'User') +
            fact('started', 'Started') +
            fact('parent', 'Parent') +
            '<div class="dr-fact"><dt><label for="dr-priority">Priority</label></dt><dd>' +
              `<select class="select dr-select" id="dr-priority" data-r="priority">${prio}<option value="" disabled>Unknown</option></select>` +
            '</dd></div>' +
            fact('threads', 'Threads') +
            fact('handles', 'Handles') +
            fact('memory', 'Memory') +
            `<div class="dr-fact"><dt title="${IO_TITLE}">I/O total</dt><dd data-f="disk"></dd></div>` +
          '</dl>' +
          '<p class="dr-note" data-r="note" hidden></p>' +
        '</section>' +
        '<section class="dr-section" aria-labelledby="dr-kids-h">' +
          '<h3 class="micro dr-h" id="dr-kids-h" data-r="kidsHead">Child processes</h3>' +
          '<ul class="dr-children" data-r="kids"></ul>' +
        '</section>' +
        '<section class="dr-section sec-drawer" aria-label="Modules and environment">' +
          '<details class="sec-more" data-r="modsBox">' +
            `<summary class="sec-summary"><span class="sec-summary-title">Modules</span><span class="sec-summary-count" data-r="modsCount"></span>${icon('i-chevron')}</summary>` +
            '<div class="sec-list" data-r="mods"></div>' +
          '</details>' +
          '<details class="sec-more" data-r="envBox">' +
            `<summary class="sec-summary"><span class="sec-summary-title">Environment</span><span class="sec-summary-count" data-r="envCount"></span>${icon('i-chevron')}</summary>` +
            '<div class="sec-list" data-r="env"></div>' +
          '</details>' +
        '</section>' +
      '</div>' +
      '<footer class="dr-actions" data-r="actions">' +
        `<button type="button" class="btn btn--danger btn--sm" data-act="end"><span>End task</span>${icon('i-stop')}</button>` +
        `<button type="button" class="btn btn--ghost btn--sm" data-act="restart"><span>Restart</span>${icon('i-refresh')}</button>` +
        `<button type="button" class="btn btn--ghost btn--sm" data-act="tree"><span>End tree</span>${icon('i-tree')}</button>` +
        `<button type="button" class="btn btn--ghost btn--sm" data-act="suspend" data-r="suspend"><span>Suspend</span>${icon('i-pause')}</button>` +
        `<button type="button" class="btn btn--ghost btn--sm" data-act="affinity"><span>Set affinity</span>${icon('i-cpu')}</button>` +
        `<button type="button" class="btn btn--ghost btn--sm" data-act="location" data-r="location"><span>Open location</span>${icon('i-folder')}</button>` +
        `<button type="button" class="btn btn--ghost btn--sm" data-act="search"><span>Search online</span>${icon('i-globe')}</button>` +
        `<button type="button" class="btn btn--ghost btn--sm" data-act="copy"><span>Copy details</span>${icon('i-copy')}</button>` +
        `<button type="button" class="btn btn--ghost btn--sm" data-act="pin" data-r="pin" aria-pressed="false"><span>Pin</span>${icon('i-pin')}</button>` +
      '</footer>' +
      '<div class="empty dr-exited" data-r="exited" hidden>' +
        '<p class="micro" data-r="exitedMicro"></p>' +
        '<p class="dr-exited-title">This process has exited.</p>' +
        `<button type="button" class="btn btn--ghost btn--sm" data-act="close"><span>Close</span>${icon('i-close')}</button>` +
      '</div>' +
    '</div>'
  );
}

export function initDetails() {
  const el = document.getElementById('details');
  if (!el || d.el) return;
  d.el = el;
  el.classList.add('drawer', 'proc-drawer');
  el.setAttribute('role', 'dialog');
  el.setAttribute('aria-modal', 'false');
  el.setAttribute('aria-labelledby', 'dr-name');
  el.setAttribute('aria-hidden', 'true');
  el.inert = true;
  el.innerHTML = template();

  const r = {};
  for (const node of el.querySelectorAll('[data-r]')) r[node.dataset.r] = node;
  r.facts = {};
  for (const node of el.querySelectorAll('[data-f]')) r.facts[node.dataset.f] = node;
  r.copies = {};
  for (const node of el.querySelectorAll('[data-copy]')) r.copies[node.dataset.copy] = node;
  d.r = r;

  el.addEventListener('click', onClick);
  r.priority.addEventListener('change', onPriorityChange);
  // 'toggle' does not bubble: listen on each box. Lists load the first time a box opens.
  r.modsBox.addEventListener('toggle', () => { if (r.modsBox.open) loadModules(); });
  r.envBox.addEventListener('toggle', () => { if (r.envBox.open) loadEnvironment(); });
  d.sec = freshSec();
  document.addEventListener('keydown', onKeyDown, true);

  on('select', (data) => {
    const pid = data && typeof data === 'object' ? data.pid : data;
    if (pid == null || pid === '') close();
    else if (Number.isFinite(Number(pid))) openFor(Number(pid));
  });
  // Switching section closes the drawer. Views that open it for a process
  // (Services "Go to process", palette "Details") navigate first, then select.
  on('view', () => {
    if (!d.open) return;
    close(false);
    selectProcess(null);
  });
  on('snapshot', onSnapshot);
  // Minimised: skip the drawer's live patching; catch up when the window shows again.
  document.addEventListener('visibilitychange', () => { if (!document.hidden) onSnapshot(); });
  on('settings', () => {
    if (d.open && !d.exited) patchActions(findRow(d.pid));
  });
}

/* ------------------------------------------------------------------ */
/* Open / close                                                        */
/* ------------------------------------------------------------------ */

function openFor(pid) {
  if (!d.el) return;
  const wasOpen = d.open;
  if (!wasOpen) {
    const a = document.activeElement;
    d.opener = a instanceof HTMLElement && a !== document.body && !d.el.contains(a) ? a : null;
  }
  const row = findRow(pid);
  if (wasOpen && pid === d.pid && !d.exited) return;

  d.pid = pid;
  d.token++;
  d.inflight = false;
  d.details = null;
  d.error = null;
  d.exited = false;
  d.cache = {};
  d.iconExe = undefined;
  d.priority = null;
  d.name = row ? row.name : `PID ${pid}`;
  d.startTime = row ? row.startTime : null;

  d.r.body.hidden = false;
  d.r.actions.hidden = false;
  d.r.exited.hidden = true;
  d.r.body.scrollTop = 0;
  d.r.note.hidden = true;
  resetSecurity();

  patchHeader(row);
  patchLive(row);
  patchFacts();
  patchActions(row);
  loadIcon(row ? row.exe : null, row ? row.kind : null);

  d.open = true;
  d.el.inert = false;
  d.el.removeAttribute('aria-hidden');
  d.el.classList.add('is-open');

  clearInterval(d.timer);
  if (!row && hasSnapshot()) {
    setExited();
  } else {
    d.timer = setInterval(fetchDetails, REFRESH_MS);
    fetchDetails();
  }
  if (!wasOpen) requestAnimationFrame(() => { if (d.open) d.r.close.focus({ preventScroll: true }); });
}

function close(restoreFocus = true) {
  if (!d.el || !d.open) return;
  const hadFocus = d.el.contains(document.activeElement);
  if (!restoreFocus && hadFocus) document.activeElement.blur();
  d.open = false;
  d.token++;
  clearInterval(d.timer);
  d.timer = 0;
  d.el.classList.remove('is-open');
  d.el.setAttribute('aria-hidden', 'true');
  d.el.inert = true;
  if (restoreFocus && (hadFocus || document.activeElement === document.body)) {
    const back = d.opener && d.opener.isConnected ? d.opener : document.querySelector('#view-processes .proc-scroll');
    if (back) back.focus({ preventScroll: true });
  }
  d.opener = null;
  d.pid = null;
}

function setExited() {
  if (d.exited) return;
  d.exited = true;
  clearInterval(d.timer);
  d.timer = 0;
  const focusInside = d.r.body.contains(document.activeElement) || d.r.actions.contains(document.activeElement);
  d.r.body.hidden = true;
  d.r.actions.hidden = true;
  d.r.exited.hidden = false;
  d.r.exitedMicro.textContent = `PID ${d.pid} / Ended`;
  setHtml('chips', d.r.chips, '<span class="chip chip--danger">Exited</span>');
  if (focusInside) d.r.close.focus({ preventScroll: true });
}

function isVisible(node) {
  if (!node || node.hidden || node.getAttribute('aria-hidden') === 'true') return false;
  if (!node.getClientRects().length) return false;
  const cs = getComputedStyle(node);
  return cs.visibility !== 'hidden' && cs.display !== 'none' && Number(cs.opacity) > 0;
}

function onKeyDown(e) {
  if (!d.open || e.key !== 'Escape' || e.defaultPrevented) return;
  const t = e.target;
  if (t instanceof Element && t.closest('#palette, #context-menu, dialog, [aria-modal="true"]')) return;
  // Let an open menu, palette or modal close first; the drawer closes on the next Esc.
  if (isVisible(document.getElementById('context-menu')) || isVisible(document.getElementById('palette'))) return;
  for (const m of document.querySelectorAll('[aria-modal="true"], dialog[open]')) {
    if (m !== d.el && isVisible(m)) return;
  }
  e.preventDefault();
  e.stopPropagation();
  close();
}

/* ------------------------------------------------------------------ */
/* Data                                                                */
/* ------------------------------------------------------------------ */

async function fetchDetails() {
  if (!d.open || d.exited || d.inflight) return;
  const pid = d.pid;
  const tok = d.token;
  d.inflight = true;
  let det = null;
  let err = null;
  try {
    det = await call('get_process_details', { pid });
  } catch (e) {
    err = e;
  }
  if (tok !== d.token) return;
  d.inflight = false;
  if (err) {
    if (hasSnapshot() && !findRow(pid)) {
      setExited();
      return;
    }
    d.error = errText(err);
  } else {
    d.details = det;
    d.error = null;
    if (det && det.exe && d.iconExe !== det.exe && !d.cache.hasIcon) {
      const row = findRow(pid);
      loadIcon(det.exe, row ? row.kind : null);
    }
  }
  patchFacts();
}

function onSnapshot() {
  if (!d.open || d.exited || document.hidden) return;
  const row = findRow(d.pid);
  if (!row) {
    if (hasSnapshot()) setExited();
    return;
  }
  // Same PID, different start time: Windows reused the PID for a new process.
  if (d.startTime && row.startTime && Math.abs(row.startTime - d.startTime) > 1) {
    setExited();
    return;
  }
  if (!d.startTime && row.startTime) d.startTime = row.startTime;
  patchHeader(row);
  patchLive(row);
  patchActions(row);
}

async function loadIcon(exe, kind) {
  const ico = d.r.ico;
  if (!d.cache.hasIcon) {
    const fallback = kind === 'windows' ? 'i-shield' : 'i-grid';
    if (d.cache.icoFallback !== fallback) {
      d.cache.icoFallback = fallback;
      ico.innerHTML = `<svg class="icon"><use href="#${fallback}"/></svg>`;
    }
  }
  d.iconExe = exe || null;
  if (!exe) return;
  const pid = d.pid;
  try {
    const map = await call('get_icons', { paths: [exe] });
    if (d.pid !== pid || !map) return;
    const src = map[exe];
    if (typeof src === 'string' && src.startsWith('data:image/')) {
      const img = document.createElement('img');
      img.alt = '';
      img.width = 28;
      img.height = 28;
      img.decoding = 'async';
      img.src = src;
      ico.replaceChildren(img);
      d.cache.hasIcon = true;
    }
  } catch (_) { /* keep the generic icon */ }
}

/* ------------------------------------------------------------------ */
/* Patching                                                            */
/* ------------------------------------------------------------------ */

function patchHeader(row) {
  const r = d.r;
  const name = row ? row.name : (d.details && d.details.name) || d.name;
  d.name = name;
  set('name', r.name, name);
  if (r.name.title !== name) r.name.title = name;
  set('micro', r.micro, `Details / PID ${d.pid}`);
  const sub = row && row.windowTitle ? row.windowTitle : '';
  set('sub', r.sub, sub);
  if (r.sub.hidden === !!sub) r.sub.hidden = !sub;
  if (!row) return;
  const status = statusLabel(row);
  const tone = row.notResponding ? 'chip--warn' : row.suspended ? '' : 'chip--live';
  const pinned = isPinned(row);
  const html =
    `<span class="chip">${escapeHtml(kindLabel(row.kind))}</span>` +
    `<span class="chip ${tone}">${escapeHtml(status)}</span>` +
    (pinned ? `<span class="chip chip--on">${icon('i-pin')}Pinned</span>` : '');
  setHtml('chips', r.chips, html);
}

function peak(arr, floor) {
  let m = floor;
  for (let i = 0; i < arr.length; i++) if (arr[i] > m) m = arr[i];
  return m;
}

function patchLive(row) {
  const r = d.r;
  // The history is 60 samples, so its span follows the update speed.
  set('liveH', r.liveH, `Live / ${historySpan(store.settings && store.settings.intervalMs)}`);
  if (row) {
    set('cpuVal', r.cpuVal, pct(row.cpu || 0, 1));
    set('memVal', r.memVal, bytes(row.memory || 0));
  }
  const hist = store.history && store.history.proc && store.history.proc.get(d.pid);
  const cpu = hist && Array.isArray(hist.cpu) && hist.cpu.length ? hist.cpu : row ? [row.cpu || 0] : [];
  const mem = hist && Array.isArray(hist.mem) && hist.mem.length ? hist.mem : row ? [row.memory || 0] : [];
  if (!d.open && !row) return;
  try {
    sparkline(r.cpuCanvas, cpu, { color: '--chart-cpu', max: Math.min(100, Math.max(5, peak(cpu, 0) * 1.25)) });
    sparkline(r.memCanvas, mem, { color: '--chart-mem', max: Math.max(64 * 1024 * 1024, peak(mem, 0) * 1.2) });
  } catch (_) { /* a chart failure must not break the drawer */ }
}

function setFact(key, value) {
  const node = d.r.facts[key];
  if (node) set(`f:${key}`, node, value);
}

function setCopy(key, value) {
  const btn = d.r.copies[key];
  if (!btn) return;
  const on = !!value;
  if (btn.disabled === on) btn.disabled = !on;
}

function patchFacts() {
  if (!d.r) return;
  const det = d.details;
  const row = findRow(d.pid);
  const exe = (det && det.exe) || (row && row.exe) || null;
  const cmd = (det && det.cmd) || (row && row.cmd) || '';
  const cwd = (det && det.cwd) || null;
  const user = (det && det.user) || (row && row.user) || null;
  const start = (det && det.startTime) || (row && row.startTime) || 0;

  setFact('path', exe || 'Access denied');
  setCopy('path', exe);
  patchSignature(exe);
  setFact('cmd', cmd || DASH);
  setCopy('cmd', cmd);
  setFact('cwd', cwd || DASH);
  setCopy('cwd', cwd);
  setFact('user', user || DASH);
  if (start > 0) {
    const ago = Math.max(0, Math.floor(Date.now() / 1000 - start));
    setFact('started', `${time(start)} / ${duration(ago)} ago`);
  } else {
    setFact('started', DASH);
  }

  // Parent
  const ppid = det ? det.parentPid : row ? row.parentPid : null;
  let parentHtml;
  if (ppid == null) {
    parentHtml = 'None';
  } else {
    const live = findRow(ppid);
    const pname = (det && det.parentName) || (live && live.name) || 'Unknown';
    parentHtml = live
      ? `<button type="button" class="dr-link" data-pid="${ppid}">${escapeHtml(pname)} <span class="dr-pid">${ppid}</span></button>`
      : `${escapeHtml(pname)} <span class="dr-pid">${ppid}</span> <span class="dr-dim">(exited)</span>`;
  }
  setHtml('f:parent', d.r.facts.parent, parentHtml);

  // Priority select (do not fight the user while it has focus)
  const sel = d.r.priority;
  const prio = det && det.priority ? det.priority : null;
  if (document.activeElement !== sel) {
    const v = prio && PRIORITIES.some((p) => p.value === prio) ? prio : '';
    if (sel.value !== v) sel.value = v;
    d.priority = v || null;
  }
  const canSet = !!det;
  if (sel.disabled === canSet) sel.disabled = !canSet;

  setFact('threads', det && det.threads != null ? String(det.threads) : DASH);
  setFact('handles', det && det.handles != null ? String(det.handles) : DASH);
  if (det) {
    setFact('memory', `${bytes(det.memory || 0)} / ${bytes(det.virtualMemory || 0)} virtual`);
    setFact('disk', `${bytes(det.diskTotalRead || 0)} read / ${bytes(det.diskTotalWritten || 0)} written`);
  } else {
    setFact('memory', row ? bytes(row.memory || 0) : DASH);
    setFact('disk', DASH);
  }

  // Note on errors (usually access denied)
  const note = d.error ? `Some details are hidden. ${d.error}` : '';
  set('note', d.r.note, note);
  if (d.r.note.hidden === !!note) d.r.note.hidden = !note;

  patchChildren(det ? det.children : null);
}

function patchChildren(children) {
  const r = d.r;
  const list = Array.isArray(children) ? children : null;
  const key = list ? list.map((c) => `${c.pid}:${c.name}`).join('|') : '?';
  if (d.cache.kids === key) return;
  d.cache.kids = key;
  if (!list) {
    set('kidsHead', r.kidsHead, 'Child processes');
    r.kids.innerHTML = `<li class="dr-kids-empty">${d.error ? 'Hidden' : 'Loading'}</li>`;
    return;
  }
  set('kidsHead', r.kidsHead, `Child processes (${list.length})`);
  if (!list.length) {
    r.kids.innerHTML = '<li class="dr-kids-empty">No child processes.</li>';
    return;
  }
  const shown = list.slice(0, MAX_CHILDREN);
  let html = '';
  for (const c of shown) {
    html += `<li><button type="button" class="dr-child" data-pid="${Number(c.pid)}">` +
      `<span class="dr-child-name">${escapeHtml(c.name || 'Unknown')}</span>` +
      `<span class="dr-pid">${Number(c.pid)}</span></button></li>`;
  }
  if (list.length > shown.length) html += `<li class="dr-kids-empty">And ${list.length - shown.length} more.</li>`;
  const focused = document.activeElement;
  const focusedPid = focused && r.kids.contains(focused) ? focused.getAttribute('data-pid') : null;
  r.kids.innerHTML = html;
  if (focusedPid) {
    const again = r.kids.querySelector(`[data-pid="${focusedPid}"]`);
    if (again) again.focus({ preventScroll: true });
  }
}

function patchActions(row) {
  const r = d.r;
  if (!r) return;
  const suspended = !!(row && row.suspended);
  setHtml('suspend', r.suspend, suspended ? `<span>Resume</span>${icon('i-play')}` : `<span>Suspend</span>${icon('i-pause')}`);
  const exe = (row && row.exe) || (d.details && d.details.exe) || null;
  if (r.location.disabled === !!exe) r.location.disabled = !exe;
  const pinned = !!row && isPinned(row);
  setHtml('pin', r.pin, pinned ? `<span>Unpin</span>${icon('i-pin')}` : `<span>Pin</span>${icon('i-pin')}`);
  const pressed = pinned ? 'true' : 'false';
  if (r.pin.getAttribute('aria-pressed') !== pressed) r.pin.setAttribute('aria-pressed', pressed);
}

/* ------------------------------------------------------------------ */
/* Interaction                                                         */
/* ------------------------------------------------------------------ */

function actionRow() {
  const row = findRow(d.pid);
  if (row) return row;
  const det = d.details;
  if (!det) return null;
  return {
    pid: det.pid, parentPid: det.parentPid, name: det.name, exe: det.exe, cmd: det.cmd,
    user: det.user, cpu: 0, memory: det.memory || 0, diskReadBps: 0, diskWriteBps: 0,
    gpu: null, status: 'unknown', startTime: det.startTime, kind: 'background',
    windowTitle: null, notResponding: false, suspended: false,
  };
}

async function onPriorityChange() {
  const sel = d.r.priority;
  const value = sel.value;
  const pid = d.pid;
  if (!value || pid == null) return;
  const prev = d.priority;
  const ok = await setPriority(pid, value, d.startTime);
  if (d.pid !== pid) return;
  if (ok) {
    d.priority = value;
    if (d.details) d.details.priority = value;
  } else {
    sel.value = prev || '';
  }
}

async function onClick(e) {
  const t = e.target instanceof Element ? e.target : null;
  if (!t) return;
  const pidBtn = t.closest('[data-pid]');
  if (pidBtn && d.el.contains(pidBtn)) {
    const pid = Number(pidBtn.getAttribute('data-pid'));
    if (Number.isFinite(pid)) selectProcess(pid);
    return;
  }
  const envBtn = t.closest('[data-env-copy]');
  if (envBtn) {
    const v = d.sec && Array.isArray(d.sec.env) ? d.sec.env[Number(envBtn.dataset.envCopy)] : null;
    if (!v) return;
    const ok = await copyText(`${v.name}=${v.value}`);
    toast(ok ? `Copied ${v.name}.` : 'Could not copy to the clipboard.', ok ? 'success' : 'error');
    return;
  }
  const copyBtn = t.closest('[data-copy]');
  if (copyBtn) {
    const k = copyBtn.dataset.copy;
    const det = d.details;
    const row = findRow(d.pid);
    const value = k === 'path' ? (det && det.exe) || (row && row.exe)
      : k === 'cmd' ? (det && det.cmd) || (row && row.cmd)
        : det && det.cwd;
    if (!value) return;
    const ok = await copyText(value);
    const what = k === 'path' ? 'path' : k === 'cmd' ? 'command line' : 'working directory';
    toast(ok ? `Copied ${what}.` : 'Could not copy to the clipboard.', ok ? 'success' : 'error');
    return;
  }
  const actBtn = t.closest('[data-act]');
  if (!actBtn) return;
  const act = actBtn.dataset.act;
  if (act === 'close') {
    close();
    return;
  }
  if (act === 'verify') {
    const exe = currentExe();
    if (exe) loadSignature(exe, true);
    return;
  }
  if (act === 'sec-retry') {
    if (actBtn.dataset.which === 'mods') loadModules();
    else loadEnvironment();
    return;
  }
  const row = actionRow();
  if (!row) return;
  switch (act) {
    case 'end':
      if (await endTask(row.pid, row.name, d.startTime || row.startTime)) fetchSoon();
      break;
    case 'restart': {
      const pid = d.pid;
      const newPid = await restartProcess(row.pid, row.name, d.startTime || row.startTime);
      // Follow the restarted process once a snapshot lists it.
      if (newPid != null && (await whenListed(newPid)) && d.open && d.pid === pid) selectProcess(newPid);
      break;
    }
    case 'tree':
      if (await endTree(row.pid, row.name, d.startTime || row.startTime)) fetchSoon();
      break;
    case 'suspend':
      await toggleSuspend(row);
      break;
    case 'affinity':
      await setAffinity(row);
      break;
    case 'location':
      if (!row.exe) {
        toast('The file location is hidden. Try Restart as administrator.', 'error');
        break;
      }
      try { await call('open_location', { path: row.exe }); } catch (err) { toast(errText(err), 'error'); }
      break;
    case 'search':
      try { await call('search_online', { query: row.name }); } catch (err) { toast(errText(err), 'error'); }
      break;
    case 'copy':
      await copyDetails(row);
      break;
    case 'pin':
      togglePin(row);
      patchActions(findRow(d.pid));
      patchHeader(findRow(d.pid));
      break;
    default:
      break;
  }
}

/* ------------------------------------------------------------------ */
/* Security: signature, modules, environment                           */
/* ------------------------------------------------------------------ */

function currentExe() {
  const row = findRow(d.pid);
  return (d.details && d.details.exe) || (row && row.exe) || null;
}

function resetSecurity() {
  const r = d.r;
  d.sec = freshSec();
  r.modsBox.open = false;
  r.envBox.open = false;
  r.mods.innerHTML = '';
  r.env.innerHTML = '';
  r.modsCount.textContent = '';
  r.envCount.textContent = '';
  r.sigLine.textContent = DASH;
  r.sigResult.hidden = true;
  r.sigResult.textContent = '';
  r.verify.disabled = true;
}

function patchSignature(exe) {
  const s = d.sec;
  if (!s || !d.r) return;
  if (!exe) {
    if (!s.sigExe && !s.sigBusy) {
      d.r.sigLine.textContent = 'No file to check';
      d.r.verify.disabled = true;
    }
    return;
  }
  if (s.sigExe !== exe) loadSignature(exe, false);
}

function sigLineHtml(info) {
  const pub = info.publisher ? `<span class="sec-sig-pub">${escapeHtml(info.publisher)}</span>` : '';
  switch (info.status) {
    case 'signed':
      return `<span class="chip chip--sm chip--ok">Signed</span>${pub || '<span class="sec-sig-pub">Unknown publisher</span>'}`;
    case 'unsigned':
      return '<span class="chip chip--sm chip--warn">Unsigned</span>';
    case 'invalid':
      return `<span class="chip chip--sm chip--danger">Invalid</span>${pub}`;
    default:
      return '<span class="chip chip--sm">Unknown</span><span class="sec-sig-pub">Orbit could not read the file.</span>';
  }
}

function verdict(info) {
  const at = new Date().toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' });
  switch (info.status) {
    case 'signed':
      return `Valid signature from ${info.publisher || 'an unknown publisher'}. Checked at ${at}.`;
    case 'unsigned':
      return `No signature. Anyone could have made this file, so make sure you trust where it came from. Checked at ${at}.`;
    case 'invalid':
      return `The signature is not valid. The file may have changed after it was signed, or Windows does not trust the certificate. Checked at ${at}.`;
    default:
      return 'Orbit could not read this file to check it. Try Restart as administrator.';
  }
}

async function loadSignature(exe, verify) {
  const s = d.sec;
  const r = d.r;
  const tok = d.token;
  s.sigExe = exe;
  s.sigBusy = true;
  r.verify.disabled = true;
  if (!s.sig) r.sigLine.textContent = 'Checking';
  if (verify) {
    r.sigResult.hidden = false;
    r.sigResult.textContent = 'Checking the signature.';
  }
  let info = null;
  let err = null;
  try {
    const map = await call('get_signatures', { paths: [exe] });
    info = (map && map[exe]) || { status: 'unknown', publisher: null };
  } catch (e) {
    err = errText(e);
  }
  if (tok !== d.token || d.sec !== s) return;
  s.sigBusy = false;
  r.verify.disabled = false;
  if (info) {
    s.sig = info;
    r.sigLine.innerHTML = sigLineHtml(info);
  } else if (!s.sig) {
    r.sigLine.textContent = 'Could not check';
  }
  if (verify) r.sigResult.textContent = err || verdict(info);
}

function listMessage(msg, which) {
  const retry = which
    ? `<button type="button" class="btn btn--ghost btn--sm" data-act="sec-retry" data-which="${which}"><span>Try again</span>${icon('i-refresh')}</button>`
    : '';
  return `<div class="sec-list-msg"><p>${escapeHtml(msg)}</p>${retry}</div>`;
}

function splitPath(path) {
  const p = String(path || '');
  const i = p.lastIndexOf('\\');
  return i < 0 ? { name: p, dir: '' } : { name: p.slice(i + 1), dir: p.slice(0, i) };
}

async function loadModules() {
  const s = d.sec;
  if (!s || s.mods || s.modsBusy || d.pid == null || d.exited) return;
  const tok = d.token;
  s.modsBusy = true;
  d.r.mods.innerHTML = listMessage('Loading modules.');
  try {
    const list = await call('get_process_modules', { pid: d.pid, startTime: d.startTime || undefined });
    if (tok !== d.token) return;
    s.mods = Array.isArray(list) ? list : [];
    s.modsErr = null;
  } catch (e) {
    if (tok !== d.token) return;
    s.modsErr = errText(e);
  }
  s.modsBusy = false;
  const r = d.r;
  if (!s.mods) {
    r.modsCount.textContent = '';
    r.mods.innerHTML = listMessage(s.modsErr, 'mods');
    return;
  }
  r.modsCount.textContent = String(s.mods.length);
  if (!s.mods.length) {
    r.mods.innerHTML = listMessage('No modules listed.');
    return;
  }
  const shown = s.mods.slice(0, MAX_LIST_ITEMS);
  let html = '<ul class="sec-items">';
  for (const m of shown) {
    const { name, dir } = splitPath(m.path);
    html += `<li class="sec-item" title="${escapeHtml(m.path)}"><span class="sec-item-main">` +
      `<span class="sec-item-name">${escapeHtml(name)}</span>` +
      (dir ? `<span class="sec-item-sub">${escapeHtml(dir)}</span>` : '') +
      `</span><span class="sec-item-side">${m.size ? bytes(m.size) : DASH}</span></li>`;
  }
  if (s.mods.length > shown.length) html += `<li class="sec-item sec-item--more">And ${s.mods.length - shown.length} more.</li>`;
  r.mods.innerHTML = `${html}</ul>`;
}

async function loadEnvironment() {
  const s = d.sec;
  if (!s || s.env || s.envBusy || d.pid == null || d.exited) return;
  const tok = d.token;
  s.envBusy = true;
  d.r.env.innerHTML = listMessage('Loading environment.');
  try {
    const list = await call('get_process_environment', { pid: d.pid });
    if (tok !== d.token) return;
    s.env = Array.isArray(list) ? list : [];
    s.envErr = null;
  } catch (e) {
    if (tok !== d.token) return;
    s.envErr = errText(e);
  }
  s.envBusy = false;
  const r = d.r;
  if (!s.env) {
    r.envCount.textContent = '';
    r.env.innerHTML = listMessage(s.envErr, 'env');
    return;
  }
  r.envCount.textContent = String(s.env.length);
  if (!s.env.length) {
    r.env.innerHTML = listMessage('No environment variables.');
    return;
  }
  let html = '<ul class="sec-items">';
  s.env.slice(0, MAX_LIST_ITEMS).forEach((v, i) => {
    const name = escapeHtml(v.name);
    html += `<li class="sec-item"><span class="sec-item-main">` +
      `<span class="sec-item-name">${name}</span>` +
      `<span class="sec-item-sub sec-item-value">${v.value ? escapeHtml(v.value) : '(empty)'}</span>` +
      `</span><button type="button" class="icon-btn dr-copy" data-env-copy="${i}" aria-label="Copy ${name}" title="Copy ${name}=value">${icon('i-copy')}</button></li>`;
  });
  r.env.innerHTML = `${html}</ul>`;
}

function fetchSoon() {
  const tok = d.token;
  setTimeout(() => {
    if (tok === d.token && d.open && !d.exited) fetchDetails();
  }, 400);
}
