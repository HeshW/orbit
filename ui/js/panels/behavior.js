// Orbit / panels/behavior: the Behavior panel in Settings (shell feature, docs/V04-CONTRACT.md).
// settings.js calls mountPanel(el) once with an empty <section class="panel lv-panel">
// in the Settings grid; this module owns everything inside it.
//
// Switches use data-sh-switch (not settings.js's data-switch) so only this module handles
// them. Start with Windows goes through set_autostart; the others are plain settings that
// shell.js passes to the backend. Store event 'shell' carries the ShellState.

import { call } from '../api.js';
import { store, on, setSettings } from '../store.js';
import { toast } from '../app.js';
import { getShellState, refreshShellState, toggleMini, isMiniOpen } from '../shell.js';

const FIELDS = [
  {
    key: 'showTray',
    label: 'Show tray icon',
    help: 'Keep Orbit in the notification area. Hover it for CPU and memory, click it to open Orbit.',
  },
  {
    key: 'closeToTray',
    label: 'Keep running in the tray when closed',
    help: 'The close button hides Orbit instead of quitting. Quit from the tray icon’s menu.',
  },
  {
    key: 'startWithWindows',
    label: 'Start with Windows',
    help: 'Open Orbit when you sign in.',
  },
  {
    key: 'startMinimized',
    label: 'Start minimized to tray',
    help: 'When Orbit starts with Windows, it waits in the tray instead of opening its window.',
  },
  {
    key: 'hotkeyEnabled',
    label: 'Ctrl+Shift+O opens Orbit',
    help: 'Show Orbit from any app, or hide it again when it is in front.',
  },
];

let root = null;
let busyAutostart = false;

function switchHtml(key, labelId) {
  return `<button type="button" class="switch" role="switch" data-sh-switch="${key}" aria-checked="false" aria-labelledby="${labelId}" aria-describedby="${labelId}-h"><span class="switch-thumb" aria-hidden="true"></span></button>`;
}

/** Render the panel into `containerEl`. */
export function mountPanel(containerEl) {
  if (!containerEl) return;
  root = containerEl;
  root.setAttribute('aria-labelledby', 'set-behavior-h');
  root.innerHTML = `
    <div class="lv-panel-head">
      <h2 class="lv-panel-title" id="set-behavior-h">Behavior</h2>
      <span class="micro">Tray / Startup / Hotkey</span>
    </div>
    ${FIELDS.map((f) => `
    <div class="lv-field" data-sh-field="${f.key}">
      <div class="lv-field-text">
        <span class="lv-label" id="sh-${f.key}-l">${f.label}</span>
        <span class="lv-help" id="sh-${f.key}-l-h" data-sh-help="${f.key}">${f.help}</span>
        ${f.key === 'hotkeyEnabled' ? '<span class="lv-help sh-status" data-sh-hotkey role="status"></span>' : ''}
      </div>
      ${switchHtml(f.key, `sh-${f.key}-l`)}
    </div>`).join('')}
    <div class="lv-field">
      <div class="lv-field-text">
        <span class="lv-label" id="sh-mini-l">Mini mode</span>
        <span class="lv-help">A small window that stays on top with live CPU, memory, disk, network and GPU.</span>
      </div>
      <button type="button" class="btn btn--sm" data-sh-mini aria-describedby="sh-mini-l">Open mini</button>
    </div>`;

  root.addEventListener('click', (ev) => {
    const sw = ev.target.closest('[data-sh-switch]');
    if (sw && root.contains(sw)) {
      if (sw.getAttribute('aria-disabled') === 'true') return;
      onSwitch(sw.dataset.shSwitch);
      return;
    }
    if (ev.target.closest('[data-sh-mini]')) toggleMini();
  });

  on('settings', render);
  on('shell', render);
  render();
  if (!getShellState()) refreshShellState();
}

async function onSwitch(key) {
  const next = !store.settings[key];
  if (key !== 'startWithWindows') {
    setSettings({ [key]: next });
    return;
  }
  if (busyAutostart) return;
  busyAutostart = true;
  render();
  try {
    await call('set_autostart', { enabled: next });
    setSettings({ startWithWindows: next });
    if (next) toast('Orbit will start when you sign in.', 'success');
  } catch (err) {
    toast(String(err || 'Windows did not save the startup entry.'), 'error');
  } finally {
    busyAutostart = false;
    await refreshShellState();
  }
}

function setSwitch(key, checked, disabled) {
  const sw = root.querySelector(`[data-sh-switch="${key}"]`);
  if (!sw) return;
  sw.setAttribute('aria-checked', checked ? 'true' : 'false');
  if (disabled) sw.setAttribute('aria-disabled', 'true');
  else sw.removeAttribute('aria-disabled');
}

function setHelp(key, text) {
  const el = root.querySelector(`[data-sh-help="${key}"]`);
  if (el && el.textContent !== text) el.textContent = text;
}

function render() {
  if (!root) return;
  const s = store.settings;
  const st = getShellState();
  const isAdmin = Boolean(store.staticInfo && store.staticInfo.isAdmin);
  const trayOk = !st || st.trayAvailable;
  const help = Object.fromEntries(FIELDS.map((f) => [f.key, f.help]));

  setSwitch('showTray', s.showTray && trayOk, !trayOk);
  setHelp('showTray', trayOk ? help.showTray : 'Windows did not create the tray icon. Restart Orbit to try again.');

  const trayOn = s.showTray && trayOk;
  setSwitch('closeToTray', s.closeToTray, !trayOn);
  setHelp('closeToTray', trayOn ? help.closeToTray : 'Needs the tray icon.');

  // Elevated, turning it on would add a machine-wide entry, so the backend refuses;
  // turning it off still works.
  const autoLocked = isAdmin && !s.startWithWindows;
  setSwitch('startWithWindows', s.startWithWindows, busyAutostart || autoLocked);
  setHelp('startWithWindows', autoLocked ? 'Orbit is running as administrator. Open it normally to turn this on.' : help.startWithWindows);

  const minOk = trayOn && s.startWithWindows;
  setSwitch('startMinimized', s.startMinimized, !minOk);
  setHelp('startMinimized', minOk ? help.startMinimized
    : !trayOn ? 'Needs the tray icon.' : 'Turn on Start with Windows first.');

  setSwitch('hotkeyEnabled', s.hotkeyEnabled, false);
  const status = root.querySelector('[data-sh-hotkey]');
  if (status) {
    let text;
    let error = false;
    if (!s.hotkeyEnabled) text = 'Off.';
    else if (!st) text = 'Checking.';
    else if (st.hotkeyRegistered) text = 'Ready. Press Ctrl+Shift+O in any app.';
    else {
      text = `${st.hotkeyError || 'Windows did not accept Ctrl+Shift+O.'} Close that app, or turn this off.`;
      error = true;
    }
    if (status.textContent !== text) status.textContent = text;
    status.classList.toggle('sh-status--error', error);
  }

  const mini = root.querySelector('[data-sh-mini]');
  if (mini) {
    const label = isMiniOpen() ? 'Close mini' : 'Open mini';
    if (mini.textContent !== label) mini.textContent = label;
  }
}
