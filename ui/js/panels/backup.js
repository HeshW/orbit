// Orbit / panels/backup: the Backup panel in Settings (tools feature, docs/V04-CONTRACT.md).
// settings.js calls mountPanel(el) once with an empty <section class="panel lv-panel">
// in the Settings grid; this module owns everything inside it.

/** Render the panel into `containerEl`. v0.4 scaffold: a placeholder. */
export function mountPanel(containerEl) {
  if (!containerEl) return;
  containerEl.setAttribute('aria-labelledby', 'set-backup-h');
  containerEl.innerHTML = `
    <div class="lv-panel-head">
      <h2 class="lv-panel-title" id="set-backup-h">Backup</h2>
      <span class="micro">Settings / File</span>
    </div>
    <p class="lv-help">Saving your settings to a file and loading them back is coming in Orbit 0.4.</p>`;
}
