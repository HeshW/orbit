// Optimize > Cleanup (0.5, feature 7c): junk files and biggest folders.
// Junk files: cleanup_scan sizes a fixed list of categories (temp folders, the Windows Update
// cache, crash dumps, thumbnails, browser caches, the Recycle Bin); the user ticks some and
// cleanup_clean deletes them after a confirm. Only category ids go to Rust, never paths.
// Biggest folders: cleanup_folder_sizes on a drive or folder, drill down, "Open folder"
// (open_location). Read only: there is no delete there.
// Progress for all three jobs arrives as orbit://cleanup-progress; each can be cancelled.
import { call, onEvent } from '../../api.js';
import { escapeHtml, bytes, time } from '../../format.js';
import { toast, confirmDialog, restartAsAdmin } from '../../app.js';

const ON_BY_DEFAULT = new Set(['user-temp', 'windows-temp', 'update-cache', 'crash-dumps', 'system-dumps', 'thumbnails']);

let root = null;
let ctx = null;
const els = {};
const junk = { scan: null, scanning: false, progress: null, error: null, picks: null, olderOn: true, cleaning: false, result: null };
const big = { drives: [], path: '', data: null, scanning: false, progress: null, error: null, seen: new Map() };

const icon = (id) => `<svg class="icon" aria-hidden="true"><use href="#${id}"/></svg>`;
const text = (s) => escapeHtml(String(s ?? ''));
const count = (n) => Number(n || 0).toLocaleString();
const plural = (n, one, many = `${one}s`) => `${count(n)} ${n === 1 ? one : many}`;

function errMsg(err) {
  if (typeof err === 'string') return err;
  if (err && typeof err.message === 'string') return err.message;
  return 'Something went wrong.';
}

/* ---------- junk files: data ---------- */

function sizeOf(c) {
  return c.ageFilter && junk.olderOn ? { bytes: c.oldBytes, files: c.oldFiles } : { bytes: c.bytes, files: c.files };
}

/** Why this category can't be ticked, or '' when it can. */
function lockReason(c) {
  if (c.needsAdmin && !junk.scan?.isAdmin) return 'admin';
  if (c.blocked) return 'blocked';
  if (!c.found) return 'missing';
  if (sizeOf(c).files === 0) return 'empty';
  return '';
}

function picked() {
  return (junk.scan?.categories || []).filter((c) => junk.picks?.has(c.id) && !lockReason(c));
}

async function scan() {
  if (junk.scanning) return;
  junk.scanning = true;
  junk.progress = null;
  junk.error = null;
  renderJunk();
  try {
    const s = await call('cleanup_scan');
    if (!s.cancelled || !junk.scan) junk.scan = s;
    if (!junk.picks) junk.picks = new Set(s.categories.filter((c) => ON_BY_DEFAULT.has(c.id)).map((c) => c.id));
  } catch (err) {
    junk.error = errMsg(err);
  } finally {
    junk.scanning = false;
  }
  renderJunk();
}

async function clean() {
  const list = picked();
  if (!list.length || junk.cleaning) return;
  const total = list.reduce((a, c) => a + sizeOf(c).bytes, 0);
  const names = list.map((c) => c.label).join(', ');
  const ok = await confirmDialog({
    title: `Delete ${bytes(total)} of junk?`,
    body: `${names}. These files are deleted for good, not moved to the Recycle Bin. Files that are in use are skipped.`,
    confirmLabel: 'Clean',
    danger: true,
  });
  if (!ok) return;
  junk.cleaning = true;
  junk.progress = null;
  junk.result = null;
  renderJunk();
  try {
    junk.result = await call('cleanup_clean', { ids: list.map((c) => c.id), olderThanDay: junk.olderOn });
    toast(junk.result.freedBytes ? `Freed ${bytes(junk.result.freedBytes)}.` : 'Nothing was deleted.', junk.result.freedBytes ? 'success' : 'info');
  } catch (err) {
    toast(errMsg(err), 'error');
  } finally {
    junk.cleaning = false;
  }
  renderJunk();
  scan();
}

/* ---------- junk files: render ---------- */

function progressHtml(p, verb, job) {
  const what = p ? [p.label, p.files ? plural(p.files, 'file') : '', p.bytes ? bytes(p.bytes) : ''].filter(Boolean).join(' · ') : 'Starting';
  const pct = p && p.steps ? Math.min(100, ((p.step - (p.done ? 0 : 0.5)) / p.steps) * 100) : 0;
  return `<div class="cl-progress" role="status">
      <div class="cl-progress-text"><span class="signal" aria-hidden="true"></span><span>${text(verb)} <span class="cl-progress-what">${text(what)}</span></span></div>
      <div class="cl-progress-bar${p && p.steps ? '' : ' is-loose'}"><span style="width:${pct.toFixed(1)}%"></span></div>
      <button type="button" class="btn btn--ghost btn--sm" data-cl-cancel="${job}">Cancel</button>
    </div>`;
}

function catHtml(c) {
  const lock = lockReason(c);
  const s = sizeOf(c);
  const on = !lock && junk.picks?.has(c.id);
  const chips = [];
  if (c.needsAdmin) chips.push(`<span class="chip chip--sm ${junk.scan.isAdmin ? '' : 'chip--warn'}" title="Windows only lets administrators clean this">${icon('i-shield')}Admin</span>`);
  if (c.ageFilter && junk.olderOn && c.files > c.oldFiles) chips.push(`<span class="chip chip--sm" title="Files from the last 24 hours stay">${plural(c.files - c.oldFiles, 'new file')} kept</span>`);
  let line = c.description;
  let sub = '';
  if (lock === 'admin') sub = c.readable ? 'Restart as administrator to clean this.' : 'Restart as administrator to see and clean this.';
  else if (lock === 'blocked') sub = c.blocked;
  else if (lock === 'missing') sub = 'Not on this PC.';
  else if (!c.readable) sub = 'Some of it could not be read.';
  if (c.note) sub = [sub, c.note].filter(Boolean).join(' ');
  const size = lock === 'missing' ? '' : lock === 'admin' && !c.readable ? '<span class="cl-size-unknown">Unknown</span>'
    : `${text(bytes(s.bytes))}<small>${text(plural(s.files, 'file'))}</small>`;
  return `<li class="cl-cat${lock ? ' is-locked' : ''}${on ? ' is-on' : ''}" title="${text(c.paths.join('\n'))}">
      <label class="cl-cat-main">
        <input type="checkbox" class="cl-check" data-cl-pick="${text(c.id)}"${on ? ' checked' : ''}${lock || junk.cleaning ? ' disabled' : ''}>
        <span class="cl-cat-text">
          <span class="cl-cat-title">${text(c.label)}${chips.join('')}</span>
          <span class="cl-cat-line">${text(line)}</span>
          ${sub ? `<span class="cl-cat-sub${lock === 'blocked' || lock === 'admin' ? ' cl-cat-sub--warn' : ''}">${text(sub)}</span>` : ''}
        </span>
      </label>
      <span class="cl-size">${size}</span>
    </li>`;
}

function resultHtml(r) {
  const skipped = r.skippedInUse + r.skippedDenied + r.skippedOther;
  const parts = [];
  if (r.skippedInUse) parts.push(`${count(r.skippedInUse)} in use`);
  if (r.skippedDenied) parts.push(`${count(r.skippedDenied)} access denied or read-only`);
  if (r.skippedOther) parts.push(`${count(r.skippedOther)} links or other`);
  const whole = r.categories.filter((c) => c.skipped);
  const lines = [];
  if (skipped) lines.push(`${plural(skipped, 'item')} skipped: ${parts.join(', ')}. Orbit never forces files that are open.`);
  if (r.keptNew) lines.push(`${plural(r.keptNew, 'file')} from the last 24 hours kept.`);
  for (const c of whole) lines.push(`${c.label}: ${c.skipped}`);
  if (r.cancelled) lines.push('Stopped early. What was already deleted stays deleted.');
  return `<div class="cl-result" role="status">
      <span class="cl-result-icon">${icon(r.freedBytes ? 'i-check' : 'i-clock')}</span>
      <div class="cl-result-text">
        <p class="cl-result-title">${r.freedBytes ? `Freed ${text(bytes(r.freedBytes))}` : 'Nothing was deleted'}<span class="cl-result-sub">${text(plural(r.deletedFiles, 'file'))} deleted</span></p>
        ${lines.map((l) => `<p class="cl-result-line">${text(l)}</p>`).join('')}
      </div>
      <button type="button" class="icon-btn" data-cl-dismiss aria-label="Dismiss" title="Dismiss">${icon('i-close')}</button>
    </div>`;
}

function renderJunk() {
  if (!root) return;
  const s = junk.scan;
  els.junkWhen.textContent = s && !junk.scanning ? `Scanned ${time(s.scannedAt)}` : '';
  els.rescan.disabled = junk.scanning || junk.cleaning;
  els.rescan.classList.toggle('is-busy', junk.scanning);
  let body = '';
  if (s?.testRoot) body += `<p class="cl-test">Test mode: every folder is inside ${text(s.testRoot)}.</p>`;
  if (junk.result) body += resultHtml(junk.result);
  if (junk.scanning) body += progressHtml(junk.progress, 'Scanning', 'scan');
  else if (junk.cleaning) body += progressHtml(junk.progress, 'Cleaning', 'clean');
  if (!s) {
    body += junk.error
      ? `<div class="empty">${icon('i-x')}<strong>Could not scan</strong><p>${text(junk.error)}</p><button type="button" class="btn btn--ghost btn--sm" data-cl-rescan>Try again</button></div>`
      : '';
    els.junk.innerHTML = body;
    els.foot.hidden = true;
    ctx.setBadge('');
    return;
  }
  body += `<ul class="cl-cats" aria-label="Junk categories">${s.categories.map(catHtml).join('')}</ul>`;
  els.junk.innerHTML = body;

  const list = picked();
  const total = list.reduce((a, c) => a + sizeOf(c).bytes, 0);
  const adminCats = s.categories.filter((c) => c.needsAdmin && c.found).length;
  els.foot.hidden = false;
  els.total.innerHTML = list.length
    ? `<strong>${text(bytes(total))}</strong> in ${plural(list.length, 'category', 'categories')}`
    : 'Tick what to clean';
  els.clean.disabled = !list.length || junk.cleaning || junk.scanning;
  els.older.checked = junk.olderOn;
  els.older.disabled = junk.cleaning;
  els.admin.hidden = s.isAdmin || !adminCats;
  els.admin.title = `${plural(adminCats, 'category', 'categories')} need administrator rights`;
  const all = s.categories.filter((c) => !lockReason(c)).reduce((a, c) => a + sizeOf(c).bytes, 0);
  ctx.setBadge(all >= 1024 ** 3 ? bytes(all, 0) : '');
}

/* ---------- biggest folders ---------- */

async function loadDrives() {
  try {
    big.drives = await call('cleanup_drives');
  } catch {
    big.drives = [];
  }
  renderBig();
}

async function openFolder(path, { fresh = false } = {}) {
  if (big.scanning) return;
  const key = path.toLowerCase();
  if (!fresh && big.seen.has(key)) {
    big.path = path;
    big.data = big.seen.get(key);
    big.error = null;
    renderBig();
    return;
  }
  big.scanning = true;
  big.progress = null;
  big.error = null;
  big.path = path;
  renderBig();
  try {
    const d = await call('cleanup_folder_sizes', { path, fresh });
    if (d.cancelled) {
      big.error = 'Scan cancelled.';
    } else {
      big.data = d;
      big.path = d.path;
      big.seen.set(d.path.toLowerCase(), d);
    }
  } catch (err) {
    big.error = errMsg(err);
  } finally {
    big.scanning = false;
  }
  renderBig();
}

function parentOf(path) {
  const p = path.replace(/\\+$/, '');
  const i = p.lastIndexOf('\\');
  if (i < 0) return null;
  const up = p.slice(0, i);
  return /^[a-z]:$/i.test(up) ? `${up}\\` : up || null;
}

function crumbsHtml(path) {
  const parts = path.replace(/\\+$/, '').split('\\').filter(Boolean);
  let acc = '';
  return parts.map((name, i) => {
    acc = i === 0 ? `${name}\\` : `${acc.replace(/\\$/, '')}\\${name}`;
    const last = i === parts.length - 1;
    return last
      ? `<span class="cl-crumb is-here" aria-current="location">${text(name)}</span>`
      : `<button type="button" class="cl-crumb" data-cl-go="${text(acc)}">${text(name)}</button><span class="cl-crumb-sep" aria-hidden="true">${icon('i-chevron')}</span>`;
  }).join('');
}

function rowHtml(f, max) {
  const pct = max ? Math.max(0.5, (f.bytes / max) * 100) : 0;
  return `<li class="cl-row">
      <button type="button" class="cl-row-main" data-cl-go="${text(f.path)}" title="Show what is inside ${text(f.name)}">
        <span class="cl-row-name">${icon('i-folder')}<span>${text(f.name)}</span></span>
        <span class="cl-row-bar" aria-hidden="true"><span style="width:${pct.toFixed(1)}%"></span></span>
        <span class="cl-row-size">${text(bytes(f.bytes))}</span>
        <span class="cl-row-files">${text(plural(f.files, 'file'))}</span>
      </button>
      <button type="button" class="btn btn--ghost btn--sm cl-row-open" data-cl-open="${text(f.path)}" title="Open in File Explorer">Open folder</button>
    </li>`;
}

function renderBig() {
  if (!root) return;
  const drives = big.drives.map((d) => {
    const on = big.path && big.path.toLowerCase().startsWith(d.mount.toLowerCase());
    const letter = d.mount.replace(/\\$/, '');
    return `<button type="button" class="chip cl-drive" data-cl-go="${text(d.mount)}" aria-pressed="${on ? 'true' : 'false'}"${big.scanning ? ' disabled' : ''}
      title="${text(`${bytes(d.available)} free of ${bytes(d.total)}`)}">${icon('i-disk')}${text(letter)}${d.name ? ` <span class="lv-faint">${text(d.name)}</span>` : ''}</button>`;
  }).join('');
  els.drives.innerHTML = drives;
  els.pathBtn.disabled = big.scanning;

  let body = '';
  if (big.scanning) body += progressHtml(big.progress && { ...big.progress, steps: 0 }, `Measuring ${big.path}`, 'folders');
  if (big.error) body += `<p class="cl-folder-error">${text(big.error)}</p>`;
  const d = big.data;
  if (!d && !big.scanning) {
    body += `<p class="lv-help cl-folder-help">Pick a drive or type a folder to see which folders take the most space. Orbit only measures here; it never deletes anything.</p>`;
  }
  if (d) {
    const up = parentOf(d.path);
    const max = Math.max(d.filesHereBytes, ...d.folders.map((f) => f.bytes), 1);
    const notes = [];
    if (d.unreadable) notes.push(`${plural(d.unreadable, 'folder')} could not be read${junk.scan?.isAdmin ? '' : ' (restart as administrator to include them)'}.`);
    if (d.links) notes.push(`${plural(d.links, 'link')} not followed.`);
    if (d.otherCount) notes.push(`${plural(d.otherCount, 'smaller folder')} (${bytes(d.otherBytes)}) not listed.`);
    body += `<div class="cl-folder-head">
        <button type="button" class="icon-btn" data-cl-go="${text(up || '')}" aria-label="Up one folder" title="Up one folder"${up && !big.scanning ? '' : ' disabled'}>${icon('i-arrow-up')}</button>
        <nav class="cl-crumbs" aria-label="Folder">${crumbsHtml(d.path)}</nav>
        <span class="cl-folder-total">${text(bytes(d.bytes))} <span class="lv-faint">${text(plural(d.files, 'file'))}</span></span>
        <button type="button" class="btn btn--ghost btn--sm" data-cl-open="${text(d.path)}" title="Open in File Explorer">Open folder</button>
        <button type="button" class="icon-btn" data-cl-refresh-folder aria-label="Measure again" title="Measure again${d.cached ? ' (these sizes came from the scan of a parent folder)' : ''}"${big.scanning ? ' disabled' : ''}>${icon('i-refresh')}</button>
      </div>
      <ul class="cl-rows" aria-label="Biggest folders">
        ${d.folders.map((f) => rowHtml(f, max)).join('')}
        ${d.filesHere ? `<li class="cl-row cl-row--files"><span class="cl-row-main"><span class="cl-row-name">${icon('i-file')}<span>Files in this folder</span></span>
          <span class="cl-row-bar" aria-hidden="true"><span style="width:${Math.max(0.5, (d.filesHereBytes / max) * 100).toFixed(1)}%"></span></span>
          <span class="cl-row-size">${text(bytes(d.filesHereBytes))}</span><span class="cl-row-files">${text(plural(d.filesHere, 'file'))}</span></span><span class="cl-row-gap" aria-hidden="true"></span></li>` : ''}
        ${!d.folders.length && !d.filesHere ? '<li class="cl-row cl-row--empty">This folder is empty.</li>' : ''}
      </ul>
      ${notes.length ? `<p class="lv-help cl-folder-note">${text(notes.join(' '))}</p>` : ''}
      <p class="micro cl-folder-when">Measured ${text(time(d.scannedAt))}</p>`;
  }
  els.big.innerHTML = body;
}

async function openInExplorer(path) {
  try {
    await call('open_location', { path });
  } catch (err) {
    toast(errMsg(err), 'error');
  }
}

/* ---------- mount ---------- */

function onProgress(p) {
  if (!p || !root) return;
  if (p.job === 'folders') {
    if (!big.scanning) return;
    big.progress = p;
    const el = els.big.querySelector('.cl-progress-what');
    if (el) el.textContent = [p.label, p.files ? plural(p.files, 'file') : '', p.bytes ? bytes(p.bytes) : ''].filter(Boolean).join(' · ');
    return;
  }
  if ((p.job === 'scan' && !junk.scanning) || (p.job === 'clean' && !junk.cleaning)) return;
  junk.progress = p;
  const box = els.junk.querySelector('.cl-progress');
  if (!box) return;
  box.outerHTML = progressHtml(p, p.job === 'scan' ? 'Scanning' : 'Cleaning', p.job);
}

export function mount(paneEl, context) {
  root = paneEl;
  ctx = context;
  root.innerHTML = `
    <div class="toolbar cl-toolbar">
      <p class="cl-lede">Clears only the folders listed here. Your documents, downloads, photos and browser history are never touched.</p>
    </div>
    <section class="panel cl-panel" aria-labelledby="cl-junk-title">
      <div class="cl-panel-head">
        <h2 class="micro cl-panel-title" id="cl-junk-title">Junk files</h2>
        <span class="micro lv-count" data-cl-when aria-live="polite"></span>
        <button type="button" class="icon-btn lv-refresh" data-cl-rescan aria-label="Scan again" title="Scan again">${icon('i-refresh')}</button>
      </div>
      <div data-cl-junk></div>
      <div class="cl-foot" data-cl-foot hidden>
        <label class="check"><input type="checkbox" data-cl-older checked><span>Only files older than 24 hours <span class="lv-faint">(temp folders and update downloads)</span></span></label>
        <span class="cl-foot-total" data-cl-total></span>
        <button type="button" class="btn btn--ghost btn--sm" data-cl-admin hidden>${icon('i-shield')}Restart as administrator</button>
        <button type="button" class="btn btn--primary" data-cl-clean disabled>Clean</button>
      </div>
    </section>
    <section class="panel cl-panel" aria-labelledby="cl-big-title">
      <div class="cl-panel-head">
        <h2 class="micro cl-panel-title" id="cl-big-title">Biggest folders</h2>
      </div>
      <div class="cl-pick">
        <div class="cl-drives" data-cl-drives></div>
        <form class="cl-path" data-cl-path>
          <input type="text" class="input cl-path-input" data-cl-path-input placeholder="Or type a folder, like C:\\Users" aria-label="Folder to measure" spellcheck="false" autocomplete="off">
          <button type="submit" class="btn btn--ghost btn--sm" data-cl-path-btn>Measure</button>
        </form>
      </div>
      <div data-cl-big></div>
    </section>`;
  els.junk = root.querySelector('[data-cl-junk]');
  els.junkWhen = root.querySelector('[data-cl-when]');
  els.rescan = root.querySelector('.cl-panel-head [data-cl-rescan]');
  els.foot = root.querySelector('[data-cl-foot]');
  els.older = root.querySelector('[data-cl-older]');
  els.total = root.querySelector('[data-cl-total]');
  els.admin = root.querySelector('[data-cl-admin]');
  els.clean = root.querySelector('[data-cl-clean]');
  els.big = root.querySelector('[data-cl-big]');
  els.drives = root.querySelector('[data-cl-drives]');
  els.pathInput = root.querySelector('[data-cl-path-input]');
  els.pathBtn = root.querySelector('[data-cl-path-btn]');

  onEvent('orbit://cleanup-progress', onProgress);

  root.addEventListener('change', (ev) => {
    const pick = ev.target.closest('[data-cl-pick]');
    if (pick) {
      if (pick.checked) junk.picks.add(pick.dataset.clPick);
      else junk.picks.delete(pick.dataset.clPick);
      renderJunk();
    } else if (ev.target === els.older) {
      junk.olderOn = els.older.checked;
      renderJunk();
    }
  });
  root.addEventListener('click', (ev) => {
    const t = ev.target;
    const cancel = t.closest('[data-cl-cancel]');
    if (cancel) {
      cancel.disabled = true;
      call('cleanup_cancel', { job: cancel.dataset.clCancel }).catch(() => {});
      return;
    }
    const go = t.closest('[data-cl-go]');
    if (go && go.dataset.clGo && !go.disabled) {
      openFolder(go.dataset.clGo);
      return;
    }
    const open = t.closest('[data-cl-open]');
    if (open) {
      openInExplorer(open.dataset.clOpen);
      return;
    }
    if (t.closest('[data-cl-rescan]')) scan();
    else if (t.closest('[data-cl-clean]')) clean();
    else if (t.closest('[data-cl-admin]')) restartAsAdmin();
    else if (t.closest('[data-cl-refresh-folder]') && big.data) openFolder(big.data.path, { fresh: true });
    else if (t.closest('[data-cl-dismiss]')) {
      junk.result = null;
      renderJunk();
    }
  });
  root.querySelector('[data-cl-path]').addEventListener('submit', (ev) => {
    ev.preventDefault();
    const v = els.pathInput.value.trim();
    if (v) openFolder(v, { fresh: true });
  });

  renderJunk();
  renderBig();
}

export function onShow() {
  if (!junk.scan && !junk.scanning) scan();
  if (!big.drives.length) loadDrives();
}

export function onHide() {}
