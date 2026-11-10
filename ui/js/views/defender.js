// 08 / Security > Malware scan: Microsoft Defender's status and scans (defender.rs).
//
// Orbit ships no antivirus engine; it shows what Defender says and asks it to scan.
// The panel sits at the top of the Security view (mountDefender / showDefender). The
// shared actions are used elsewhere too: scanWithDefender(path, name) for the Processes
// and History menus (defenderMenuItem(path) builds the item), and the palette commands.
//
// One scan runs at a time. `defender_scan` resolves with the result; while it runs the
// backend sends orbit://defender-progress (state "running" + elapsedSecs) about once a
// second. Threats are never removed here: "Open Windows Security" shows them where
// Defender handles them.

import { call, onEvent } from '../api.js';
import { escapeHtml } from '../format.js';
import { toast, navigate, confirmDialog, showMenu, restartAsAdmin, currentViewName } from '../app.js';
import { registerCommands } from '../palette.js';

const EVENT = 'orbit://defender-progress';
const STALE_MS = 30000;
const KIND_LABEL = { quick: 'Quick scan', full: 'Full scan', custom: 'Custom scan' };
const LEDE = 'Scans use Microsoft Defender, built into Windows.';

let status = null; // DefenderStatus, or null before the first read
let statusAt = 0;
let statusError = null;
let loading = null; // Promise while a status read runs
let scan = null; // DefenderScan: the running one, else the last one
let listening = false;
const listeners = new Set();
let unregister = null;

let host = null; // the panel element
const els = {};

const icon = (id) => `<svg class="icon" aria-hidden="true"><use href="#${id}"/></svg>`;
const text = (s) => escapeHtml(s == null ? '' : String(s));

function errText(err) {
  if (typeof err === 'string' && err) return err;
  if (err && typeof err.message === 'string' && err.message) return err.message;
  return 'Something went wrong. Try again.';
}

/** "C:\\x\\y\\setup.exe" -> "setup.exe"; a drive root stays "C:\\". */
export function baseName(path) {
  const p = String(path || '').replace(/[\\/]+$/, '');
  if (/^[a-z]:$/i.test(p)) return `${p}\\`;
  const i = Math.max(p.lastIndexOf('\\'), p.lastIndexOf('/'));
  return i >= 0 ? p.slice(i + 1) : p;
}

/** 0:07, 4:12, 1:02:09. */
export function elapsedText(secs) {
  const s = Math.max(0, Math.floor(Number(secs) || 0));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const ss = String(s % 60).padStart(2, '0');
  return h ? `${h}:${String(m).padStart(2, '0')}:${ss}` : `${m}:${ss}`;
}

/** Unix seconds -> "Today, 10:22", "Yesterday, 10:22", "3 days ago", "Never". */
export function whenText(unix, now = Date.now()) {
  if (!unix) return 'Never';
  const d = new Date(unix * 1000);
  const day = (x) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const days = Math.round((day(new Date(now)) - day(d)) / 86400000);
  const hm = d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  if (days <= 0) return `Today, ${hm}`;
  if (days === 1) return `Yesterday, ${hm}`;
  return `${days} days ago`;
}

function notify() {
  for (const fn of listeners) {
    try { fn(); } catch (_) { /* a listener's error must not stop the others */ }
  }
  render();
}

function ensureListening() {
  if (listening) return;
  listening = true;
  onEvent(EVENT, (p) => {
    if (!p || typeof p !== 'object') return;
    if (scan && scan.id && p.id && p.id < scan.id) return; // an older scan's late event
    scan = p;
    notify();
  });
}

/** Subscribe to status / scan changes. Returns an unsubscribe function. */
export function onDefender(fn) {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

export function defenderStatus() {
  return status;
}

export function currentScan() {
  return scan;
}

/** Read Defender's status (cached for 30 s unless `force`). */
export async function loadStatus(force = false) {
  ensureListening();
  if (loading) return loading;
  if (!force && status && Date.now() - statusAt < STALE_MS) return status;
  loading = (async () => {
    try {
      const s = await call('defender_status');
      status = s;
      statusAt = Date.now();
      statusError = null;
      // A scan this page did not start (another window, or before a reload).
      if (s && s.scan && (!scan || (s.scan.id || 0) >= (scan.id || 0))) scan = s.scan;
    } catch (err) {
      statusError = errText(err);
    } finally {
      loading = null;
    }
    notify();
    return status;
  })();
  return loading;
}

/** Why scans are unavailable, in one sentence. */
function blockedText(s) {
  if (s.other) return `Microsoft Defender is off because ${s.other.name} is your antivirus.`;
  return 'Microsoft Defender is off, so it can’t scan. Turn it on in Windows Security.';
}

function running() {
  return !!(scan && scan.state === 'running');
}

// ------------------------------------------------------------------ actions

/**
 * Start a scan and wait for it. kind: 'quick' | 'full' | 'custom' (with path).
 * Returns the DefenderScan result, or null when it could not start.
 */
export async function startScan(kind, path = null) {
  ensureListening();
  if (running()) {
    toast('A Defender scan is already running.', 'warn');
    return null;
  }
  const before = scan;
  scan = { id: 0, kind, target: path, state: 'running', startedAt: Math.floor(Date.now() / 1000), elapsedSecs: 0, threats: [], message: null, needsAdmin: false, exitCode: null };
  notify();
  let res = null;
  try {
    res = await call('defender_scan', path ? { kind, path } : { kind });
    if (!scan || !scan.id || (res && res.id >= scan.id)) scan = res;
  } catch (err) {
    const msg = errText(err);
    if (/already running/i.test(msg)) {
      scan = before;
      toast(msg, 'warn');
      loadStatus(true);
      return null;
    }
    scan = { ...scan, state: 'failed', message: msg };
    res = scan;
  }
  notify();
  loadStatus(true); // last quick / full scan times
  return res;
}

export async function cancelScan() {
  try {
    await call('defender_cancel');
  } catch (err) {
    toast(errText(err), 'error');
  }
}

export async function openWindowsSecurity(page = 'home') {
  try {
    await call('open_windows_security', { page });
  } catch (err) {
    toast(errText(err), 'error');
  }
}

function resultToast(res, label) {
  if (!res) return;
  const where = label ? ` in ${label}` : '';
  switch (res.state) {
    case 'clean':
      toast(`No threats found${where}.`, 'success');
      break;
    case 'threats': {
      const n = res.threats ? res.threats.length : 0;
      const what = n ? `${n} threat${n === 1 ? '' : 's'}` : 'threats';
      toast(`Defender found ${what}${where}.`, 'error', { label: 'Open Windows Security', run: () => openWindowsSecurity('threats') });
      break;
    }
    case 'cancelled':
      toast('Scan cancelled.', 'info');
      break;
    default:
      toast(res.message || 'Defender could not finish the scan.', 'error',
        res.needsAdmin ? { label: 'Restart as administrator', run: () => restartAsAdmin() } : null);
  }
}

/** Scan one file or folder (Processes and History menus). */
export async function scanWithDefender(path, name) {
  if (!path) {
    toast('The file location is hidden. Try Restart as administrator.', 'error');
    return null;
  }
  const label = name || baseName(path);
  const st = status || await loadStatus();
  if (st && !st.canScan) {
    toast(blockedText(st), 'warn');
    return null;
  }
  if (running()) {
    toast('A Defender scan is already running.', 'warn', { label: 'Show', run: () => navigate('security') });
    return null;
  }
  toast(`Scanning ${label} with Microsoft Defender.`, 'info');
  const res = await startScan('custom', path);
  if (res) resultToast(res, label);
  return res;
}

/** The "Scan with Defender" menu item for a program file. */
export function defenderMenuItem(path) {
  if (!status && !loading) loadStatus();
  const blocked = !!(status && !status.canScan);
  return {
    label: 'Scan with Defender',
    icon: 'i-shield',
    disabled: !path || blocked,
    hint: blocked ? 'Defender is off' : undefined,
    action: () => scanWithDefender(path, baseName(path)),
  };
}

async function quickScan() {
  const res = await startScan('quick');
  if (res && currentViewName() !== 'security') resultToast(res, '');
}

async function fullScan() {
  const ok = await confirmDialog({
    title: 'Run a full scan?',
    body: 'Defender checks every file on the PC. It can take an hour or more, and you can keep working.',
    confirmLabel: 'Start full scan',
  });
  if (!ok) return;
  const res = await startScan('full');
  if (res && currentViewName() !== 'security') resultToast(res, '');
}

async function pickAndScan(folder) {
  let path = null;
  try {
    path = await call('defender_pick_target', { folder });
  } catch (err) {
    toast(errText(err), 'error');
    return;
  }
  if (!path) return;
  const res = await startScan('custom', path);
  if (res && currentViewName() !== 'security') resultToast(res, baseName(path));
}

function openPickMenu(btn) {
  const r = btn.getBoundingClientRect();
  showMenu(r.left, r.bottom + 4, [
    { label: 'A file…', icon: 'i-file', action: () => pickAndScan(false) },
    { label: 'A folder…', icon: 'i-folder', action: () => pickAndScan(true) },
  ]);
}

// ------------------------------------------------------------------ render

function fact(label, value, sub, tone) {
  return `<div class="df-fact${tone ? ` df-fact--${tone}` : ''}">
    <dt class="micro">${text(label)}</dt>
    <dd>${text(value)}${sub ? `<span class="df-fact__sub">${text(sub)}</span>` : ''}</dd>
  </div>`;
}

function defsTone(s) {
  if (s.signaturesOutOfDate || (s.signatureAgeDays != null && s.signatureAgeDays >= 7)) return 'warn';
  return '';
}

function defsText(s) {
  if (s.signaturesOutOfDate) return 'Out of date';
  const d = s.signatureAgeDays;
  if (d == null) return s.signatureUpdated ? `Updated ${whenText(s.signatureUpdated).toLowerCase()}` : 'Unknown';
  return d === 0 ? 'Up to date' : `${d} day${d === 1 ? '' : 's'} old`;
}

/** { tone, title, lede, facts, acts } for the current status. */
function view() {
  const s = status;
  if (!s) {
    if (statusError) {
      return { tone: 'warn', title: 'Orbit could not read Defender’s status', lede: statusError, facts: '', acts: 'retry' };
    }
    return { tone: '', title: 'Checking Microsoft Defender', lede: LEDE, facts: '', acts: '' };
  }
  if (!s.active && s.other) {
    const o = s.other;
    return {
      tone: '',
      title: `${o.name} protects this PC`,
      lede: blockedText(s),
      facts: fact('Antivirus', o.name)
        + fact('Status', o.state === 'on' ? 'On' : o.state === 'snoozed' ? 'Snoozed' : o.state === 'expired' ? 'Expired' : 'Off', null, o.state === 'on' ? '' : 'warn')
        + fact('Definitions', o.upToDate ? 'Up to date' : 'Out of date', null, o.upToDate ? '' : 'warn')
        + fact('Microsoft Defender', s.runningMode && /passive/i.test(s.runningMode) ? 'Passive' : 'Off'),
      acts: 'security',
    };
  }
  if (!s.active) {
    return {
      tone: 'danger',
      title: 'Microsoft Defender is off',
      lede: s.available || !s.error ? 'No antivirus is protecting this PC. Turn Defender on in Windows Security.' : s.error,
      facts: '',
      acts: 'security',
    };
  }
  const rt = s.realTime;
  const version = s.productVersion || s.engineVersion;
  return {
    tone: rt === false ? 'warn' : '',
    title: rt === false ? 'Real-time protection is off' : 'Microsoft Defender is on',
    lede: LEDE,
    facts: fact('Real-time protection', rt == null ? 'Unknown' : rt ? 'On' : 'Off', null, rt === false ? 'warn' : '')
      + fact('Definitions', defsText(s), s.signatureVersion, defsTone(s))
      + fact('Last quick scan', whenText(s.quickScanAt))
      + fact('Last full scan', whenText(s.fullScanAt))
      + (version ? fact('Defender', version, s.engineVersion && s.productVersion ? `Engine ${s.engineVersion}` : null) : ''),
    acts: s.canScan ? 'scan' : 'security',
  };
}

function actsHtml(kind) {
  const busy = running();
  if (kind === 'scan') {
    const off = busy ? ' disabled' : '';
    return `<button type="button" class="btn btn--primary btn--sm" data-df-scan="quick"${off}>Quick scan${icon('i-radar')}</button>
      <button type="button" class="btn btn--ghost btn--sm" data-df-scan="full"${off}>Full scan</button>
      <button type="button" class="btn btn--ghost btn--sm" data-df-scan="pick"${off}>Scan a file or folder…</button>`;
  }
  if (kind === 'security') return `<button type="button" class="btn btn--ghost btn--sm" data-df-act="security">Open Windows Security${icon('i-arrow-right')}</button>`;
  if (kind === 'retry') return `<button type="button" class="btn btn--ghost btn--sm" data-df-act="retry">Try again${icon('i-refresh')}</button>`;
  return '';
}

function scanLabel(sc) {
  if (sc.kind === 'custom') return sc.target ? baseName(sc.target) : 'the file';
  return KIND_LABEL[sc.kind] || 'Scan';
}

function runHtml() {
  const sc = scan;
  if (!sc) return '';
  const name = scanLabel(sc);
  const target = sc.kind === 'custom' && sc.target ? ` title="${text(sc.target)}"` : '';
  const where = sc.kind === 'custom' ? ` in <strong${target}>${text(name)}</strong>` : '';
  const took = sc.elapsedSecs ? ` ${text(KIND_LABEL[sc.kind] || 'Scan')}, ${elapsedText(sc.elapsedSecs)}.` : '';
  switch (sc.state) {
    case 'running': {
      const what = sc.kind === 'custom' ? `Scanning <strong${target}>${text(name)}</strong>` : `${text(name)} running`;
      return `<div class="df-run df-run--running" data-state="running" role="status">
        <span class="df-spin" aria-hidden="true">${icon('i-refresh')}</span>
        <p class="df-run__text">${what} <span class="df-run__time" data-df-elapsed>${elapsedText(sc.elapsedSecs)}</span></p>
        <button type="button" class="btn btn--ghost btn--sm" data-df-act="cancel">Cancel</button>
      </div>`;
    }
    case 'clean':
      return `<div class="df-run df-run--clean" data-state="clean" role="status">
        ${icon('i-check')}<p class="df-run__text">No threats found${where}.${took}</p>
      </div>`;
    case 'threats': {
      const n = sc.threats ? sc.threats.length : 0;
      const what = n ? `${n} threat${n === 1 ? '' : 's'}` : 'threats';
      const chips = (sc.threats || []).map((t) => `<span class="chip chip--danger df-threat">${text(t)}</span>`).join('');
      return `<div class="df-run df-run--threats" data-state="threats" role="alert">
        ${icon('i-shield')}
        <div class="df-run__body">
          <p class="df-run__text">Defender found ${what}${where}. Orbit doesn’t remove anything; handle them in Windows Security.</p>
          ${chips ? `<div class="df-threats">${chips}</div>` : ''}
        </div>
        <button type="button" class="btn btn--danger btn--sm" data-df-act="threats">Open Windows Security${icon('i-arrow-right')}</button>
      </div>`;
    }
    case 'cancelled':
      return `<div class="df-run df-run--cancelled" data-state="cancelled" role="status">
        ${icon('i-x')}<p class="df-run__text">${text(name)} cancelled.</p>
      </div>`;
    default:
      return `<div class="df-run df-run--failed" data-state="failed" role="alert">
        ${icon('i-x')}<p class="df-run__text">${text(sc.message || 'Defender could not finish the scan.')}</p>
        ${sc.needsAdmin ? `<button type="button" class="btn btn--ghost btn--sm" data-df-act="admin">Restart as administrator${icon('i-shield')}</button>` : ''}
      </div>`;
  }
}

let lastKey = '';

function render() {
  if (!host) return;
  // While a scan runs only the elapsed time changes each second: patch it in place.
  if (running() && els.elapsed && lastKey === renderKey()) {
    els.elapsed.textContent = elapsedText(scan.elapsedSecs);
    return;
  }
  lastKey = renderKey();
  const v = view();
  // Toggle, not overwrite: motion.js adds .enter-up to the panel on view switches.
  host.classList.add('panel', 'df');
  host.classList.toggle('df--warn', v.tone === 'warn');
  host.classList.toggle('df--danger', v.tone === 'danger');
  host.innerHTML = `
    <div class="df-head">
      <span class="df-icon" aria-hidden="true">${icon('i-shield')}</span>
      <div class="df-headtext">
        <p class="micro">Malware scan</p>
        <h2 class="df-title">${text(v.title)}</h2>
        <p class="df-lede">${text(v.lede)}</p>
      </div>
      <div class="df-acts">${actsHtml(v.acts)}</div>
    </div>
    ${v.facts ? `<dl class="df-facts">${v.facts}</dl>` : ''}
    ${v.acts === 'scan' || (scan && scan.state === 'running') ? runHtml() : ''}`;
  els.elapsed = host.querySelector('[data-df-elapsed]');
}

function renderKey() {
  const sc = scan || {};
  return JSON.stringify([status, statusError, sc.id, sc.state, sc.kind, sc.target, sc.message, sc.threats]);
}

function onClick(e) {
  const t = e.target instanceof Element ? e.target : null;
  const scanBtn = t && t.closest('[data-df-scan]');
  if (scanBtn && !scanBtn.disabled) {
    const k = scanBtn.dataset.dfScan;
    if (k === 'quick') quickScan();
    else if (k === 'full') fullScan();
    else openPickMenu(scanBtn);
    return;
  }
  const act = t && t.closest('[data-df-act]');
  if (!act) return;
  switch (act.dataset.dfAct) {
    case 'cancel': cancelScan(); break;
    case 'threats': openWindowsSecurity('threats'); break;
    case 'security': openWindowsSecurity('home'); break;
    case 'admin': restartAsAdmin(); break;
    case 'retry': loadStatus(true); break;
    default: break;
  }
}

// ------------------------------------------------------------------ palette

function paletteItems() {
  const can = !status || status.canScan;
  if (!can) return [];
  const go = (fn) => () => {
    navigate('security');
    fn();
  };
  return [
    { label: 'Quick scan with Defender', search: 'defender quick scan malware virus antivirus', icon: 'i-shield', hint: running() ? 'Running' : '', run: go(quickScan) },
    { label: 'Full scan with Defender', search: 'defender full scan malware virus antivirus', icon: 'i-shield', run: go(fullScan) },
    { label: 'Scan a file with Defender', search: 'defender scan file folder malware virus antivirus', icon: 'i-shield', run: go(() => pickAndScan(false)) },
  ];
}

// ------------------------------------------------------------------ view API

/** Render the panel into `el` (once, from the Security view's mount). */
export function mountDefender(el) {
  host = el;
  host.addEventListener('click', onClick);
  ensureListening();
  if (!unregister) unregister = registerCommands(paletteItems);
  render();
}

/** The Security view became visible: refresh a stale status. */
export function showDefender() {
  loadStatus();
  render();
}
