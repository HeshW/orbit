// Orbit / shell: tray, close to tray, start with Windows, the Ctrl+Shift+O hotkey and the
// mini window toggle (#mini-toggle in the titlebar). Shell feature, docs/V04-CONTRACT.md.
//
// The settings live in store.settings; the backend (shell.rs) gets the ones it needs:
//   orbit://shell-settings { showTray, closeToTray, startMinimized, paused }  (event, at boot
//     and on every change; shell.rs also keeps them for the next start)
//   set_hotkey_enabled(settings.hotkeyEnabled)                              (at boot and on change)
// and sends back:
//   orbit://shell-action { action: 'togglePause' }   tray menu "Pause updates"
//   orbit://shell-mini { open }                      the mini window opened or closed
// store event 'shell' carries the latest ShellState (get_shell_state) for the Behavior panel.

import { call, isTauri } from './api.js';
import { store, on, emit, setSettings } from './store.js';
import { toast } from './app.js';
import { registerCommands } from './palette.js';

const T = typeof window !== 'undefined' ? window.__TAURI__ : undefined;
const SYNC_KEYS = ['showTray', 'closeToTray', 'startMinimized', 'paused'];

let started = false;
let miniOpen = false;
let shellState = null;
let lastSync = '';
let lastHotkey = null;

/** The latest ShellState, or null before the first read. */
export function getShellState() {
  return shellState;
}

/** Whether the mini window is open. */
export function isMiniOpen() {
  return miniOpen;
}

/** Reads get_shell_state, keeps it and tells the Behavior panel (store event 'shell'). */
export async function refreshShellState() {
  try {
    shellState = await call('get_shell_state');
  } catch (err) {
    console.error('[orbit] get_shell_state failed', err);
    shellState = null;
  }
  emit('shell', shellState);
  return shellState;
}

/** Opens or closes the mini window. */
export async function toggleMini() {
  try {
    await call('toggle_mini');
  } catch (err) {
    toast(String(err || 'Orbit could not open mini mode.'), 'error');
    return;
  }
  if (!isTauri) {
    // The browser preview has one window; the mini readout is at mini.html.
    setMiniOpen(!miniOpen);
    if (miniOpen) toast('Mini mode opens its own small window in the app. Preview it at mini.html.');
  }
}

function setMiniOpen(open) {
  miniOpen = !!open;
  const btn = document.getElementById('mini-toggle');
  if (btn) btn.setAttribute('aria-pressed', miniOpen ? 'true' : 'false');
  emit('shell', shellState);
}

function listen(name, cb) {
  if (!isTauri || !T.event) return;
  T.event.listen(name, (e) => cb(e.payload || {})).catch((err) => console.error(`[orbit] listen ${name} failed`, err));
}

function syncBackend() {
  if (!isTauri || !T.event) return;
  const s = store.settings;
  const payload = {};
  for (const k of SYNC_KEYS) payload[k] = !!s[k];
  const text = JSON.stringify(payload);
  if (text === lastSync) return;
  lastSync = text;
  T.event.emit('orbit://shell-settings', payload).catch((err) => console.error('[orbit] shell settings sync failed', err));
}

async function syncHotkey() {
  const enabled = !!store.settings.hotkeyEnabled;
  if (enabled === lastHotkey) return;
  lastHotkey = enabled;
  try {
    await call('set_hotkey_enabled', { enabled });
  } catch (err) {
    console.error('[orbit] set_hotkey_enabled failed', err);
  }
  await refreshShellState();
}

function paletteItems() {
  return [{
    label: miniOpen ? 'Close mini mode' : 'Mini mode',
    search: 'Mini mode small window always on top compact readout widget',
    icon: 'i-mini',
    hint: 'Window',
    run: () => toggleMini(),
  }];
}

/** Called once by app.js at boot, not awaited. */
export async function init() {
  if (started) return;
  started = true;

  const btn = document.getElementById('mini-toggle');
  if (btn) {
    btn.hidden = false;
    btn.setAttribute('aria-pressed', 'false');
    btn.addEventListener('click', () => toggleMini());
  }
  registerCommands(paletteItems);

  listen('orbit://shell-mini', (p) => setMiniOpen(p.open));
  listen('orbit://shell-action', (p) => {
    if (p.action === 'togglePause') setSettings({ paused: !store.settings.paused });
  });

  on('settings', () => {
    syncBackend();
    syncHotkey();
  });
  syncBackend();
  await syncHotkey();

  // Start with Windows can change outside Orbit (Task Manager, the Startup view):
  // the registry is the truth, so the setting follows it.
  const state = shellState || (await refreshShellState());
  if (state && state.autostart !== !!store.settings.startWithWindows) {
    setSettings({ startWithWindows: !!state.autostart });
  }
}
