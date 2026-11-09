# Feature: shell (v0.4)

Tray icon, close to tray, start with Windows, the Ctrl+Shift+O hotkey, single instance and
mini mode. Commands and shapes as in `docs/V04-CONTRACT.md`; nothing renamed.

Files: `src-tauri/src/shell.rs`, `src-tauri/src/actions.rs` (restart_as_admin only),
`src-tauri/capabilities/default.json`, `src-tauri/tauri.conf.json` (`app.windows`),
`ui/js/shell.js`, `ui/js/panels/behavior.js`, `ui/mini.html`, `ui/js/mini.js`,
`ui/css/views/shell.css`, `ui/js/mock/shell.js`.

## Backend (`shell.rs`)

| piece | how |
|---|---|
| `configure` | Waits for `--await-pid <pid>` (up to 10 s, `OpenProcess(SYNCHRONIZE)` + `WaitForSingleObject`), then registers single-instance (callback: show, unminimize, focus the main window), global-shortcut (one handler: toggle on Pressed) and autostart (`args ["--minimized"]`). |
| Restart as administrator | `actions::restart_as_admin` now passes `--await-pid <own pid>` to the elevated Orbit, so it waits for the old one to exit before taking the single-instance lock instead of handing its arguments over and quitting. Elevated, Orbit also lets the single-instance message window accept `WM_COPYDATA` (`ChangeWindowMessageFilterEx`), so a normal second launch still reaches it. |
| Updates | Read the code path: `tauri-plugin-updater` 2.14 `install` on Windows runs the installer with `ShellExecuteW` and then `std::process::exit(0)` itself, so the lock is gone before the installer starts Orbit again. Unaffected. |
| Start | The main window is `visible: false` in `tauri.conf.json`; `setup` shows and focuses it, unless Orbit was launched with `--minimized` and `shell.json` says the tray icon is on and Start minimized is on: then it stays hidden in the tray. |
| Tray | Id `orbit`, the app icon (`default_window_icon`, the Orbit mark), menu Show Orbit / Mini mode / Pause updates (Resume updates when paused) / separator / Quit. Left click (button up) shows and focuses Orbit; the menu opens on right click. Tooltip `Orbit · CPU 23% · Memory 61%` (`Orbit · Paused · ...` while paused) from `AppState.latest`, refreshed every 2 s on a small thread, only when the text changes. Created once; Show tray icon off hides it (`set_visible(false)`, NIS_HIDDEN) instead of deleting it. Removed on `RunEvent::Exit`. |
| Pause from the tray | The UI owns pausing, so the menu sends `orbit://shell-action { action: "togglePause" }` to the main window; `shell.js` flips `settings.paused`, and the label follows the `paused` value the UI sends back. |
| Close to tray | Main window `CloseRequested`: when Keep running in the tray is on and the tray icon is shown, hide (and tell WebView2 it is hidden, so it stops drawing) instead of closing. Otherwise, if the mini window is open, `app.exit(0)` so closing the main window still quits. Quit in the tray menu is `app.exit(0)`, so `RunEvent::Exit` (resume suspended processes, ETW, history flush) still runs. |
| Hotkey | Ctrl+Shift+O. Not in front (hidden, minimized or not focused): show and focus. In front: hide to the tray, or minimize when the tray icon is off (nothing to come back from). `set_hotkey_enabled` registers/unregisters and is always Ok; a combination another app holds gives `hotkeyError` "Another app is using Ctrl+Shift+O." (global-hotkey's `AlreadyRegistered`), other failures "Windows didn't accept Ctrl+Shift+O: ...". |
| Start with Windows | `set_autostart` uses the autostart plugin (HKCU `Run\Orbit` = `"<exe> --minimized"`, value name = productName). Turning it on while Orbit runs elevated is refused ("Orbit is running as administrator. Open it normally to turn this on."): the plugin would write the machine-wide HKLM Run key. Turning it off also removes the `Explorer\StartupApproved\Run\Orbit` value the plugin's enable writes, so nothing is left behind. `autostart` in `get_shell_state` is the plugin's `is_enabled` (Run value present and not disabled in Task Manager). |
| Mini window | `toggle_mini` (async: creating a window from a sync command can deadlock) creates label `mini`, `mini.html`, 300x170 logical, frameless, always on top, not resizable, no taskbar button; or destroys it. Position: the saved one when its title strip is on a monitor's work area, else the bottom-right corner of the work area of the monitor Orbit is on (16 px margin). Moves are remembered in memory and written to `shell.json` when it closes and on exit. `orbit://shell-mini { open }` goes to every window on open and close. |
| `shell.json` | `{ showTray, closeToTray, startMinimized, hotkeyEnabled, miniX, miniY }` in the app local data folder (`%LOCALAPPDATA%\me.legacytech.orbit`), or `ORBIT_SHELL_DIR`. Written only when something changed. The UI's settings stay the source of truth; this file lets the next start decide before the page loads. |

Events (JS to Rust, all `listen_any`): `orbit://shell-settings { showTray, closeToTray,
startMinimized, paused }` (shell.js, at boot and on change) and `orbit://shell-show` (the
mini window's Open Orbit). `core:event:default` already allows emit, so no new permission.

Tests (`cargo test --lib shell::`): `--await-pid` parsing; waiting returns at once for a gone
pid or our own pid and waits for a real child; tooltip text; hotkey error text; mini
placement (on-screen check across two monitors, corner position, small screens);
`shell.json` defaults and the settings payload.

## UI

**`shell.js`** `init()`: un-hides `#mini-toggle` (aria-pressed follows the mini window) and
wires it to `toggle_mini`; registers the palette command "Mini mode" ("Close mini mode" while
open, icon `i-mini`); sends `orbit://shell-settings` at boot and on change; calls
`set_hotkey_enabled(settings.hotkeyEnabled)` at boot and on change; listens to
`orbit://shell-action` and `orbit://shell-mini`. Start with Windows can change outside Orbit,
so at boot `settings.startWithWindows` follows `get_shell_state().autostart`. Exports
`getShellState()`, `refreshShellState()` (also emits store event `'shell'`), `toggleMini()`,
`isMiniOpen()`. In the browser preview the mini toggle only flips the pressed state and says
the readout is at `mini.html`.

**`panels/behavior.js`**: switches Show tray icon, Keep running in the tray when closed,
Start with Windows, Start minimized to tray, Ctrl+Shift+O opens Orbit (with a status line:
"Ready. Press Ctrl+Shift+O in any app.", "Off.", or the error plus "Close that app, or turn
this off." in `--danger`), and a Mini mode button (Open mini / Close mini). Switches use
`data-sh-switch` so settings.js's generic `data-switch` handler leaves them alone.
Dependent switches keep their value but are disabled with the reason in the help line:
close to tray and start minimized need the tray icon; start minimized needs Start with
Windows; Start with Windows can't be turned on while elevated. Start with Windows calls
`set_autostart` first and saves the setting only when it worked.

**`mini.html` / `mini.js`**: header (mark, "Orbit", Live/Paused, Open Orbit, close) and five
rows (CPU, Memory, Disk, Network, GPU) with value and a 60-sample sparkline (`charts.sparkline`,
chart colour variables). Disk shows active time, or read + write per second when Windows has no
active-time counter. Fills from `get_snapshot` at once, then `orbit://snapshot` (full or
delta). Theme, reduced motion and Live/Paused follow `orbit.settings` through the `storage`
event. Esc or the close button calls `toggle_mini`; Open Orbit emits `orbit://shell-show`.
The whole window drags (`data-tauri-drag-region`). Contains its own one-symbol sprite
(`i-close`).

**`mock/shell.js`**: `?hotkeytaken` (another app holds the hotkey), `?notray` (no tray),
refuses `set_autostart(true)` when the mock is elevated.

## Verified

- `cargo build` (no warnings), `cargo test` (96 passed), `node --check` on every `ui/js` file.
- Browser preview (mock): Behavior panel switches, dependencies and status lines, hotkey taken
  (`?hotkeytaken`), the palette command, all nine views open, no console errors; mini.html at
  300x170 in all four themes. Contrast (reduced motion on, so no transitions mid-way): mini
  labels 6.6-13.6:1, values and title 17.6-20.3:1, Live 4.9-13.9:1, Open Orbit 11.8-17.5:1,
  close icon 5.3-12.1:1; the hotkey error line 6.5-9.0:1.
- Live, debug exe over CDP (port 9408, scratch WebView2 folder, `ORBIT_SHELL_DIR` and
  `ORBIT_HISTORY_DIR` in the scratchpad):
  - Starts visible with tray, hotkey registered; settings reach `shell.json`.
  - Close with Keep running in the tray on: window hidden, `document.hidden` true, process alive.
    A second launch exits with code 0 and the first shows and focuses.
  - Mini: opens (always on top, 300x170 inner), live values, pressed state in the titlebar,
    0 console errors in both windows; the close button closes it and saves the position;
    a saved position (200, 200) is reused; follows Night/Daylight and shows Paused; Open Orbit
    restores a minimized main window and focuses it. Closing the main window while the mini is
    open quits Orbit.
  - Tray Pause: the `togglePause` event flips `settings.paused` in the UI.
  - Hotkey (real key presses from a hidden helper): with the tray, hide/show alternate; without
    it, minimize/restore alternate. Off unregisters, on registers again. Held by another process
    (`RegisterHotKey` in PowerShell): "Another app is using Ctrl+Shift+O." in state and panel;
    turning it off and on after release registers it.
  - Start with Windows: on wrote `HKCU\...\Run\Orbit` = `E:\Work\.orbit-target\debug\orbit.exe
    --minimized` (and the StartupApproved value); launched with `--minimized`, the window stayed
    hidden with the tray up; off removed both values. Checked at the end: no `Orbit` value in
    HKCU Run, HKCU StartupApproved\Run or HKLM Run.
  - `--await-pid`: B launched with A's pid waited while A ran, became the instance after A
    exited (hotkey registered), and a third launch was turned away.
  - Tray icons (TB_BUTTONCOUNT on the tray toolbars): exactly one more while Orbit runs, hidden
    (not removed) with Show tray icon off, gone after a normal exit. A test instance that had to
    be force-killed (hidden windows don't get taskkill's WM_CLOSE) left one ghost icon that
    clears on hover.

## Not verified

- Clicking the tray icon and its menu items for real: the icon sits in the collapsed overflow
  area, where a posted click has no icon rect and tray-icon drops it. Left click and Show
  Orbit call the same `show_main` the single-instance and Open Orbit paths use; Mini mode
  calls `toggle_mini_window`; Pause sends the event tested above; Quit is `app.exit(0)`. The
  tooltip text is unit tested, not read off the screen.
- Anything elevated: Restart as administrator end to end, the `WM_COPYDATA` filter, and the
  elevated refusal in `set_autostart` (only the mock path).
- A real sign-in start, and the updater relaunch (read the code only).
- After a restore from minimized by the hotkey, Windows sometimes leaves focus elsewhere
  (seen once in eight presses); the window is shown either way.
- Updates exit with `process::exit(0)` and skip `RunEvent::Exit`, so the tray icon can linger
  as a ghost until hovered after an update (Windows clears it).
