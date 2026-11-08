// Orbit / app: boot, router, shortcuts, theme, titlebar, toasts, context menu, confirm dialog.

import { isTauri, call, onSnapshot, win } from './api.js';
import { store, on, emit, setSettings, setQuery, pushSnapshot, applyDelta, isDelta, isPidPinned } from './store.js';
import { pct, bytes, escapeHtml } from './format.js';

export const views = {};

const VIEW_NAMES = ['processes', 'performance', 'startup', 'services', 'settings'];
const VIEW_KEY = 'orbit.view';
const SEARCH_DEBOUNCE_MS = 80;
const WATCH_CPU = 50;
const WATCH_HOLD_MS = 10000;
const WATCH_REPEAT_MS = 60000;

const $ = (id) => document.getElementById(id);
const icon = (id) => `<svg class="icon" aria-hidden="true"><use href="#${id}"/></svg>`;

let currentView = null;
const scrollPos = {};
let palette = null;

function errText(err) {
  if (typeof err === 'string') return err;
  if (err && typeof err.message === 'string') return err.message;
  return String(err);
}

// ======================================================================== toasts

const TOAST_MS = { info: 3800, success: 3800, warn: 6000, error: 6500 };
const TOAST_ICON = { info: null, success: 'i-check', warn: 'i-bolt', error: 'i-x' };
const MAX_TOASTS = 4;

/** Show a short message. kind: 'info' | 'success' | 'warn' | 'error'. Returns a dismiss function. */
export function toast(message, kind = 'info') {
  const host = $('toasts');
  if (!host) return () => {};
  const k = Object.prototype.hasOwnProperty.call(TOAST_MS, kind) ? kind : 'info';
  const text = message && typeof message === 'object' ? errText(message) : String(message ?? '');

  const el = document.createElement('div');
  el.className = `toast toast--${k}`;
  if (k === 'error') el.setAttribute('role', 'alert');
  el.innerHTML = `${TOAST_ICON[k] ? icon(TOAST_ICON[k]) : '<span class="signal" aria-hidden="true"></span>'}<span class="toast__msg">${escapeHtml(text)}</span>`;
  host.appendChild(el);
  while (host.children.length > MAX_TOASTS) host.firstElementChild.remove();

  let timer = 0;
  const dismiss = () => {
    clearTimeout(timer);
    if (el.classList.contains('is-leaving')) return;
    el.classList.add('is-leaving');
    setTimeout(() => el.remove(), 260);
  };
  const arm = () => {
    clearTimeout(timer);
    timer = setTimeout(dismiss, TOAST_MS[k]);
  };
  el.addEventListener('mouseenter', () => clearTimeout(timer));
  el.addEventListener('mouseleave', arm);
  el.addEventListener('click', dismiss);
  arm();
  return dismiss;
}

// ======================================================================== context menu

let menu = null; // { el, restore, items }

function closeMenu(restoreFocus = true) {
  if (!menu) return;
  const { el, restore } = menu;
  menu = null;
  el.hidden = true;
  el.innerHTML = '';
  document.removeEventListener('pointerdown', onMenuPointer, true);
  window.removeEventListener('blur', onMenuDismiss);
  window.removeEventListener('resize', onMenuDismiss);
  document.removeEventListener('scroll', onMenuScroll, true);
  if (restoreFocus && restore && document.contains(restore)) restore.focus({ preventScroll: true });
}

function onMenuPointer(e) {
  if (menu && !menu.el.contains(e.target)) closeMenu(false);
}

function onMenuDismiss() {
  closeMenu(false);
}

function onMenuScroll(e) {
  if (menu && !menu.el.contains(e.target)) closeMenu(false);
}

function menuButtons() {
  return menu ? [...menu.el.querySelectorAll('.menu-item:not(:disabled)')] : [];
}

/**
 * Open the shared context menu at viewport (x, y).
 * items: [{ label, icon, action, danger?, disabled?, separator?, hint? }]
 * A separator is { separator: true } (no label). Returns a close function.
 */
export function showMenu(x, y, items) {
  closeMenu(false);
  const el = $('context-menu');
  if (!el) return () => {};
  const list = (Array.isArray(items) ? items : []).filter(Boolean);

  // Drop leading, trailing and doubled separators.
  const clean = [];
  for (const it of list) {
    const isSep = it.separator && !it.label;
    if (isSep && (!clean.length || clean[clean.length - 1].isSep)) continue;
    clean.push(isSep ? { isSep: true } : it);
  }
  while (clean.length && clean[clean.length - 1].isSep) clean.pop();
  if (!clean.some((it) => !it.isSep)) return () => {};

  const restore = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  const html = [];
  clean.forEach((it, i) => {
    if (it.isSep) {
      html.push('<div class="menu-sep" role="separator"></div>');
      return;
    }
    if (it.separator) html.push('<div class="menu-sep" role="separator"></div>');
    html.push(
      `<button type="button" class="menu-item${it.danger ? ' menu-item--danger' : ''}" role="menuitem" data-index="${i}"${it.disabled ? ' disabled aria-disabled="true"' : ''}>`
      + `${it.icon ? icon(it.icon) : '<span class="menu-icon-pad" aria-hidden="true"></span>'}`
      + `<span class="menu-item__label">${escapeHtml(it.label ?? '')}</span>`
      + (it.hint ? `<span class="menu-item__hint">${escapeHtml(it.hint)}</span>` : '')
      + '</button>',
    );
  });
  el.innerHTML = html.join('');
  el.style.left = '0px';
  el.style.top = '0px';
  el.hidden = false;

  const vw = window.innerWidth;
  const vh = window.innerHeight;
  const w = el.offsetWidth;
  const h = el.offsetHeight;
  let left = Math.round(Number(x) || 0);
  let top = Math.round(Number(y) || 0);
  if (left + w > vw - 8) left = Math.max(8, vw - w - 8);
  if (top + h > vh - 8) top = Math.max(8, top - h > 8 ? top - h : vh - h - 8);
  el.style.left = `${Math.max(8, left)}px`;
  el.style.top = `${Math.max(8, top)}px`;

  menu = { el, restore, items: clean };

  el.onclick = (e) => {
    const btn = e.target.closest('.menu-item');
    if (!btn || btn.disabled) return;
    const it = clean[Number(btn.dataset.index)];
    closeMenu(true);
    if (it && typeof it.action === 'function') {
      try {
        const r = it.action();
        if (r && typeof r.catch === 'function') r.catch((err) => toast(errText(err), 'error'));
      } catch (err) {
        toast(errText(err), 'error');
      }
    }
  };
  el.oncontextmenu = (e) => e.preventDefault();

  document.addEventListener('pointerdown', onMenuPointer, true);
  window.addEventListener('blur', onMenuDismiss);
  window.addEventListener('resize', onMenuDismiss);
  document.addEventListener('scroll', onMenuScroll, true);

  const first = menuButtons()[0];
  if (first) first.focus({ preventScroll: true });
  return () => closeMenu(true);
}

function onMenuKey(e) {
  if (!menu) return;
  const btns = menuButtons();
  const i = btns.indexOf(document.activeElement);
  switch (e.key) {
    case 'Escape':
      e.preventDefault();
      e.stopImmediatePropagation();
      closeMenu(true);
      break;
    case 'ArrowDown':
      e.preventDefault();
      e.stopImmediatePropagation();
      if (btns.length) btns[(i + 1) % btns.length].focus();
      break;
    case 'ArrowUp':
      e.preventDefault();
      e.stopImmediatePropagation();
      if (btns.length) btns[(i - 1 + btns.length) % btns.length].focus();
      break;
    case 'Home':
      e.preventDefault();
      e.stopImmediatePropagation();
      if (btns.length) btns[0].focus();
      break;
    case 'End':
      e.preventDefault();
      e.stopImmediatePropagation();
      if (btns.length) btns[btns.length - 1].focus();
      break;
    case 'Tab':
      e.preventDefault();
      e.stopImmediatePropagation();
      closeMenu(true);
      break;
    case 'Enter':
    case ' ':
      // Native button activation; keep global shortcuts (Space = pause) out of it.
      e.stopImmediatePropagation();
      break;
    default:
      e.stopImmediatePropagation();
  }
}

// ======================================================================== confirm dialog

let dialog = null; // { resolve, restore, form?, cancel }

function finishDialog(value) {
  if (!dialog) return;
  const { resolve, restore } = dialog;
  dialog = null;
  const layer = $('dialog');
  layer.hidden = true;
  layer.innerHTML = '';
  layer.onclick = null;
  if (restore && document.contains(restore)) restore.focus({ preventScroll: true });
  resolve(value);
}

function cancelDialog() {
  if (dialog) finishDialog(dialog.cancel);
}

/** Ask a yes/no question. Resolves true when confirmed. */
export function confirmDialog({ title = 'Are you sure?', body = '', confirmLabel = 'Confirm', danger = false } = {}) {
  cancelDialog();
  closeMenu(false);
  const layer = $('dialog');
  if (!layer) return Promise.resolve(false);
  const restore = document.activeElement instanceof HTMLElement ? document.activeElement : null;

  layer.innerHTML = `
    <div class="dialog pass" role="alertdialog" aria-modal="true" aria-labelledby="dialog-title" aria-describedby="dialog-body">
      <div class="dialog__label micro"><span class="signal" aria-hidden="true"></span>Orbit / Confirm</div>
      <h2 class="dialog__title" id="dialog-title"></h2>
      <p class="dialog__body" id="dialog-body"></p>
      <div class="dialog__actions">
        <button type="button" class="btn btn--ghost" data-act="cancel">Cancel</button>
        <button type="button" class="btn ${danger ? 'btn--danger' : 'btn--primary'}" data-act="ok"></button>
      </div>
    </div>`;
  layer.querySelector('.dialog__title').textContent = title;
  const bodyEl = layer.querySelector('.dialog__body');
  if (body) bodyEl.textContent = body;
  else bodyEl.remove();
  const ok = layer.querySelector('[data-act="ok"]');
  ok.textContent = confirmLabel;
  layer.hidden = false;

  layer.onclick = (e) => {
    const act = e.target.closest('[data-act]');
    if (act) finishDialog(act.dataset.act === 'ok');
    else if (e.target === layer) finishDialog(false);
  };

  return new Promise((resolve) => {
    dialog = { resolve, restore, cancel: false };
    // Destructive confirms start on Cancel, so a stray Enter (Del, Enter) does not end anything.
    const initial = danger ? layer.querySelector('[data-act="cancel"]') : ok;
    initial.focus({ preventScroll: true });
  });
}

/**
 * A dialog with form fields, in the confirm dialog's style.
 *   render(fieldsEl, ctl) builds the fields. ctl.setOk(enabled) enables the OK button,
 *   ctl.error(message) shows an inline error ('' hides it).
 *   onSubmit(ctl) runs on OK or Enter: return true to close, or an error string to keep
 *   the dialog open and show it. It may be async (OK shows busy meanwhile).
 * Resolves true when submitted and closed, false when cancelled (Esc, Cancel, backdrop).
 */
export function formDialog({ label = 'Orbit', title = '', body = '', okLabel = 'OK', render, onSubmit } = {}) {
  cancelDialog();
  closeMenu(false);
  const layer = $('dialog');
  if (!layer) return Promise.resolve(false);
  const restore = document.activeElement instanceof HTMLElement ? document.activeElement : null;

  layer.innerHTML = `
    <form class="dialog dialog--form pass" role="dialog" aria-modal="true" aria-labelledby="dialog-title" novalidate>
      <div class="dialog__label micro"><span class="signal" aria-hidden="true"></span><span data-r="label"></span></div>
      <h2 class="dialog__title" id="dialog-title"></h2>
      <p class="dialog__body" id="dialog-body"></p>
      <div class="dialog__fields"></div>
      <p class="dialog__error" role="alert" hidden></p>
      <div class="dialog__actions">
        <button type="button" class="btn btn--ghost" data-act="cancel">Cancel</button>
        <button type="submit" class="btn btn--primary" data-act="ok"></button>
      </div>
    </form>`;
  const form = layer.querySelector('form');
  layer.querySelector('[data-r="label"]').textContent = `Orbit / ${label}`;
  layer.querySelector('.dialog__title').textContent = title;
  const bodyEl = layer.querySelector('.dialog__body');
  if (body) {
    bodyEl.textContent = body;
    form.setAttribute('aria-describedby', 'dialog-body');
  } else {
    bodyEl.remove();
  }
  const ok = layer.querySelector('[data-act="ok"]');
  ok.textContent = okLabel;
  const errEl = layer.querySelector('.dialog__error');
  let okWanted = true;
  let busy = false;
  const ctl = {
    form,
    setOk(enabled) {
      okWanted = !!enabled;
      ok.disabled = busy || !okWanted;
    },
    error(message) {
      errEl.textContent = message || '';
      errEl.hidden = !message;
    },
  };
  if (typeof render === 'function') render(layer.querySelector('.dialog__fields'), ctl);
  layer.hidden = false;

  return new Promise((resolve) => {
    dialog = { resolve, restore, form: true, cancel: false };
    const mine = dialog;
    layer.onclick = (e) => {
      if (e.target.closest('[data-act="cancel"]') || e.target === layer) cancelDialog();
    };
    form.addEventListener('submit', async (e) => {
      e.preventDefault();
      if (busy || !okWanted || dialog !== mine) return;
      busy = true;
      ok.disabled = true;
      ok.setAttribute('aria-busy', 'true');
      let result;
      try {
        result = typeof onSubmit === 'function' ? await onSubmit(ctl) : true;
      } catch (err) {
        result = errText(err);
      }
      if (dialog !== mine) return;
      busy = false;
      ok.removeAttribute('aria-busy');
      ok.disabled = !okWanted;
      if (result === true) finishDialog(true);
      else if (typeof result === 'string' && result) ctl.error(result);
    });
    const first = form.querySelector('[autofocus], .dialog__fields input:not(:disabled), .dialog__fields button') || ok;
    first.focus({ preventScroll: true });
    if (first instanceof HTMLInputElement && first.type === 'text') first.select();
  });
}

// ======================================================================== run new task

const RUN_HISTORY_KEY = 'orbit.runHistory';
const RUN_HISTORY_MAX = 10;

function loadRunHistory() {
  try {
    const v = JSON.parse(localStorage.getItem(RUN_HISTORY_KEY) || '[]');
    return Array.isArray(v) ? v.filter((s) => typeof s === 'string' && s.trim()).slice(0, RUN_HISTORY_MAX) : [];
  } catch {
    return [];
  }
}

function saveRunHistory(command) {
  const lower = command.toLowerCase();
  const list = [command, ...loadRunHistory().filter((c) => c.toLowerCase() !== lower)].slice(0, RUN_HISTORY_MAX);
  try {
    localStorage.setItem(RUN_HISTORY_KEY, JSON.stringify(list));
  } catch {
    /* storage unavailable: no history */
  }
}

/** The Run new task dialog (Ctrl+N): a program, folder, document or URL, like Windows Run. */
export function openRunTask() {
  const history = loadRunHistory();
  const admin = !!(store.staticInfo && store.staticInfo.isAdmin);
  let input = null;
  let check = null;
  return formDialog({
    label: 'Run new task',
    title: 'Run new task',
    body: 'Type the name of a program, folder, document or website, and Windows opens it.',
    okLabel: 'Run',
    render(fields, ctl) {
      fields.innerHTML = `
        <label class="dialog__field">
          <span class="dialog__field-label micro">Open</span>
          <input class="input dialog__input" type="text" list="run-history" autocomplete="off" spellcheck="false" placeholder="notepad, cmd, a folder or a website">
        </label>
        <datalist id="run-history"></datalist>
        <label class="check">
          <input type="checkbox">
          <span>Run as administrator</span>
        </label>`;
      input = fields.querySelector('input[type="text"]');
      check = fields.querySelector('input[type="checkbox"]');
      const list = fields.querySelector('datalist');
      for (const cmd of history) {
        const opt = document.createElement('option');
        opt.value = cmd;
        list.appendChild(opt);
      }
      input.value = history[0] || '';
      if (admin) {
        check.checked = true;
        check.disabled = true;
        check.closest('.check').title = 'Orbit runs as administrator, so new tasks do too.';
      }
      const sync = () => {
        ctl.setOk(!!input.value.trim());
        ctl.error('');
      };
      input.addEventListener('input', sync);
      sync();
    },
    async onSubmit() {
      const command = input.value.trim();
      if (!command) return 'Type a command first.';
      await call('run_task', { command, asAdmin: !!check.checked });
      saveRunHistory(command);
      toast(`Started ${command}.`, 'success');
      return true;
    },
  });
}

// ======================================================================== always on top

let topApplied = false; // the window starts as a normal window

function applyAlwaysOnTop() {
  const on = !!store.settings.alwaysOnTop;
  const btn = $('win-pin');
  if (btn) btn.setAttribute('aria-pressed', on ? 'true' : 'false');
  if (topApplied === on) return;
  topApplied = on;
  win.setAlwaysOnTop(on).catch((err) => {
    topApplied = !on;
    toast(`Could not change always on top. ${errText(err)}`, 'error');
  });
}

function dialogFocusables(layer) {
  return [...layer.querySelectorAll('button, input, select, textarea')].filter((el) => !el.disabled && el.getClientRects().length);
}

function onFormDialogKey(e) {
  const layer = $('dialog');
  const t = e.target;
  if (e.key === 'Escape') {
    e.preventDefault();
    e.stopImmediatePropagation();
    cancelDialog();
    return;
  }
  if (e.key === 'Tab') {
    e.preventDefault();
    e.stopImmediatePropagation();
    const els = dialogFocusables(layer);
    if (!els.length) return;
    const i = els.indexOf(document.activeElement);
    const next = e.shiftKey ? (i <= 0 ? els.length - 1 : i - 1) : (i + 1) % els.length;
    els[next].focus();
    return;
  }
  if (e.key === 'Enter' && t instanceof HTMLInputElement && t.type === 'checkbox') {
    // Enter runs the dialog from a checkbox too (text inputs submit natively).
    e.preventDefault();
    const form = layer.querySelector('form');
    if (form) form.requestSubmit();
  }
  // Typing, Space and Enter work natively; nothing leaks to the app's shortcuts.
  e.stopImmediatePropagation();
}

function onDialogKey(e) {
  if (!dialog) return;
  if (dialog.form) {
    onFormDialogKey(e);
    return;
  }
  const layer = $('dialog');
  const btns = [...layer.querySelectorAll('button')];
  if (e.key === 'Escape') {
    e.preventDefault();
    e.stopImmediatePropagation();
    finishDialog(false);
    return;
  }
  if (e.key === 'Tab') {
    e.preventDefault();
    e.stopImmediatePropagation();
    const i = btns.indexOf(document.activeElement);
    const next = e.shiftKey ? (i <= 0 ? btns.length - 1 : i - 1) : (i + 1) % btns.length;
    btns[next].focus();
    return;
  }
  if (e.key === 'ArrowLeft' || e.key === 'ArrowRight') {
    e.preventDefault();
    const i = btns.indexOf(document.activeElement);
    btns[(i + (e.key === 'ArrowRight' ? 1 : btns.length - 1)) % btns.length].focus();
  }
  // Enter / Space activate the focused button natively; nothing else leaks to the app.
  e.stopImmediatePropagation();
}

// ======================================================================== router

function initialView() {
  try {
    const v = localStorage.getItem(VIEW_KEY);
    if (VIEW_NAMES.includes(v)) return v;
  } catch {
    /* storage unavailable */
  }
  return 'processes';
}

/** Show a view by name: processes | performance | startup | services | settings. */
export function navigate(view) {
  if (!VIEW_NAMES.includes(view) || view === currentView) return;
  const main = $('main');
  const prev = currentView;
  if (prev) {
    scrollPos[prev] = main.scrollTop;
    const prevEl = $(`view-${prev}`);
    if (prevEl) prevEl.classList.remove('is-active');
    try {
      views[prev]?.onHide?.();
    } catch (err) {
      console.error(`[orbit] ${prev}.onHide failed`, err);
    }
  }
  currentView = view;
  const el = $(`view-${view}`);
  if (el) el.classList.add('is-active');
  main.scrollTop = scrollPos[view] || 0;

  for (const btn of document.querySelectorAll('#sidebar .nav-item')) {
    const on = btn.dataset.view === view;
    btn.classList.toggle('is-active', on);
    if (on) btn.setAttribute('aria-current', 'page');
    else btn.removeAttribute('aria-current');
  }

  try {
    views[view]?.onShow?.();
  } catch (err) {
    console.error(`[orbit] ${view}.onShow failed`, err);
  }
  try {
    localStorage.setItem(VIEW_KEY, view);
  } catch {
    /* storage unavailable */
  }
  emit('view', view);
}

export function currentViewName() {
  return currentView;
}

// ======================================================================== settings, live chip, admin

let sampling = { intervalMs: null, paused: null };

function applyVisualSettings(s) {
  const root = document.documentElement;
  if (root.dataset.theme !== s.theme) root.dataset.theme = s.theme;
  root.classList.toggle('compact', !!s.compact);
  root.classList.toggle('reduce-motion', !!s.reducedMotion);
  const fav = document.querySelector('link[rel="icon"]');
  if (fav) {
    const href = s.theme === 'daylight' ? 'assets/orbit-mark-light.svg' : 'assets/orbit-mark-dark.svg';
    if (fav.getAttribute('href') !== href) fav.setAttribute('href', href);
  }
}

function applySampling(force = false) {
  const { intervalMs, paused } = store.settings;
  if (!force && sampling.intervalMs === intervalMs && sampling.paused === paused) return;
  sampling = { intervalMs, paused };
  // delta: the event then carries static process fields once per process (handleSnapshot merges).
  call('set_sampling', { intervalMs, paused, delta: true }).catch((err) => toast(`Could not change the update speed. ${errText(err)}`, 'error'));
}

function updateLive() {
  const btn = $('live-indicator');
  if (!btn) return;
  const { paused, intervalMs } = store.settings;
  btn.classList.toggle('chip--paused', paused);
  // The label names both the state and the action, so no aria-pressed (it would contradict it).
  btn.setAttribute('aria-label', paused ? 'Paused. Resume updates' : `Live, updating every ${(intervalMs / 1000).toFixed(1)} seconds. Pause updates`);
  btn.title = paused ? 'Resume updates (Space)' : 'Pause updates (Space)';
  const sig = btn.querySelector('.signal');
  if (sig) sig.classList.toggle('signal--off', paused);
  const text = $('live-text');
  if (text) text.textContent = paused ? 'Paused' : `Live / ${(intervalMs / 1000).toFixed(1)} s`;
}

function renderAdminBadge() {
  const host = $('admin-badge');
  if (!host) return;
  const admin = !!(store.staticInfo && store.staticInfo.isAdmin);
  if (admin) {
    host.innerHTML = `<span class="chip chip--admin" title="Orbit is running as administrator">${icon('i-shield')}Admin</span>`;
  } else {
    host.innerHTML = `<button type="button" class="btn btn--ghost btn--sm" title="Some actions need administrator rights">${icon('i-shield')}Run as admin</button>`;
    host.querySelector('button').addEventListener('click', () => restartAsAdmin());
  }
}

/** Relaunch Orbit elevated (UAC prompt). In Tauri the current instance exits on success. */
export async function restartAsAdmin() {
  try {
    await call('restart_as_admin');
    if (!isTauri) {
      store.staticInfo = await call('get_static_info');
      renderAdminBadge();
      toast('Running as administrator (browser preview).', 'success');
    }
  } catch (err) {
    toast(errText(err), 'error');
  }
}

// ======================================================================== snapshots

const miniEls = {};
let miniCache = { cpu: '', mem: '', sub: '', cpuX: -1, memX: -1 };

function updateMini(snap) {
  if (!miniEls.cpu) {
    miniEls.cpu = $('mini-cpu');
    miniEls.cpuBar = $('mini-cpu-bar');
    miniEls.mem = $('mini-mem');
    miniEls.memBar = $('mini-mem-bar');
    miniEls.sub = $('mini-mem-sub');
  }
  const cpu = snap.cpu && typeof snap.cpu.usage === 'number' ? snap.cpu.usage : 0;
  const m = snap.memory || {};
  const memP = m.total > 0 ? (m.used / m.total) * 100 : 0;
  const cpuText = pct(cpu, 0);
  const memText = pct(memP, 0);
  const subText = m.total > 0 ? `${bytes(m.used)} / ${bytes(m.total, 0)}` : '';
  if (cpuText !== miniCache.cpu && miniEls.cpu) miniEls.cpu.textContent = cpuText;
  if (memText !== miniCache.mem && miniEls.mem) miniEls.mem.textContent = memText;
  if (subText !== miniCache.sub && miniEls.sub) miniEls.sub.textContent = subText;
  const cpuX = Math.round(Math.min(1, Math.max(0, cpu / 100)) * 1000) / 1000;
  const memX = Math.round(Math.min(1, Math.max(0, memP / 100)) * 1000) / 1000;
  if (cpuX !== miniCache.cpuX && miniEls.cpuBar) miniEls.cpuBar.style.transform = `scaleX(${cpuX})`;
  if (memX !== miniCache.memX && miniEls.memBar) miniEls.memBar.style.transform = `scaleX(${memX})`;
  miniCache = { cpu: cpuText, mem: memText, sub: subText, cpuX, memX };
}

// One short beat of the titlebar signal per snapshot (no endless animation).
function beatSignal() {
  const s = store.settings;
  if (s.paused || s.reducedMotion) return;
  if (window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches) return;
  const sig = document.querySelector('#live-indicator .signal');
  if (!sig) return;
  sig.classList.remove('is-beat');
  void sig.offsetWidth; // restart the animation
  sig.classList.add('is-beat');
}

// Watchlist: warn once a minute when a pinned process stays above 50% CPU for 10 seconds.
const watchHot = new Map();
const watchToasted = new Map();
let lastSnapTs = 0;

function checkWatchlist(snap) {
  const { watchlist, watchNames } = store.settings;
  const interval = snap.intervalMs || store.settings.intervalMs || 1000;
  const dt = lastSnapTs && snap.ts > lastSnapTs ? Math.min(snap.ts - lastSnapTs, interval * 2) : interval;
  lastSnapTs = snap.ts || Date.now();
  if (!watchlist.length && !watchNames.length) {
    watchHot.clear();
    return;
  }
  const nameSet = new Set(watchNames.map((n) => n.toLowerCase()));
  const seen = new Set();
  const rows = Array.isArray(snap.processes) ? snap.processes : [];
  for (let i = 0; i < rows.length; i++) {
    const r = rows[i];
    if (!nameSet.has(String(r.name).toLowerCase()) && !isPidPinned(r, watchlist)) continue;
    seen.add(r.pid);
    if (r.cpu > WATCH_CPU) {
      const held = (watchHot.get(r.pid) || 0) + dt;
      watchHot.set(r.pid, held);
      if (held >= WATCH_HOLD_MS) {
        const now = Date.now();
        const last = watchToasted.get(r.pid) || 0;
        if (now - last >= WATCH_REPEAT_MS) {
          watchToasted.set(r.pid, now);
          toast(`${r.name} is using ${Math.round(r.cpu)}% CPU`, 'warn');
        }
      }
    } else {
      watchHot.delete(r.pid);
    }
  }
  for (const pid of watchHot.keys()) if (!seen.has(pid)) watchHot.delete(pid);
  for (const pid of watchToasted.keys()) if (!seen.has(pid)) watchToasted.delete(pid);
}

let resyncing = false;

// A delta whose rows we cannot merge (an event was missed, or the page reloaded while the
// backend stayed in delta mode): fetch the full snapshot once and carry on from it.
async function resync() {
  if (resyncing) return;
  resyncing = true;
  try {
    handleSnapshot(await call('get_snapshot'));
  } catch (err) {
    console.error('[orbit] resync failed', err);
  } finally {
    resyncing = false;
  }
}

function handleSnapshot(payload) {
  if (!payload || typeof payload !== 'object') return;
  if (store.snapshot && typeof payload.ts === 'number' && payload.ts <= store.snapshot.ts) return;
  let snap = payload;
  if (isDelta(payload)) {
    snap = applyDelta(payload);
    if (!snap) {
      resync();
      return;
    }
  }
  pushSnapshot(snap);
  if (!document.hidden) {
    updateMini(snap);
    beatSignal();
  }
  checkWatchlist(snap);
}

// ======================================================================== wiring

function isTyping(el) {
  if (!el || !(el instanceof HTMLElement)) return false;
  if (el.isContentEditable) return true;
  const tag = el.tagName;
  if (tag === 'TEXTAREA' || tag === 'SELECT') return true;
  if (tag === 'INPUT') {
    const t = (el.type || 'text').toLowerCase();
    return !['checkbox', 'radio', 'button', 'submit', 'reset', 'range', 'color', 'file'].includes(t);
  }
  return false;
}

const INTERACTIVE_ROLES = new Set(['button', 'switch', 'checkbox', 'menuitem', 'option', 'tab', 'link', 'radio', 'slider', 'spinbutton', 'combobox', 'treeitem']);

function isInteractive(el) {
  if (!el || !(el instanceof HTMLElement)) return false;
  const tag = el.tagName;
  if (tag === 'BUTTON' || tag === 'A' || tag === 'SELECT' || tag === 'SUMMARY' || tag === 'INPUT' || tag === 'TEXTAREA') return true;
  const role = el.getAttribute('role');
  return !!role && INTERACTIVE_ROLES.has(role);
}

let keyboardFocused = null;

function isToggleControl(el) {
  const role = el.getAttribute('role');
  return role === 'switch' || role === 'checkbox' || el.type === 'checkbox';
}

function focusSearch() {
  const input = $('search');
  if (!input) return;
  input.focus();
  input.select();
}

function wireSearch() {
  const input = $('search');
  const clear = $('search-clear');
  if (!input) return;
  let timer = 0;
  const commit = (v) => {
    clearTimeout(timer);
    setQuery(v);
    // Performance has nothing to filter: show the matching processes instead.
    if (v.trim() && currentView === 'performance') navigate('processes');
  };
  input.addEventListener('input', () => {
    if (clear) clear.hidden = !input.value;
    clearTimeout(timer);
    timer = setTimeout(() => commit(input.value), SEARCH_DEBOUNCE_MS);
  });
  input.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      if (input.value) {
        input.value = '';
        if (clear) clear.hidden = true;
        commit('');
      } else {
        input.blur();
      }
    } else if (e.key === 'Enter') {
      e.preventDefault();
      commit(input.value);
    }
  });
  if (clear) {
    clear.addEventListener('click', () => {
      input.value = '';
      clear.hidden = true;
      commit('');
      input.focus();
    });
  }
  on('query', (q) => {
    if (input.value !== q) {
      input.value = q;
      if (clear) clear.hidden = !q;
    }
  });
}

function wireSidebar() {
  const nav = $('sidebar');
  if (!nav) return;
  nav.addEventListener('click', (e) => {
    const item = e.target.closest('.nav-item');
    if (item) navigate(item.dataset.view);
  });
  nav.addEventListener('keydown', (e) => {
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
    const items = [...nav.querySelectorAll('.nav-item')];
    const i = items.indexOf(document.activeElement);
    if (i === -1) return;
    e.preventDefault();
    items[(i + (e.key === 'ArrowDown' ? 1 : items.length - 1)) % items.length].focus();
  });
  const mini = $('mini-readout');
  if (mini) mini.addEventListener('click', () => navigate('performance'));
}

function syncMaxIcon() {
  const btn = $('win-max');
  if (!btn) return;
  win.isMaximized().then((max) => {
    const use = btn.querySelector('use');
    if (use) use.setAttribute('href', max ? '#i-restore' : '#i-maximize');
    btn.setAttribute('aria-label', max ? 'Restore' : 'Maximize');
    btn.title = max ? 'Restore' : 'Maximize';
  }).catch(() => {});
}

function wireTitlebar() {
  const live = $('live-indicator');
  if (live) live.addEventListener('click', () => setSettings({ paused: !store.settings.paused }));
  const run = (fn) => () => Promise.resolve().then(fn).catch((err) => toast(errText(err), 'error'));
  $('win-pin')?.addEventListener('click', () => setSettings({ alwaysOnTop: !store.settings.alwaysOnTop }));
  $('win-min')?.addEventListener('click', run(() => win.minimize()));
  $('win-max')?.addEventListener('click', run(() => win.toggleMaximize()));
  $('win-close')?.addEventListener('click', run(() => win.close()));
  let t = 0;
  window.addEventListener('resize', () => {
    clearTimeout(t);
    t = setTimeout(syncMaxIcon, 120);
  });
  syncMaxIcon();
}

function wireKeys() {
  // Remember which element got focus from the keyboard (Tab, arrows, a menu opened by key),
  // as opposed to a mouse click, for the Space shortcut below.
  let lastInput = 'key';
  document.addEventListener('pointerdown', () => { lastInput = 'pointer'; }, true);
  document.addEventListener('keydown', () => { lastInput = 'key'; }, true);
  document.addEventListener('focusin', (e) => {
    keyboardFocused = lastInput === 'key' ? e.target : null;
  }, true);

  // Modal layers own the keyboard while open (capture phase, before views see keys).
  window.addEventListener('keydown', (e) => {
    // Ctrl+N is Run new task, never WebView2's "new window", even under the palette or a dialog.
    if ((e.ctrlKey || e.metaKey) && (e.key === 'n' || e.key === 'N')) e.preventDefault();
    if (dialog) onDialogKey(e);
    else if (menu) onMenuKey(e);
  }, true);

  document.addEventListener('keydown', (e) => {
    const mod = e.ctrlKey || e.metaKey;
    const key = e.key;
    if (mod && !e.altKey && !e.shiftKey && (key === 'n' || key === 'N')) {
      // Default already prevented by the capture listener above.
      openRunTask();
      return;
    }
    if (e.defaultPrevented) return;

    if (mod && !e.altKey && !e.shiftKey && (key === 'k' || key === 'K')) {
      e.preventDefault();
      if (palette) palette.openPalette();
      return;
    }
    if (mod && !e.altKey && !e.shiftKey && (key === 'f' || key === 'F')) {
      e.preventDefault();
      focusSearch();
      return;
    }
    if (mod || e.altKey || isTyping(e.target)) return;

    if (key === '/') {
      e.preventDefault();
      focusSearch();
      return;
    }
    if (key >= '1' && key <= '5' && key.length === 1) {
      e.preventDefault();
      navigate(VIEW_NAMES[Number(key) - 1]);
      return;
    }
    if (key === ' ' || e.code === 'Space') {
      // A control reached by keyboard keeps native Space. After a mouse click focus stays
      // on the button, and Space should still pause. (:focus-visible cannot tell: Chromium
      // turns it on for the clicked button as soon as any key is pressed.)
      const t = e.target;
      if (isInteractive(t) && (t === keyboardFocused || isToggleControl(t))) return;
      e.preventDefault();
      setSettings({ paused: !store.settings.paused });
      return;
    }
    // Del = end task for the process shown in the details drawer. The processes table
    // handles Del itself (and prevents default), so this only runs outside the table.
    if (key === 'Delete') {
      const drawer = $('details');
      const pid = store.selected;
      const proc = views.processes;
      if (pid == null || !drawer || !drawer.classList.contains('is-open') || !proc || typeof proc.endTask !== 'function') return;
      e.preventDefault();
      const rows = store.snapshot && Array.isArray(store.snapshot.processes) ? store.snapshot.processes : [];
      const row = rows.find((r) => r.pid === pid);
      proc.endTask(pid, row ? row.name : undefined, row ? row.startTime : undefined);
    }
  });

  // No browser context menu in the app window, except where text is edited.
  if (isTauri) {
    document.addEventListener('contextmenu', (e) => {
      if (!isTyping(e.target)) e.preventDefault();
    });
  }
}

function viewFailed(name, err) {
  console.error(`[orbit] view "${name}" failed to load`, err);
  const el = $(`view-${name}`);
  if (!el) return;
  el.innerHTML = `
    <div class="empty">
      ${icon('i-x')}
      <strong>This section did not load.</strong>
      <p></p>
    </div>`;
  el.querySelector('p').textContent = errText(err);
}

async function loadViews() {
  const mods = await Promise.allSettled(VIEW_NAMES.map((n) => import(`./views/${n}.js`)));
  mods.forEach((res, i) => {
    const name = VIEW_NAMES[i];
    if (res.status !== 'fulfilled') {
      viewFailed(name, res.reason);
      return;
    }
    const mod = res.value;
    views[name] = mod;
    try {
      if (typeof mod.mount === 'function') mod.mount($(`view-${name}`));
    } catch (err) {
      delete views[name];
      viewFailed(name, err);
    }
  });
}

// ======================================================================== boot

async function boot() {
  applyVisualSettings(store.settings);
  updateLive();
  wireTitlebar();
  wireSidebar();
  wireSearch();
  wireKeys();

  applyAlwaysOnTop();
  on('settings', (s) => {
    applyVisualSettings(s);
    updateLive();
    applySampling();
    applyAlwaysOnTop();
  });
  // Hidden (minimised): snapshots still update the store, but the titlebar readout waits.
  document.addEventListener('visibilitychange', () => {
    if (!document.hidden && store.snapshot) updateMini(store.snapshot);
  });

  try {
    store.staticInfo = await call('get_static_info');
  } catch (err) {
    toast(`Could not read system information. ${errText(err)}`, 'error');
  }
  renderAdminBadge();

  await loadViews();

  try {
    const details = await import('./details.js');
    details.initDetails();
  } catch (err) {
    console.error('[orbit] details drawer failed to load', err);
  }
  try {
    palette = await import('./palette.js');
    palette.initPalette();
  } catch (err) {
    palette = null;
    console.error('[orbit] command palette failed to load', err);
  }

  // A sidebar click or 1-5 during boot already picked a view before its module had
  // mounted: keep that choice and show it now instead of jumping back to the saved view.
  if (currentView) {
    try {
      views[currentView]?.onShow?.();
    } catch (err) {
      console.error(`[orbit] ${currentView}.onShow failed`, err);
    }
  } else {
    navigate(initialView());
  }

  onSnapshot(handleSnapshot);
  try {
    handleSnapshot(await call('get_snapshot'));
  } catch (err) {
    toast(`Could not read the first sample. ${errText(err)}`, 'error');
  }
  applySampling(true);
}

if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', () => { boot(); }, { once: true });
} else {
  boot();
}
