# Feature: Startup list stays in sync, Store app startup tasks, hide suggestions (v0.8, 10b and 10c)

What the Startup tab shows now follows changes made outside Orbit, lists Store (MSIX) apps that
start through a startup task, and the "These N apps add the most to your sign-in" card can be hidden.
Shapes are in `docs/CONTRACT.md` ("v0.8").

Files: `src-tauri/src/startup.rs`, `startup/packaged.rs` (new), `startup/watch.rs` (new),
`model.rs` (`StartupEntry.packagedState`), `lib.rs` (starts the watcher),
`ui/js/views/startup.js`, `ui/js/views/optimize/suggestions.js`, `ui/js/store.js`,
`ui/css/views/lists.css`, `ui/css/views/optimize/health.css`, `ui/js/api.js` and
`ui/js/mock/health.js` (mock), `tests/ui-smoke.mjs`.

## Staying in sync

- The tab re-reads every time it is shown (the old rows stay until the new ones arrive, so scroll
  position and focus are kept).
- `startup/watch.rs`: one thread, `WaitForMultipleObjects` on `RegNotifyChangeKeyValue` events
  (HKCU Run, HKLM Run, HKLM Run32, the HKCU and HKLM `StartupApproved` trees, the packaged-task
  state tree) and `FindFirstChangeNotificationW` handles (both Startup folders). Nothing runs
  while nothing changes. After a change it waits for 500 ms of quiet, reads the list once and
  emits `orbit://startup-changed` only if it differs from the last `list_startup` result (the
  packaged state tree also changes when apps save window positions). The Startup tab reloads
  when the event arrives while it is visible.

## Store app startup tasks

- Task list: each package of the current user (`PackageManager`, main packages) with a
  `windows.startupTask` extension in its `AppxManifest.xml` (`uap5:` and `desktop:` forms).
  Parsed manifests are cached by package full name.
- State: `HKCU\Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\
  SystemAppData\<PackageFamilyName>\<TaskId>`, value `State` = the `StartupTaskState` numbers
  (0 disabled, 1 disabled by user, 2 enabled, 3 disabled by policy, 4 enabled by policy). With no
  key the manifest's `Enabled` attribute decides. Checked against this PC's Settings > Apps >
  Startup data (Claude, ModernFlyouts, Phone Link enabled; WhatsApp, Teams, Copilot etc. off).
- Read only. Row: locked switch, state in words, location "Store app", button "Open Startup
  settings" (`ms-settings:startupapps`). `set_startup_enabled` refuses `Packaged\...` ids.
- Not in Debloat, the startup suggestions or the impact measurement: those use
  `collect_startup()`, which does not list them. They are not part of Health's "Startup load" count.

## Hiding the suggestions card

`hiddenSuggestions` (UI setting) holds a hash of the suggested apps' ids; the card stays hidden
until that set changes. Startup and Health share it. See CONTRACT "v0.8".

## Tests

- Rust: manifest parsing, state mapping, entries and refusal (`startup::packaged`), the watcher
  against a scratch key `HKCU\Software\OrbitTest\...` and a temp folder, missing targets, the real
  targets arming, fingerprint (`startup::watch`). `cargo test print_this_pc -- --ignored
  --nocapture` lists this PC's Store app tasks.
- UI smoke: "suggestions hide" (Hide label, hidden in Startup and Health, survives a reload,
  returns when the set changes) and "startup sync" (Store app rows read only, Settings button,
  re-read on show, reload on `orbit://startup-changed`).
