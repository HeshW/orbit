// Orbit / mock/shell: tray, autostart, hotkey and mini window state for the browser
// preview (shell feature, docs/V04-CONTRACT.md). Nothing here touches the real system.
// ?hotkeytaken in the URL: another app holds Ctrl+Shift+O. ?notray: no tray icon.

const params = typeof location !== 'undefined' ? new URLSearchParams(location.search) : new URLSearchParams();
const TAKEN = params.has('hotkeytaken');

const state = {
  trayAvailable: !params.has('notray'),
  autostart: false,
  hotkeyRegistered: !TAKEN,
  hotkeyError: TAKEN ? 'Another app is using Ctrl+Shift+O.' : null,
  mini: false,
};

export default {
  get_shell_state() {
    const { mini: _m, ...out } = state;
    return out;
  },

  set_autostart({ enabled }, ctx) {
    if (enabled && ctx && ctx.isAdmin()) throw 'Orbit is running as administrator. Open it normally to turn this on.';
    state.autostart = !!enabled;
    return null;
  },

  set_hotkey_enabled({ enabled }) {
    if (!enabled) {
      state.hotkeyRegistered = false;
      state.hotkeyError = null;
    } else if (TAKEN) {
      state.hotkeyRegistered = false;
      state.hotkeyError = 'Another app is using Ctrl+Shift+O.';
    } else {
      state.hotkeyRegistered = true;
      state.hotkeyError = null;
    }
    return null;
  },

  toggle_mini() {
    // The browser preview has one window: remember the state only.
    state.mini = !state.mini;
    return null;
  },
};
