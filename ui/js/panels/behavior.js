// Orbit / panels/behavior: the Behavior panel in Settings (shell feature, docs/V04-CONTRACT.md).
// settings.js calls mountPanel(el) once with an empty <section class="panel lv-panel">
// in the Settings grid; this module owns everything inside it.

/** Render the panel into `containerEl`. v0.4 scaffold: a placeholder. */
export function mountPanel(containerEl) {
  if (!containerEl) return;
  containerEl.setAttribute('aria-labelledby', 'set-behavior-h');
  containerEl.innerHTML = `
    <div class="lv-panel-head">
      <h2 class="lv-panel-title" id="set-behavior-h">Behavior</h2>
      <span class="micro">Tray / Startup / Hotkey</span>
    </div>
    <p class="lv-help">Tray icon, close to tray, start with Windows and the Ctrl+Shift+O shortcut are coming in Orbit 0.4.</p>`;
}
