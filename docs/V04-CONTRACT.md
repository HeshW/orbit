# Orbit 0.4 contract (for the feature agents)

Read `CONTRACT.md` (including Additions) first: everything there still holds. This file
adds the 0.4 shapes, commands and file ownership so about ten agents can build in
parallel, each in its own git worktree, without touching each other's files.

Follow it to the letter. If your feature needs a change to a shared file, do not make
it: list the exact change in `contractChanges` in your report and the merge step applies it.

## Hard rules (every agent)

- Never touch the owner's installed Orbit (`%LOCALAPPDATA%\Orbit`) or a running orbit.exe
  you did not start. Stop only processes you started, by the PID you got, and confirm none
  remain at the end.
- Never sign out, disconnect or message the owner's real Windows session. Never trigger
  UAC or elevation prompts. Never write GitHub secrets. Never write the Winlogon, Image
  File Execution Options or Run/RunOnce registry keys (single exception: the shell agent
  may test the autostart plugin and must revert it). Never push, tag, merge to main, or
  create GitHub repos or releases. Never delete files outside your own work.
- If a safety check or permission denial blocks you, stop that part, do not work around
  it, and report it.
- Windows 10, PowerShell and Git Bash. cargo is in `%USERPROFILE%\.cargo\bin` (prepend to
  PATH). Node 24. Use your own `CARGO_TARGET_DIR` under your scratchpad.
- Design: LegacyTech design system as already applied. Reuse the CSS variables and shared
  classes (`CONTRACT.md` "Styling contract", `ui/css/app.css`, settings panels use
  `lv-*` from `lists.css`). No emoji. Sentence-case, short verb labels. Plain, specific
  copy. All four themes (night, void, signal, daylight) must work; text contrast >= 4.5:1.
- Unelevated first: show what can be read without admin and put "Try Restart as
  administrator." on what needs it. Destructive actions go through
  `confirmDialog({ danger: true })`.
- Each agent writes `docs/features/<key>.md` describing what it built, how to test it,
  and what it could not verify.

## Ownership

Only edit the files of your feature. Everything else is read-only for you.

| key | files you may edit |
|---|---|
| data | `src-tauri/src/{sampler,pdh,winenum}.rs`, `src-tauri/src/model.rs` (fill/extend only, no renames), `ui/js/mock/data.js` |
| security | `src-tauri/src/security.rs`, `ui/js/views/security.js`, `ui/js/details.js`, `ui/css/views/security.css`, `ui/js/mock/security.js` |
| autoruns | `src-tauri/src/autoruns.rs`, `ui/js/views/startup.js`, `ui/css/views/autoruns.css`, `ui/js/mock/autoruns.js` |
| network | `src-tauri/src/network.rs`, `ui/js/views/network.js`, `ui/css/views/network.css`, `ui/js/mock/network.js` |
| users | `src-tauri/src/users.rs`, `ui/js/views/users.js`, `ui/css/views/users.css`, `ui/js/mock/users.js` |
| history | `src-tauri/src/history.rs`, `ui/js/views/history.js`, `ui/css/views/history.css`, `ui/js/mock/history.js` |
| tools | `src-tauri/src/tools.rs`, `ui/js/tools.js`, `ui/js/views/processes.js`, `ui/js/panels/backup.js`, `ui/css/views/tools.css`, `ui/js/mock/tools.js` |
| shell | `src-tauri/src/shell.rs`, `src-tauri/src/actions.rs` (restart_as_admin handoff only), `src-tauri/capabilities/default.json`, `src-tauri/tauri.conf.json` (`app.windows` only), `ui/js/shell.js`, `ui/js/panels/behavior.js`, `ui/mini.html`, `ui/js/mini.js`, `ui/css/views/shell.css`, `ui/js/mock/shell.js` |
| perf | `ui/js/views/performance.js`, `ui/css/views/performance.css` |
| quality | `.github/workflows/*`, `tests/*`, `package.json` (repo root, new), `site/*`, `LICENSE`, `README.md` |

Everyone may add `docs/features/<key>.md`.

Nobody edits: `src-tauri/src/lib.rs`, `src-tauri/Cargo.toml` (and `Cargo.lock`),
`ui/index.html`, `ui/js/app.js`, `ui/js/api.js`, `ui/js/store.js`, `ui/js/palette.js`,
`ui/js/views/settings.js`, `docs/CONTRACT.md`, this file. Needed changes there go in
`contractChanges`.

## Sidebar and views

Order, keys and section ids are fixed:

| key | view | section | nav icon | head micro |
|---|---|---|---|---|
| 1 | Processes | `#view-processes` | `i-list` | `01 / Processes` |
| 2 | Performance | `#view-performance` | `i-chart` | `02 / Performance` |
| 3 | History | `#view-history` | `i-history` | `03 / History` |
| 4 | Startup | `#view-startup` | `i-rocket` | `04 / Startup` |
| 5 | Users | `#view-users` | `i-users` | `05 / Users` |
| 6 | Services | `#view-services` | `i-services` | `06 / Services` |
| 7 | Network | `#view-network` | `i-plug` | `07 / Network` |
| 8 | Security | `#view-security` | `i-radar` | `08 / Security` |
| 9 | Settings | `#view-settings` | `i-gear` | `09 / Settings` |

`app.js` `VIEW_NAMES` follows this order; keys 1-9 switch views; the palette "Go to"
group lists all nine. New view modules (`views/history.js`, `users.js`, `network.js`,
`security.js`) follow the existing view API: `export function mount(rootEl)`,
`onShow()`, `onHide()`; re-render only while visible; keep the `view-head` with the
micro label above. They currently render a placeholder. The Startup view also hosts the
autoruns lists (tasks, services, logon, image hijacks), for example as tabs or sections.

Sprite icons added (24px, stroke 1.6, round caps/joins, `currentColor`): `i-history`,
`i-users`, `i-lock`, `i-tray`, `i-mini`, `i-keyboard`, `i-radar`, `i-plug`, `i-file`,
`i-battery`. Need another icon: report it in `contractChanges` with its SVG; until then
reuse an existing one.

CSS: `ui/css/views/{history,users,network,security,autoruns,shell,tools}.css` exist and
are linked from `index.html` after `lists.css`. Prefix new classes with your feature
(`hist-`, `usr-`, `net-`, `sec-`, `ar-`, `sh-`, `tl-`) so nothing collides.

## Settings

New keys in `store.settings` (defaults, sanitized in `store.js`):

| key | default | meaning |
|---|---|---|
| `showTray` | `true` | show the tray icon |
| `closeToTray` | `false` | the close button hides Orbit to the tray instead of exiting |
| `startWithWindows` | `false` | start at sign-in (autostart plugin, HKCU Run) |
| `startMinimized` | `true` | when started with Windows, start in the tray |
| `hotkeyEnabled` | `true` | Ctrl+Shift+O shows Orbit from anywhere |
| `historyEnabled` | `true` | record App history |
| `historyDays` | `30` | days History shows and keeps, integer clamped 1..90 |

Settings panels: `settings.js` puts two empty `<section class="panel lv-panel"
data-panel="behavior|backup">` in the grid (Behavior after Display, Backup before About)
and calls `mountPanel(sectionEl)` from `ui/js/panels/behavior.js` (shell) and
`ui/js/panels/backup.js` (tools) once. The panel module owns everything inside the
section: use `lv-panel-head`, `lv-panel-title`, `lv-field`, `lv-label`, `lv-help`, the
`.switch` markup (`role="switch"`, `aria-checked`) as the other panels do, and listen to
`on('settings')` itself. Add `lv-panel--wide` to the section if it needs both columns.
If `mountPanel` throws, Settings hides that section.

The Ctrl+Shift+O hotkey is described in the Behavior panel, not in the in-app keyboard
shortcuts table (that table lists keys that work inside Orbit).

## Snapshot additions

`Snapshot` (and `SnapshotDelta`, with the same meaning) gains:

```ts
disks: { name: string,             // PDH PhysicalDisk instance, e.g. "0 C: E:"
         activePct: number|null,   // 0-100
         readBps: number, writeBps: number }[]       // [] when unavailable
gpuEngines: { name: string /*"3D"|"Copy"|"Video Decode"|...*/, usage: number /*0-100*/ }[]
battery: { percent: number /*0-100*/, charging: boolean, acOnline: boolean,
           secondsLeft: number|null } | null          // null: no battery
network.perApp: { available: boolean, reason: string|null }
                // reason is short and human, e.g. "Needs administrator rights."
```

`ProcessRow` gains, and `DynRow` carries every tick:

```ts
sessionId: number|null      // Windows session, 0 = services
netRxBps: number|null       // bytes/sec, null unless network.perApp.available
netTxBps: number|null
```

`store.applyDelta` merges every DynRow field into the cached row, so these arrive in
delta mode with no store change. Store history rings are unchanged: the perf agent keeps
any per-disk or per-engine history inside `performance.js`.

Backend fill: `sampler.rs` sets `disks: []`, `gpuEngines: []`, `battery: None`,
`sessionId: None` (data agent fills them), and takes `netRxBps`/`netTxBps` from
`network::per_app_rates()` and `network.perApp` from `network::per_app_status()` each tick.

## Commands

All registered in `lib.rs` already. Names, arguments and Ok shapes are fixed; JS args are
camelCase. Every command returns `Result<T, String>` unless shown otherwise; errors are
short sentences ("Access denied. Try Restart as administrator."). You may switch a command
between `fn` and `async fn`, and add Tauri-injected parameters (`app: AppHandle`,
`State<...>`, `window: WebviewWindow`) without a contract change. Stubs return
`Err("Not built yet.")` unless noted. Shapes below are the JSON the UI sees.

**security.rs**

| command | args | Ok |
|---|---|---|
| `get_signatures` | `paths: string[]` | `{ [path]: SignatureInfo }` |
| `security_scan` | – | `SecurityFinding[]` |
| `file_sha256` | `path: string` | `string` (lowercase hex) |
| `get_process_modules` | `pid: number, startTime?: number` | `ModuleInfo[]` |
| `get_process_environment` | `pid: number` | `EnvVar[]` |

```ts
SignatureInfo { status: "signed"|"unsigned"|"invalid"|"unknown", publisher: string|null }
SecurityFinding { pid, name, exe: string|null, severity: "high"|"medium"|"info",
  reasons: string[] /*plain sentences*/, signatureStatus: SignatureInfo["status"],
  publisher: string|null }
ModuleInfo { path: string, size: number /*bytes*/ }
EnvVar { name: string, value: string }
```

**autoruns.rs** (read only)

| command | args | Ok |
|---|---|---|
| `list_autoruns` | – | `Autoruns` |

```ts
Autoruns {
  tasks: { name /*task path*/, path /*exe, "" if none*/, author: string|null, enabled: boolean,
           triggers: string[] /*"At log on", "Daily at 03:00"*/, command: string,
           publisher: string|null, signatureStatus: string|null }[]
  services: { name, displayName, path, startType /*as ServiceEntry*/, publisher: string|null,
              signatureStatus: string|null, microsoft: boolean }[]
  logon: { shell: string, userinit: string, shellOk: boolean, userinitOk: boolean }
  imageHijacks: { image: string /*"taskmgr.exe"*/, debugger: string }[]
}
```

**network.rs**

| command | args | Ok |
|---|---|---|
| `list_connections` | – | `Connection[]` |
| `per_app_network_status` | – | `{ available, reason }` (plain value, not Result) |

```ts
Connection { protocol: "tcp"|"udp", family: "ipv4"|"ipv6", localAddress: string,
  localPort: number, remoteAddress: string|null, remotePort: number|null,
  state: string|null /*camelCase TCP state: "listen"|"established"|"timeWait"|...; null for UDP*/,
  pid: number }
```

Non-command functions the shared code calls (signatures fixed):
`pub fn per_app_rates() -> Option<HashMap<u32, (f64, f64)>>` (pid -> rx, tx bytes/sec;
called by the sampler every tick, must return fast; stub `None`),
`pub fn per_app_status() -> PerAppStatus` (every tick), `pub fn shutdown()` (on
`RunEvent::Exit`; stop any ETW session there).

**users.rs**

| command | args | Ok |
|---|---|---|
| `list_sessions` | – | `SessionInfo[]` |
| `session_action` | `sessionId: number, action: "disconnect"|"signout"|"message", message?: string` | `null` |

```ts
SessionInfo { sessionId: number, userName: string, domain: string,
  state: "active"|"disconnected"|"connected"|"idle"|"listen"|"other",
  clientName: string|null, logonTime: number|null /*Unix secs*/, isCurrent: boolean }
```

Per-session CPU and memory come from the snapshot (`sessionId` on each row). Test the
actions only against the mock: never against the real session.

**history.rs**

| command | args | Ok |
|---|---|---|
| `get_history` | `days: number` | `HistoryRow[]` |
| `clear_history` | – | `null` |
| `set_history_enabled` | `enabled: boolean` | `null` (stub returns Ok) |

```ts
HistoryRow { key /*lowercased exe path, or lowercased name*/, name, exe: string|null,
  cpuSeconds: number, maxMemory: number, avgMemory: number /*bytes*/,
  diskReadBytes, diskWriteBytes /*I/O, as ProcessRow*/, netRxBytes, netTxBytes /*0 without per-app data*/,
  activeSeconds: number, firstSeen: number, lastSeen: number /*Unix secs*/ }
```

Non-command: `pub fn init(app: &AppHandle)` (setup hook, may run after the first
`record`), `pub fn record(snapshot: &Snapshot, interval_secs: f64)` (sampler thread, every
tick; `interval_secs` = seconds this sample stands for, capped at two intervals, 0 for the
first; no disk write per call), `pub fn flush()` (on `RunEvent::Exit`). The UI calls
`set_history_enabled(settings.historyEnabled)` at mount and on change. The store lives in
the Tauri app-local-data folder (`app.path().app_local_data_dir()`, i.e.
`%LOCALAPPDATA%\me.legacytech.orbit`), never in the install folder `%LOCALAPPDATA%\Orbit`.
An `ORBIT_HISTORY_DIR` environment variable overrides it, and tests must use it with a
scratch folder.

**tools.rs**

| command | args | Ok |
|---|---|---|
| `find_file_lockers` | `path: string` | `Locker[]` |

```ts
Locker { pid: number, name: string, exe: string|null,
  appType: "app"|"service"|"explorer"|"console"|"critical"|"unknown" }
```

**shell.rs**

| command | args | Ok |
|---|---|---|
| `get_shell_state` | – | `ShellState` (plain value) |
| `set_autostart` | `enabled: boolean` | `null` |
| `set_hotkey_enabled` | `enabled: boolean` | `null` |
| `toggle_mini` | – | `null` |

```ts
ShellState { trayAvailable: boolean, autostart: boolean, hotkeyRegistered: boolean,
  hotkeyError: string|null }
```

Non-command hooks lib.rs calls: `pub fn configure(builder: Builder<Wry>) -> Builder<Wry>`
(first thing on the fresh builder: register single-instance first, then global-shortcut
and autostart here), `pub fn setup(app: &AppHandle)` (setup hook, after the sampler
starts: tray, hotkey Ctrl+Shift+O, close-to-tray window events), `pub fn
on_run_event(app: &AppHandle, event: &RunEvent)` (every run event, before Orbit's own
exit handling; use it for `ExitRequested` when closing to tray). The mini window is a
second window (label `mini`, `mini.html`) the shell agent adds in `tauri.conf.json` or
creates at runtime, with its capability in `capabilities/default.json`. The titlebar
button `#mini-toggle` (icon `i-mini`, aria-label "Mini mode") starts `hidden`; `shell.js`
un-hides it and wires it to `toggle_mini`.

## lib.rs order (already in place)

```rust
shell::configure(tauri::Builder::default())
    .plugin(tauri_plugin_updater::Builder::new().build())
    .manage(...)
    .invoke_handler(generate_handler![ ...v0.3 commands..., every command above ])
    .setup(|app| { history::init(app.handle()); sampler::start(..); shell::setup(app.handle()); ... })
    .run(|app, event| {
        shell::on_run_event(app, &event);
        if let RunEvent::Exit = event { actions::resume_all(app); network::shutdown(); history::flush(); }
    });
```

## Cargo (already in place)

`tauri` features `["tray-icon", "image-png"]`; `tauri-plugin-global-shortcut = "2"`,
`tauri-plugin-autostart = "2"`, `tauri-plugin-single-instance = "2"` (not registered:
shell does that in `configure`); `sha2 = "0.10"`; `windows` 0.58 adds
`Win32_NetworkManagement_IpHelper`, `Win32_NetworkManagement_Ndis`,
`Win32_Networking_WinSock`, `Win32_Security_Cryptography`,
`Win32_Security_Cryptography_Catalog`, `Win32_Security_WinTrust`,
`Win32_System_Diagnostics_Etw`, `Win32_System_Ole`, `Win32_System_Power`,
`Win32_System_RestartManager`, `Win32_System_TaskScheduler`, `Win32_System_Variant`.
Need another crate or feature: report it in `contractChanges`.

## Frontend registries

**Boot modules.** `app.js` imports `ui/js/shell.js` and `ui/js/tools.js` after the views,
details drawer and palette, and calls `init()` on each without awaiting it. `init` may be
async; errors are logged and never stop boot.

**Palette registry.** `palette.js` exports `registerCommands(provider)`; `provider()` is
called on every palette render and returns items in the built-in shape
`{ label, search?, icon, hint?, danger?, run }`. They are listed in the Actions group
after the built-in actions and ranked the same way. Returns an unregister function.
Register from your own module (for example in `init()` or `mount()`), never by editing
`palette.js`.

**Mock registry.** `api.js` imports `ui/js/mock/index.js`, which merges the default export
of `mock/{data,security,autoruns,network,users,history,tools,shell}.js`. Each default
export is `{ command_name(args, ctx) {...} }`; a handler returns the Ok value (or a
Promise) or throws a short string like the Rust error. `ctx = { snapshot(), isAdmin(),
delay(ms) }`. A registry handler with the same name as a built-in one in `api.js`
replaces it. Mock modules must not import `api.js`. `mock/data.js` also exports
`decorateRow(row)` and `decorateSnapshot(snap)`, which `api.js` calls for every mock row
and snapshot; they fill the 0.4 fields (two disks, three GPU engines, a battery,
`sessionId` 1 for the user's processes and 0 for services, `netRxBps`/`netTxBps` null,
`perApp { available: false, reason: "Needs administrator rights." }`). Every view must
run in a plain browser against the mock (`ui/` served over HTTP).

## Verification each agent runs

- `cargo build` and `cargo test` in `src-tauri` (own `CARGO_TARGET_DIR`).
- `node --check` on every changed `ui/js` file.
- Serve `ui/` over HTTP (`.js` as `text/javascript`), open it, check your view in all four
  themes with no console errors, and that the other eight views still open.
- Report: files changed, verification done, what is untested, and `contractChanges`.
