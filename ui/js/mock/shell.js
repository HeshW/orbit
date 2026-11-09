// Orbit / mock/shell: tray, autostart, hotkey and mini window state for the browser
// preview (shell feature, docs/V04-CONTRACT.md). Nothing here touches the real system.

const state = { trayAvailable: true, autostart: false, hotkeyRegistered: true, hotkeyError: null, mini: false };

export default {
  get_shell_state() {
    const { mini: _m, ...out } = state;
    return out;
  },

  set_autostart({ enabled }) {
    state.autostart = !!enabled;
    return null;
  },

  set_hotkey_enabled({ enabled }) {
    state.hotkeyRegistered = !!enabled;
    state.hotkeyError = null;
    return null;
  },

  toggle_mini() {
    // The browser preview has one window: remember the state only.
    state.mini = !state.mini;
    return null;
  },
};
