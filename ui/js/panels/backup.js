// Orbit / panels/backup: the Backup panel in Settings (tools feature, docs/V04-CONTRACT.md).
// settings.js calls mountPanel(el) once with an empty <section class="panel lv-panel">
// in the Settings grid; this module owns everything inside it.
//
// Export writes store.settings as JSON to Downloads (export_report, file name
// orbit-settings-YYYY-MM-DD.json). Import reads a file the user picks, keeps only valid
// values (the same rules as store.js), lists what would change and applies it with
// setSettings after a confirm.

import { call } from '../api.js';
import { store, setSettings } from '../store.js';
import { toast, confirmDialog } from '../app.js';

const MAX_FILE_BYTES = 512 * 1024;
const THEMES = ['night', 'void', 'signal', 'daylight'];
const GROUPINGS = { grouped: 'Grouped', tree: 'Tree', flat: 'Flat list' };
const BOOL_KEYS = ['paused', 'compact', 'reducedMotion', 'alwaysOnTop', 'autoUpdate', 'showTray',
  'closeToTray', 'startWithWindows', 'startMinimized', 'hotkeyEnabled', 'historyEnabled'];

const LABELS = {
  theme: 'Theme',
  intervalMs: 'Update interval',
  paused: 'Pause updates',
  compact: 'Compact rows',
  reducedMotion: 'Reduced motion',
  grouping: 'Default process view',
  alwaysOnTop: 'Always on top',
  autoUpdate: 'Check for updates automatically',
  showTray: 'Tray icon',
  closeToTray: 'Close to tray',
  startWithWindows: 'Start with Windows',
  startMinimized: 'Start in the tray',
  hotkeyEnabled: 'Ctrl+Shift+O shortcut',
  historyEnabled: 'Record app history',
  historyDays: 'Days of history',
  watchlist: 'Pinned processes',
  watchNames: 'Pinned apps',
};

/** Render the panel into `containerEl`. */
export function mountPanel(containerEl) {
  if (!containerEl) return;
  containerEl.setAttribute('aria-labelledby', 'set-backup-h');
  containerEl.innerHTML = `
    <div class="lv-panel-head">
      <h2 class="lv-panel-title" id="set-backup-h">Backup</h2>
      <span class="micro">Settings / File</span>
    </div>
    <div class="lv-field">
      <div class="lv-field-text">
        <span class="lv-label" id="set-export-l">Export settings</span>
        <span class="lv-help">Save your theme, layout, pins and other settings to a file in Downloads.</span>
      </div>
      <button type="button" class="btn btn--sm" data-backup="export" aria-describedby="set-export-l">Export</button>
    </div>
    <div class="lv-field">
      <div class="lv-field-text">
        <span class="lv-label" id="set-import-l">Import settings</span>
        <span class="lv-help">Load a file you exported. Orbit shows what changes before it applies anything.</span>
      </div>
      <button type="button" class="btn btn--sm" data-backup="import" aria-describedby="set-import-l">Import</button>
      <input type="file" accept=".json,.txt,application/json,text/plain" data-backup-file hidden tabindex="-1" aria-hidden="true">
    </div>`;

  const fileInput = containerEl.querySelector('[data-backup-file]');
  containerEl.addEventListener('click', (e) => {
    const btn = e.target instanceof Element ? e.target.closest('[data-backup]') : null;
    if (!btn) return;
    if (btn.dataset.backup === 'export') exportSettings(btn);
    else if (btn.dataset.backup === 'import') {
      fileInput.value = '';
      fileInput.click();
    }
  });
  fileInput.addEventListener('change', () => {
    const file = fileInput.files && fileInput.files[0];
    fileInput.value = '';
    if (file) importFile(file);
  });
}

// ---------------------------------------------------------------- export

function localDate(d = new Date()) {
  const p = (n) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

async function exportSettings(btn) {
  btn.disabled = true;
  try {
    const contents = `${JSON.stringify(store.settings, null, 2)}\n`;
    const path = await call('export_report', { fileName: `orbit-settings-${localDate()}.json`, contents });
    toast(`Saved settings to ${path}`, 'success');
  } catch (err) {
    toast(errText(err), 'error');
  } finally {
    btn.disabled = false;
  }
}

// ---------------------------------------------------------------- import

function errText(err) {
  if (typeof err === 'string' && err) return err;
  if (err && typeof err.message === 'string' && err.message) return err.message;
  return 'Something went wrong. Try again.';
}

/**
 * Valid settings from a parsed file, by store.js's rules.
 * Returns { patch, known, skipped } or null when it is not a settings object.
 */
export function validateSettings(raw) {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return null;
  const patch = {};
  let known = 0;
  const skipped = [];
  const take = (k, ok, value) => {
    known++;
    if (ok) patch[k] = value;
    else skipped.push(LABELS[k] || k);
  };
  for (const [k, v] of Object.entries(raw)) {
    if (k === 'theme') take(k, THEMES.includes(v), v);
    else if (k === 'intervalMs') {
      const n = Number(v);
      take(k, typeof v === 'number' && Number.isFinite(n) && n >= 250 && n <= 10000, Math.round(n));
    } else if (k === 'grouping') take(k, Object.prototype.hasOwnProperty.call(GROUPINGS, v), v);
    else if (BOOL_KEYS.includes(k)) take(k, typeof v === 'boolean', v);
    else if (k === 'historyDays') {
      take(k, typeof v === 'number' && Number.isFinite(v), Math.min(90, Math.max(1, Math.round(v))));
    } else if (k === 'watchlist') {
      const list = Array.isArray(v)
        ? v.filter((e) => e && typeof e === 'object' && Number.isInteger(Number(e.pid)) && Number(e.pid) >= 0 && Number.isFinite(Number(e.start)))
          .map((e) => ({ pid: Number(e.pid), start: Number(e.start) }))
        : null;
      take(k, !!list, list);
    } else if (k === 'watchNames') {
      const list = Array.isArray(v) ? [...new Set(v.filter((n) => typeof n === 'string' && n.trim()).map((n) => n.trim()))] : null;
      take(k, !!list, list);
    } else if (v === null || ['string', 'number', 'boolean', 'object'].includes(typeof v)) {
      // Keys other modules keep in settings (for example historyRange).
      patch[k] = v;
    }
  }
  if (!known) return null;
  return { patch, known, skipped };
}

function same(a, b) {
  return JSON.stringify(a) === JSON.stringify(b);
}

function themeName(t) {
  return typeof t === 'string' && t ? t.charAt(0).toUpperCase() + t.slice(1) : 'Night';
}

function seconds(ms) {
  const n = Number(ms) || 1000;
  return `${Math.round(n / 100) / 10} s`;
}

function show(key, v) {
  if (v === undefined) return 'Not set';
  if (key === 'theme') return themeName(v);
  if (key === 'intervalMs') return seconds(v);
  if (key === 'grouping') return GROUPINGS[v] || String(v);
  if (key === 'historyDays') return `${v} days`;
  if (key === 'watchlist' || key === 'watchNames') return `${Array.isArray(v) ? v.length : 0} pinned`;
  if (typeof v === 'boolean') return v ? 'On' : 'Off';
  if (v === null) return 'None';
  if (typeof v === 'object') return 'Changed';
  const s = String(v);
  return s.length > 40 ? `${s.slice(0, 39)}...` : s;
}

/** Lines like "Theme: Night to Daylight" for each value the patch changes. */
export function describeChanges(patch, current = store.settings) {
  const lines = [];
  const other = [];
  for (const [k, v] of Object.entries(patch)) {
    if (same(v, current[k])) continue;
    if (LABELS[k]) lines.push({ keys: [k], text: `${LABELS[k]}: ${show(k, current[k])} to ${show(k, v)}` });
    else other.push(k);
  }
  // Choices other views remember (for example the History range) share one line.
  if (other.length) {
    lines.push({ keys: other, text: other.length === 1 ? '1 other remembered view choice' : `${other.length} other remembered view choices` });
  }
  // Known settings first, in the order Settings shows them.
  const order = Object.keys(LABELS);
  const rank = (k) => (order.includes(k) ? order.indexOf(k) : order.length);
  lines.sort((a, b) => rank(a.keys[0]) - rank(b.keys[0]));
  return lines;
}

async function importFile(file) {
  if (file.size > MAX_FILE_BYTES) {
    toast("That file is too big to be Orbit settings.", 'error');
    return;
  }
  let raw;
  try {
    const text = await file.text();
    raw = JSON.parse(text.charCodeAt(0) === 0xfeff ? text.slice(1) : text);
  } catch {
    toast(`${file.name} isn't an Orbit settings file. Pick a file made with Export.`, 'error');
    return;
  }
  const result = validateSettings(raw);
  if (!result) {
    toast(`${file.name} isn't an Orbit settings file. Pick a file made with Export.`, 'error');
    return;
  }
  const changes = describeChanges(result.patch);
  const skippedNote = result.skipped.length
    ? `\n\nSkipped because the values aren't valid: ${result.skipped.join(', ')}.`
    : '';
  if (!changes.length) {
    toast(`${file.name} matches your current settings.${result.skipped.length ? ` Skipped ${result.skipped.length} values that aren't valid.` : ''}`, 'info');
    return;
  }
  const n = changes.length;
  const ok = await confirmDialog({
    title: n === 1 ? 'Import 1 setting?' : `Import ${n} settings?`,
    body: `From ${file.name}:\n${changes.map((c) => `- ${c.text}`).join('\n')}${skippedNote}`,
    confirmLabel: 'Import',
  });
  if (!ok) return;
  const patch = {};
  for (const c of changes) for (const k of c.keys) patch[k] = result.patch[k];
  setSettings(patch);
  toast(n === 1 ? 'Imported 1 setting.' : `Imported ${n} settings.`, 'success');
}
