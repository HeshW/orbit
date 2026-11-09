// 08 / Security: a scan of running processes: signatures, odd folders and other warning signs.
//
// The scan runs once when the view first opens and again on "Scan again" (or the palette's
// "Security scan"). Findings come from `security_scan`, most severe first, one per program
// file. Rows whose process has since exited stay listed, marked "Exited".

import { call } from '../api.js';
import { store, on, selectProcess } from '../store.js';
import { escapeHtml, fuzzy } from '../format.js';
import { toast, navigate } from '../app.js';
import { registerCommands } from '../palette.js';
import { endTask } from './processes.js';

const ICON_RETRY_MS = 4000;
const MAX_VT_ITEMS = 24;
const VT_TITLE = 'Check on VirusTotal. Orbit sends only the file’s SHA-256 hash, never the file.';
const CHECKED_LIST = 'a valid signature, an unusual folder (Temp, Downloads, a browser profile, ProgramData), '
  + 'hidden or system files, names that copy Windows, files padded with zeros and files missing from disk';

const SEVERITY = {
  high: { label: 'High', chip: 'chip--danger', order: 0 },
  medium: { label: 'Medium', chip: 'chip--warn', order: 1 },
  info: { label: 'Info', chip: '', order: 2 },
};

const FILTERS = [
  { id: 'all', label: 'All', test: () => true },
  { id: 'high', label: 'High', test: (f) => f.severity === 'high' },
  { id: 'medium', label: 'Medium', test: (f) => f.severity === 'medium' },
  { id: 'info', label: 'Info', test: (f) => f.severity === 'info' },
];

let root = null;
const els = {};
let visible = false;
let findings = [];
let starts = new Map(); // pid -> start time when the scan ran (pid reuse check)
let scanned = null; // { at, procs, files }
let scanning = false;
let scanError = null;
let filter = 'all';
let liveKey = '';
const icons = new Map(); // exe -> data URL, or '' when Windows has none
let iconTimer = 0;
let unregister = null;

const icon = (id) => `<svg class="icon" aria-hidden="true"><use href="#${id}"/></svg>`;
const text = (s) => escapeHtml(s == null ? '' : String(s));

function errText(err) {
  if (typeof err === 'string' && err) return err;
  if (err && typeof err.message === 'string' && err.message) return err.message;
  return 'Something went wrong. Try again.';
}

function rows() {
  const snap = store.snapshot;
  return snap && Array.isArray(snap.processes) ? snap.processes : [];
}

/** The live row for a finding, or null once that process has exited (or its pid was reused). */
function liveRow(f) {
  const list = rows();
  for (let i = 0; i < list.length; i++) {
    const r = list[i];
    if (r.pid !== f.pid) continue;
    const t = starts.get(f.pid);
    if (t && r.startTime && Math.abs(r.startTime - t) > 1) return null;
    return r;
  }
  return null;
}

// ------------------------------------------------------------------ shared actions

/**
 * Hash the file and open its VirusTotal report in the browser. Only the SHA-256 hash
 * leaves the PC (it is part of the URL); the file itself is never uploaded.
 */
export async function checkOnVirusTotal(exe, name) {
  const label = name || 'this file';
  if (!exe) {
    toast('The file location is hidden. Try Restart as administrator.', 'error');
    return false;
  }
  try {
    const hash = String(await call('file_sha256', { path: exe })).toLowerCase();
    if (!/^[0-9a-f]{64}$/.test(hash)) throw 'Could not hash that file.';
    await call('open_url', { url: `https://www.virustotal.com/gui/file/${hash}` });
    toast(`Opened VirusTotal for ${label}. Only its hash was sent.`, 'success');
    return true;
  } catch (err) {
    toast(errText(err), 'error');
    return false;
  }
}

// ------------------------------------------------------------------ scan

/** Run a scan now. Does nothing while one is running. */
export async function scan() {
  if (scanning) return;
  scanning = true;
  scanError = null;
  const list = rows();
  const files = new Set();
  let procs = 0;
  for (const r of list) {
    if (!r.exe || r.pid === 0 || r.pid === 4) continue;
    procs++;
    files.add(r.exe.toLowerCase());
  }
  render();
  try {
    const res = await call('security_scan');
    findings = Array.isArray(res) ? res : [];
    starts = new Map();
    for (const f of findings) {
      const r = list.find((x) => x.pid === f.pid);
      if (r && r.startTime) starts.set(f.pid, r.startTime);
    }
    scanned = { at: Date.now(), procs, files: files.size };
  } catch (err) {
    scanError = errText(err);
    if (findings.length) toast(`The scan did not finish. ${scanError}`, 'error');
  }
  scanning = false;
  liveKey = computeLiveKey();
  loadIcons();
  render();
}

async function loadIcons() {
  const want = [...new Set(findings.map((f) => f.exe).filter((p) => p && !icons.has(p)))];
  if (!want.length) return;
  try {
    const map = (await call('get_icons', { paths: want })) || {};
    let missing = false;
    for (const p of want) {
      const src = map[p];
      if (typeof src === 'string' && src.startsWith('data:image/')) icons.set(p, src);
      else missing = true;
    }
    if (missing && !iconTimer) {
      // Windows sometimes needs a moment for an icon; try once more, then show the blank tile.
      iconTimer = setTimeout(async () => {
        iconTimer = 0;
        const again = want.filter((p) => !icons.has(p));
        try {
          const m2 = (await call('get_icons', { paths: again })) || {};
          for (const p of again) icons.set(p, typeof m2[p] === 'string' && m2[p].startsWith('data:image/') ? m2[p] : '');
        } catch (_) {
          for (const p of again) icons.set(p, '');
        }
        if (isVisible()) renderRows();
      }, ICON_RETRY_MS);
    }
  } catch (_) { /* keep the blank tiles */ }
  if (isVisible()) renderRows();
}

function computeLiveKey() {
  let key = '';
  for (const f of findings) key += liveRow(f) ? '1' : '0';
  return key;
}

// ------------------------------------------------------------------ render

function counts() {
  const c = { high: 0, medium: 0, info: 0 };
  for (const f of findings) if (c[f.severity] !== undefined) c[f.severity]++;
  return c;
}

function matchesQuery(f, q) {
  if (!q) return true;
  const hay = `${f.name} ${f.exe || ''} ${f.publisher || ''} ${(f.reasons || []).join(' ')} ${f.pid}`;
  return fuzzy(hay, q) >= 1000; // substring matches only, as the other lists do for long text
}

function visibleFindings() {
  const flt = FILTERS.find((x) => x.id === filter) || FILTERS[0];
  const q = (store.query || '').trim();
  return findings.filter((f) => flt.test(f) && matchesQuery(f, q));
}

function publisherCell(f) {
  switch (f.signatureStatus) {
    case 'signed':
      return f.publisher ? `<span class="sec-pub" title="${text(f.publisher)}">${text(f.publisher)}</span>` : '<span class="sec-pub">Signed</span>';
    case 'unsigned':
      return '<span class="sec-pub sec-pub--warn">Unsigned</span>';
    case 'invalid':
      return `<span class="sec-pub sec-pub--danger" title="${text(f.publisher ? `Invalid signature (${f.publisher})` : 'Invalid signature')}">Invalid signature</span>`;
    default:
      return '<span class="sec-pub lv-faint">Not checked</span>';
  }
}

function iconCell(exe) {
  const src = exe ? icons.get(exe) : '';
  if (src) return `<img class="lv-ico" src="${src}" alt="" width="18" height="18" decoding="async">`;
  return `<span class="lv-ico lv-ico--blank" aria-hidden="true">${icon('i-grid')}</span>`;
}

function rowHtml(f, i) {
  const live = liveRow(f);
  const sev = SEVERITY[f.severity] || SEVERITY.info;
  const name = text(f.name);
  const reasons = (f.reasons || []).map((r) => `<span class="chip lv-chip sec-reason">${text(r)}</span>`).join('');
  const pid = live
    ? `<span class="lv-num">${Number(f.pid)}</span>`
    : '<span class="chip chip--sm lv-chip--muted" title="This process has exited since the scan">Exited</span>';
  const off = live ? '' : ' disabled';
  const noFile = f.exe ? '' : ' disabled';
  return `<tr class="row sec-row${live ? '' : ' is-dim'}" data-i="${i}">
    <td class="c-name"><div class="lv-namecell">${iconCell(f.exe)}<div class="sec-namebox">
      <span class="lv-primary" title="${name}">${name}</span>
      ${f.exe ? `<span class="lv-mono sec-path" title="${text(f.exe)}">${text(f.exe)}</span>` : ''}
    </div></div></td>
    <td class="c-pid cell-num">${pid}</td>
    <td class="c-pub">${publisherCell(f)}</td>
    <td class="c-reasons"><div class="sec-reasons">${reasons}</div></td>
    <td class="c-sev"><span class="chip lv-chip ${sev.chip}">${sev.label}</span></td>
    <td class="c-actions"><div class="sec-actions">
      <button type="button" class="icon-btn lv-more" data-act="details" aria-label="Details for ${name}" title="Details"${off}>${icon('i-spark')}</button>
      <button type="button" class="icon-btn lv-more" data-act="location" aria-label="Open file location of ${name}" title="Open file location"${noFile}>${icon('i-folder')}</button>
      <button type="button" class="icon-btn lv-more" data-act="vt" aria-label="Check ${name} on VirusTotal" title="${text(VT_TITLE)}"${noFile}>${icon('i-radar')}</button>
      <button type="button" class="icon-btn lv-more sec-act--danger" data-act="end" aria-label="End ${name}" title="End task"${off}>${icon('i-stop')}</button>
    </div></td>
  </tr>`;
}

function showEmpty(title, body, action) {
  els.table.classList.add('is-empty');
  els.empty.hidden = false;
  els.empty.innerHTML = `${icon('i-radar')}
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

function checkedText() {
  if (!scanned || !scanned.procs) return `Orbit checked every running program for ${CHECKED_LIST}. Signed Windows files in System32 are skipped.`;
  const p = scanned.procs;
  const n = scanned.files;
  return `Checked ${p} process${p === 1 ? '' : 'es'} (${n} program file${n === 1 ? '' : 's'}) for ${CHECKED_LIST}. Signed Windows files in System32 are skipped.`;
}

function renderRows() {
  if (!root) return;
  const list = visibleFindings();
  if (!scanned && scanning) {
    els.tbody.innerHTML = '';
    showEmpty('Scanning running programs.', 'Checking signatures and file details. This takes a few seconds the first time.');
    return;
  }
  if (!scanned && scanError) {
    els.tbody.innerHTML = '';
    showEmpty('The scan did not finish.', scanError, { id: 'scan', label: 'Try again' });
    return;
  }
  if (!scanned) {
    els.tbody.innerHTML = '';
    showEmpty('No scan yet.', checkedText(), { id: 'scan', label: 'Scan now' });
    return;
  }
  if (!findings.length) {
    els.tbody.innerHTML = '';
    showEmpty('Nothing suspicious found.', checkedText());
    return;
  }
  if (!list.length) {
    els.tbody.innerHTML = '';
    const q = (store.query || '').trim();
    showEmpty(q ? `No findings match “${q}”.` : 'No findings in this group.', 'Pick another filter or clear the search.', filter !== 'all' ? { id: 'all', label: 'Show all' } : null);
    return;
  }
  hideEmpty();
  const focused = document.activeElement;
  const focusAct = focused && els.tbody.contains(focused) ? focused.dataset.act : null;
  const focusIdx = focusAct ? focused.closest('tr')?.dataset.i : null;
  els.tbody.innerHTML = list.map((f) => rowHtml(f, findings.indexOf(f))).join('');
  if (focusAct && focusIdx != null) {
    const again = els.tbody.querySelector(`tr[data-i="${focusIdx}"] [data-act="${focusAct}"]`);
    if (again && !again.disabled) again.focus({ preventScroll: true });
  }
}

function render() {
  if (!root) return;
  const c = counts();
  const done = !!scanned;
  els.statHigh.textContent = done ? String(c.high) : '-';
  els.statMedium.textContent = done ? String(c.medium) : '-';
  els.statInfo.textContent = done ? String(c.info) : '-';
  els.statChecked.textContent = done ? String(scanned.procs) : '-';

  for (const btn of els.chips.querySelectorAll('[data-filter]')) {
    const id = btn.dataset.filter;
    const on = id === filter;
    btn.classList.toggle('chip--on', on);
    btn.setAttribute('aria-pressed', on ? 'true' : 'false');
    const n = id === 'all' ? findings.length : c[id];
    btn.querySelector('.lv-chip-count').textContent = done ? String(n) : '';
  }

  els.scanBtn.disabled = scanning;
  els.scanBtn.classList.toggle('is-busy', scanning);
  els.scanLabel.textContent = scanning ? 'Scanning' : done ? 'Scan again' : 'Scan now';
  if (scanning) els.when.textContent = 'Checking signatures';
  else if (done) els.when.textContent = `Scanned ${new Date(scanned.at).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}`;
  else els.when.textContent = '';

  // Only info rows: say plainly that nothing needs attention.
  const calm = done && findings.length > 0 && c.high === 0 && c.medium === 0;
  els.note.hidden = !calm;
  if (calm) els.noteText.textContent = 'Nothing suspicious found. Info rows are programs that run from a user folder, which is common and usually fine.';

  renderRows();
}

// ------------------------------------------------------------------ interaction

async function onTableClick(e) {
  const btn = e.target instanceof Element ? e.target.closest('[data-act]') : null;
  if (!btn || btn.disabled) return;
  const tr = btn.closest('tr[data-i]');
  const f = tr ? findings[Number(tr.dataset.i)] : null;
  if (!f) return;
  switch (btn.dataset.act) {
    case 'details':
      if (liveRow(f)) selectProcess(f.pid);
      else toast('That process has exited.', 'info');
      break;
    case 'location':
      try { await call('open_location', { path: f.exe }); } catch (err) { toast(errText(err), 'error'); }
      break;
    case 'vt':
      btn.disabled = true;
      await checkOnVirusTotal(f.exe, f.name);
      btn.disabled = false;
      break;
    case 'end': {
      const live = liveRow(f);
      if (!live) {
        toast('That process has exited.', 'info');
        break;
      }
      if (await endTask(f.pid, f.name, live.startTime)) {
        setTimeout(() => { if (isVisible()) onSnapshot(true); }, 600);
      }
      break;
    }
    default:
      break;
  }
}

function onSnapshot(force = false) {
  if (!isVisible() || !scanned || document.hidden) return;
  const key = computeLiveKey();
  if (!force && key === liveKey) return;
  liveKey = key;
  renderRows();
}

// ------------------------------------------------------------------ palette

const VT_STOP = new Set(['check', 'on', 'virustotal', 'virus', 'total', 'vt', 'scan', 'hash']);

function paletteQuery() {
  const el = document.getElementById('palette-input');
  return el ? el.value.trim().toLowerCase() : '';
}

function paletteItems() {
  const items = [{
    label: 'Security scan',
    search: 'security scan malware virus suspicious unsigned signature check programs',
    icon: 'i-radar',
    hint: scanning ? 'Running' : '',
    run: () => {
      navigate('security');
      scan();
    },
  }];
  // "Check <name> on VirusTotal" only for processes the typed text names, so a short
  // query does not fill the Actions group with one item per running program.
  const q = paletteQuery();
  if (!q) return items;
  const words = q.split(/\s+/).filter((w) => w.length >= 2 && !VT_STOP.has(w));
  const wantsVt = /virus|total|\bvt\b/.test(q);
  if (!words.length && !wantsVt) return items;
  const seen = new Set();
  const pick = (r) => {
    if (!r || !r.exe || seen.has(r.exe.toLowerCase())) return;
    seen.add(r.exe.toLowerCase());
    items.push({
      label: `Check ${r.name} on VirusTotal`,
      search: `Check ${r.name} on VirusTotal virustotal hash malware ${r.pid}`,
      icon: 'i-radar',
      hint: `PID ${r.pid}`,
      run: () => checkOnVirusTotal(r.exe, r.name),
    });
  };
  if (!words.length) {
    // "virustotal" alone: offer what the last scan flagged.
    for (const f of findings) {
      if (items.length > MAX_VT_ITEMS) break;
      pick(liveRow(f) || { pid: f.pid, name: f.name, exe: f.exe });
    }
    return items;
  }
  for (const r of rows()) {
    if (items.length > MAX_VT_ITEMS) break;
    const name = String(r.name || '').toLowerCase();
    if (words.some((w) => name.includes(w) || String(r.pid) === w)) pick(r);
  }
  return items;
}

// ------------------------------------------------------------------ view API

export function mount(rootEl) {
  root = rootEl;
  // The table runs its own scroller inside the view (app.css .view--fill).
  root.classList.add('view--fill');
  root.innerHTML = `
  <div class="lv sec">
    <header class="view-head">
      <div class="lv-head-text">
        <p class="micro">08 / Security</p>
        <h1 class="view-title">Check what runs. <em>Trust what stays.</em></h1>
      </div>
      <div class="lv-stats" aria-live="polite">
        <div class="stat lv-stat"><span class="micro stat-label">High</span><span class="stat-value lv-stat-value" data-stat="high">-</span></div>
        <div class="stat lv-stat"><span class="micro stat-label">Medium</span><span class="stat-value lv-stat-value" data-stat="medium">-</span></div>
        <div class="stat lv-stat"><span class="micro stat-label">Info</span><span class="stat-value lv-stat-value" data-stat="info">-</span></div>
        <div class="stat lv-stat"><span class="micro stat-label">Checked</span><span class="stat-value lv-stat-value" data-stat="checked">-</span></div>
      </div>
    </header>
    <div class="toolbar lv-toolbar">
      <div class="lv-chips" role="group" aria-label="Filter findings">
        ${FILTERS.map((f) => `<button type="button" class="chip" data-filter="${f.id}" aria-pressed="false">${f.label}<span class="lv-chip-count"></span></button>`).join('')}
      </div>
      <span class="lv-toolbar-spacer"></span>
      <span class="micro lv-count" data-when aria-live="polite"></span>
      <button type="button" class="btn btn--ghost btn--sm sec-scan" data-scan><span data-scan-label>Scan now</span>${icon('i-refresh')}</button>
    </div>
    <div class="lv-note" data-note role="status" hidden>
      ${icon('i-check')}
      <span class="lv-note-text" data-note-text></span>
    </div>
    <div class="panel lv-table-panel">
      <div class="lv-scroll">
        <table class="table lv-table sec-table">
          <thead><tr>
            <th class="c-name" scope="col">Name</th>
            <th class="c-pid cell-num" scope="col">PID</th>
            <th class="c-pub" scope="col">Publisher</th>
            <th class="c-reasons" scope="col">Why</th>
            <th class="c-sev" scope="col">Severity</th>
            <th class="c-actions" scope="col"><span class="lv-sr">Actions</span></th>
          </tr></thead>
          <tbody></tbody>
        </table>
        <div class="empty lv-empty" hidden></div>
      </div>
    </div>
  </div>`;

  els.table = root.querySelector('.sec-table');
  els.tbody = els.table.querySelector('tbody');
  els.empty = root.querySelector('.lv-empty');
  els.chips = root.querySelector('.lv-chips');
  els.scanBtn = root.querySelector('[data-scan]');
  els.scanLabel = root.querySelector('[data-scan-label]');
  els.when = root.querySelector('[data-when]');
  els.note = root.querySelector('[data-note]');
  els.noteText = root.querySelector('[data-note-text]');
  els.statHigh = root.querySelector('[data-stat="high"]');
  els.statMedium = root.querySelector('[data-stat="medium"]');
  els.statInfo = root.querySelector('[data-stat="info"]');
  els.statChecked = root.querySelector('[data-stat="checked"]');

  els.scanBtn.addEventListener('click', () => scan());
  els.chips.addEventListener('click', (e) => {
    const btn = e.target instanceof Element ? e.target.closest('[data-filter]') : null;
    if (!btn || btn.dataset.filter === filter) return;
    filter = btn.dataset.filter;
    render();
  });
  els.tbody.addEventListener('click', onTableClick);
  els.empty.addEventListener('click', (e) => {
    const btn = e.target instanceof Element ? e.target.closest('[data-empty-act]') : null;
    if (!btn) return;
    if (btn.dataset.emptyAct === 'scan') scan();
    else if (btn.dataset.emptyAct === 'all') {
      filter = 'all';
      render();
    }
  });

  on('snapshot', () => onSnapshot());
  on('query', () => { if (isVisible()) renderRows(); });
  if (!unregister) unregister = registerCommands(paletteItems);
  render();
}

export function onShow() {
  visible = true;
  if (!scanned && !scanning && !scanError) scan();
  else render();
}

export function onHide() {
  visible = false;
}

export function isVisible() {
  return visible && !!root;
}
