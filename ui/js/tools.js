// Orbit / tools: "What's locking this?" (find which processes have a file or folder
// open, via find_file_lockers). Tools feature, docs/V04-CONTRACT.md.
//
// Opened from the Processes toolbar ("Find locks") and the palette ("What's locking a
// file"). The dialog is an app.formDialog: Find runs the check and keeps the dialog
// open with the results. In Tauri a file dropped on the window while the dialog is open
// fills the path (tauri://drag-drop); the browser preview has no file paths, so it only
// takes typed paths.

import { call, isTauri } from './api.js';
import { store, setQuery, selectProcess } from './store.js';
import { escapeHtml } from './format.js';
import { formDialog, navigate } from './app.js';
import { registerCommands } from './palette.js';
import { endTask } from './views/processes.js';

const LAST_PATH_KEY = 'orbit.lockPath';
const REFRESH_AFTER_END_MS = 600;

const TYPE_LABEL = {
  app: 'App',
  service: 'Service',
  explorer: 'Explorer',
  console: 'Console',
  critical: 'Windows',
  unknown: 'Process',
};

let unregister = null;
// The open dialog: { input, results, ctl, path, closed, ending, layer } or null.
let current = null;

/** Called once by app.js at boot, not awaited. */
export async function init() {
  if (!unregister) unregister = registerCommands(paletteItems);
}

function paletteItems() {
  return [{
    label: "What's locking a file",
    search: "What's locking a file folder find locks locked in use open handle can't delete",
    icon: 'i-lock',
    hint: 'Tools',
    run: () => openLockFinder(),
  }];
}

function loadLastPath() {
  try {
    return localStorage.getItem(LAST_PATH_KEY) || '';
  } catch {
    return '';
  }
}

function saveLastPath(p) {
  try {
    localStorage.setItem(LAST_PATH_KEY, p);
  } catch {
    /* storage unavailable */
  }
}

function errText(err) {
  if (typeof err === 'string' && err) return err;
  if (err && typeof err.message === 'string' && err.message) return err.message;
  return 'Something went wrong. Try again.';
}

function snapshotRow(pid) {
  const snap = store.snapshot;
  const rows = snap && Array.isArray(snap.processes) ? snap.processes : [];
  return rows.find((r) => r.pid === pid) || null;
}

function ownPid() {
  const p = store.staticInfo && store.staticInfo.pid;
  return Number.isInteger(p) ? p : null;
}

/**
 * Open the "What's locking this?" dialog. `path` prefills the input (default: the last
 * path checked); `run` starts the check right away.
 */
export function openLockFinder({ path, run = false } = {}) {
  const start = typeof path === 'string' ? path : loadLastPath();
  const me = { input: null, results: null, ctl: null, path: '', closed: false, ending: false, unlisten: [] };
  const done = formDialog({
    label: 'Find locks',
    title: "What's locking this?",
    body: isTauri
      ? 'Type or paste the full path of a file or folder, or drop a file here. Orbit lists the programs that have it open.'
      : 'Type or paste the full path of a file or folder. Orbit lists the programs that have it open.',
    okLabel: 'Find',
    render(fields, ctl) {
      fields.innerHTML = `
        <label class="dialog__field">
          <span class="dialog__field-label micro">File or folder</span>
          <input class="input dialog__input" type="text" autocomplete="off" spellcheck="false" placeholder="C:\\Users\\you\\Documents\\report.docx">
        </label>
        <p class="tl-help">A folder is checked file by file, up to 2,000 files.</p>
        <div class="tl-results" aria-live="polite" hidden></div>`;
      me.input = fields.querySelector('input');
      me.results = fields.querySelector('.tl-results');
      me.ctl = ctl;
      me.input.value = start;
      const sync = () => {
        ctl.setOk(!!me.input.value.trim());
        ctl.error('');
      };
      me.input.addEventListener('input', sync);
      sync();
      me.results.addEventListener('click', (e) => onResultClick(me, e));
      wireDrop(me, ctl.form);
    },
    async onSubmit() {
      await runCheck(me);
      return false; // stay open with the results
    },
  });
  current = me;
  done.then(() => {
    me.closed = true;
    for (const fn of me.unlisten) {
      try { fn(); } catch { /* already gone */ }
    }
    me.unlisten = [];
    if (current === me) current = null;
  });
  if (run && start.trim()) me.ctl.form.requestSubmit();
  return done;
}

async function runCheck(me) {
  const path = me.input.value.trim();
  if (!path) {
    me.ctl.error('Type the path of a file or folder.');
    return;
  }
  me.path = path;
  me.ctl.error('');
  renderBusy(me);
  let list;
  try {
    list = await call('find_file_lockers', { path });
  } catch (err) {
    if (me.closed) return;
    renderError(me, errText(err));
    return;
  }
  if (me.closed) return;
  saveLastPath(path);
  renderList(me, Array.isArray(list) ? list : []);
}

function renderBusy(me) {
  me.results.hidden = false;
  me.results.innerHTML = '<p class="tl-state"><span class="signal" aria-hidden="true"></span>Checking. A big folder can take up to half a minute.</p>';
}

function renderError(me, message) {
  me.results.hidden = false;
  me.results.innerHTML = `
    <div class="tl-state tl-state--error" role="alert">
      <svg class="icon" aria-hidden="true"><use href="#i-x"/></svg>
      <span></span>
    </div>`;
  me.results.querySelector('span').textContent = message;
}

function renderList(me, list) {
  me.results.hidden = false;
  if (!list.length) {
    me.results.innerHTML = `
      <div class="tl-state tl-state--empty">
        <svg class="icon" aria-hidden="true"><use href="#i-check"/></svg>
        <div>
          <strong>Nothing has it open.</strong>
          <p>No program is using this right now. If Windows still says it is in use, try again in a moment.</p>
        </div>
      </div>`;
    return;
  }
  const own = ownPid();
  const items = list.map((l) => {
    const pid = Number(l.pid);
    const name = String(l.name || `PID ${pid}`);
    const type = TYPE_LABEL[l.appType] || TYPE_LABEL.unknown;
    const self = own != null && pid === own;
    const listed = !!snapshotRow(pid);
    const exe = l.exe ? String(l.exe) : '';
    return `
      <li class="tl-item" data-pid="${pid}" data-name="${escapeHtml(name)}">
        <div class="tl-item-text">
          <span class="tl-item-name">${escapeHtml(name)}</span>
          <span class="tl-item-meta"><span class="chip tl-chip">${type}</span><span class="tl-pid">PID ${pid}</span></span>
          ${exe ? `<span class="tl-item-path" title="${escapeHtml(exe)}">${escapeHtml(exe)}</span>` : `<span class="tl-item-path tl-item-path--none">${l.appType === 'critical' ? 'Part of Windows.' : 'Path hidden. Try Restart as administrator.'}</span>`}
        </div>
        <div class="tl-item-actions">
          <button type="button" class="btn btn--ghost btn--sm" data-tl="details"${listed ? '' : ' disabled title="Not in the process list yet"'}>Details</button>
          <button type="button" class="btn btn--danger btn--sm" data-tl="end"${self ? ' disabled title="This is Orbit. Close it with its window close button."' : l.appType === 'critical' ? ' disabled title="Windows needs this process. Ending it would crash the PC."' : ''}>End task</button>
        </div>
      </li>`;
  }).join('');
  const n = list.length;
  me.results.innerHTML = `
    <p class="tl-count micro">${n === 1 ? '1 program has it open' : `${n} programs have it open`}</p>
    <ul class="tl-list" aria-label="Programs using this path">${items}</ul>`;
}

async function onResultClick(me, e) {
  const btn = e.target instanceof Element ? e.target.closest('[data-tl]') : null;
  if (!btn || btn.disabled) return;
  const item = btn.closest('.tl-item');
  if (!item) return;
  const pid = Number(item.dataset.pid);
  const name = item.dataset.name || `PID ${pid}`;
  if (btn.dataset.tl === 'details') {
    closeDialog(me);
    showDetails(pid);
    return;
  }
  if (btn.dataset.tl === 'end') {
    const row = snapshotRow(pid);
    me.ending = true;
    btn.disabled = true;
    // endTask may ask first (Explorer, Windows processes); that confirm replaces this
    // dialog, so it is reopened with the same path afterwards.
    const ok = await endTask(pid, row ? row.name : name, row ? row.startTime : undefined);
    me.ending = false;
    if (me.closed) {
      if (!current) openLockFinder({ path: me.path, run: true });
      return;
    }
    if (!ok) {
      btn.disabled = false;
      return;
    }
    setTimeout(() => {
      if (!me.closed && me.path) runCheck(me);
    }, REFRESH_AFTER_END_MS);
  }
}

function closeDialog(me) {
  if (me.closed) return;
  const cancel = me.ctl && me.ctl.form.querySelector('[data-act="cancel"]');
  if (cancel) cancel.click();
}

// Same as the palette's Details: clear a search that would hide the row, then select it.
function showDetails(pid) {
  if ((store.query || '') !== '') {
    const search = document.getElementById('search');
    if (search && search.value) {
      search.value = '';
      search.dispatchEvent(new Event('input', { bubbles: true }));
    }
    setQuery('');
  }
  navigate('processes');
  selectProcess(pid);
}

function wireDrop(me, form) {
  const T = typeof window !== 'undefined' ? window.__TAURI__ : undefined;
  if (isTauri && T && T.event && typeof T.event.listen === 'function') {
    const on = (event, fn) => {
      T.event.listen(event, (e) => { if (!me.closed) fn(e.payload || {}); }).then((un) => {
        if (me.closed) un();
        else me.unlisten.push(un);
      }).catch(() => { /* events unavailable: typed paths only */ });
    };
    on('tauri://drag-enter', () => form.classList.add('tl-drop'));
    on('tauri://drag-leave', () => form.classList.remove('tl-drop'));
    on('tauri://drag-drop', (p) => {
      form.classList.remove('tl-drop');
      const paths = Array.isArray(p.paths) ? p.paths : [];
      if (!paths.length || !me.input) return;
      me.input.value = String(paths[0]);
      me.input.dispatchEvent(new Event('input', { bubbles: true }));
      form.requestSubmit();
    });
    return;
  }
  // Browser preview: a dropped file has no path here. Keep the browser from opening it.
  form.addEventListener('dragover', (e) => e.preventDefault());
  form.addEventListener('drop', (e) => {
    e.preventDefault();
    if (me.ctl) me.ctl.error("A dropped file's path isn't available here. Type or paste it instead.");
  });
}
