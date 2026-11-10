# Orbit build contract (for implementers)

Read `../PROPOSAL.md` first for the product. This file fixes the file layout, the
Rust <-> UI API and the frontend module APIs so parallel work fits together.
Do NOT change a name or shape here without saying so in your final report.

## Layout

```
orbit/
  PROPOSAL.md
  docs/CONTRACT.md
  ui/                         # frontendDist (static, no build step)
    index.html
    assets/fonts/Oxanium.ttf  # variable font, provided
    assets/fonts/Manrope.ttf  # variable font, provided
    assets/mark-dark.svg      # pixel L for dark themes, provided
    assets/mark-light.svg     # pixel L for Daylight, provided
    css/tokens.css            # @font-face + themes as CSS custom properties
    css/app.css               # layout + shared components (foundation)
    css/views/processes.css   # processes view + details drawer (processes owner)
    css/views/performance.css # performance view (performance owner)
    css/views/lists.css       # startup, services, settings (lists owner)
                              # index.html links all four after app.css
    js/api.js                 # Tauri bridge + mock backend for browser preview
    js/store.js               # state, history, settings, events
    js/format.js              # number/bytes/time formatting
    js/charts.js              # canvas charts
    js/app.js                 # boot, router, shortcuts, theme, toasts, titlebar
    js/palette.js             # Ctrl+K command palette
    js/details.js             # process details drawer
    js/views/processes.js
    js/views/performance.js
    js/views/startup.js
    js/views/services.js
    js/views/settings.js
  src-tauri/
    Cargo.toml
    build.rs
    tauri.conf.json
    capabilities/default.json
    icons/icon.ico, icons/icon.png   # provided
    src/main.rs               # calls orbit_lib::run()
    src/lib.rs                # builder, state, registers ALL commands below
    src/model.rs              # serde structs shared by modules
    src/sampler.rs            # background sampling thread + snapshot cmds
    src/pdh.rs                # PDH counters (GPU, disk, threads, handles, commit)
    src/winenum.rs            # top-level windows: titles, not-responding, app pids
    src/actions.rs            # process actions + details + shell helpers + admin
    src/icons.rs              # exe icon -> PNG data URL
    src/startup.rs            # startup entries (+ RunOnce scheduling for impact)
    src/impact.rs             # startup impact: --measure-startup run + result file
    src/services.rs           # Windows services
```

## Rust crates (Cargo.toml, owned by backend-core)

```toml
[package]
name = "orbit"
version = "0.1.0"
edition = "2021"

[lib]
name = "orbit_lib"
crate-type = ["rlib"]

[build-dependencies]
tauri-build = { version = "2", features = [] }

[dependencies]
tauri = { version = "2", features = [] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sysinfo = "0.33"
png = "0.17"
base64 = "0.22"
windows = { version = "0.58", features = [ ...whatever the modules need... ] }

[profile.release]
opt-level = 3
lto = true
codegen-units = 1
strip = true
```

`windows` 0.58 features likely needed (backend-core owns the final list, other modules
list what they need in their report): Win32_Foundation, Win32_Security,
Win32_System_Threading, Win32_System_Diagnostics_ToolHelp, Win32_System_ProcessStatus,
Win32_System_Performance, Win32_System_Services, Win32_System_Registry,
Win32_UI_Shell, Win32_UI_WindowsAndMessaging, Win32_Graphics_Gdi,
Win32_Storage_FileSystem, Win32_System_Com, Win32_System_SystemInformation,
Win32_System_Diagnostics_Debug, Wdk_System_Threading (for NtSuspendProcess /
NtResumeProcess if available; otherwise suspend each thread via Toolhelp32 +
SuspendThread).

`main.rs` must have `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]`.

## Serde rules

All structs `#[derive(Serialize, Deserialize, Clone, Debug)]` with
`#[serde(rename_all = "camelCase")]`. Enums used as strings use
`#[serde(rename_all = "camelCase")]` too. Options serialize as `null`.
Commands return `Result<T, String>` where they can fail; error strings are short,
human, and start with a capital ("Access denied. Try Restart as administrator.").

## Event

Sampler thread emits `"orbit://snapshot"` every tick. Default interval 1000 ms. By default
the payload is a full `Snapshot` (~400 KB with ~480 processes, 60% of it command lines).
After the UI calls `set_sampling({ intervalMs, paused, delta: true })` the payload is a
`SnapshotDelta` instead (~90 KB): static process fields (cmd, exe, user, name, parentPid,
startTime) are sent once per process instead of every tick. `get_snapshot` always returns
the full `Snapshot`; use it for the first load and to resync.

```ts
SnapshotDelta {
  ts, intervalMs, cpu, memory, disk, network, gpu, system   // exactly as in Snapshot
  added: ProcessRow[]      // full rows for (pid, startTime) pairs not in the previous event
  removed: number[]        // pids in the previous event that are gone (or whose pid was
                           // reused: the pid is then in both removed and added).
                           // Apply removed first, then added.
  rows: DynRow[]           // every current process, dynamic fields only
}
DynRow { pid, cpu, memory, diskReadBps, diskWriteBps, gpu, status, kind, windowTitle,
         notResponding, suspended }   // same meaning as in ProcessRow; kind can change
                                      // when a window opens or closes
```
Merge each DynRow into the cached ProcessRow with the same pid. The first event after
switching to delta mode has every process in `added`. If `rows` holds a pid with no cached row (an event
was missed), call `get_snapshot` to resync.

```ts
Snapshot {
  ts: number                     // ms since epoch
  intervalMs: number
  cpu: { usage: number /*0-100*/, perCore: number[], freqMhz: number }
  memory: { total, used, available, swapTotal, swapUsed: number /*bytes*/,
            committed: number|null, commitLimit: number|null,
            cached: number|null,   // Task Manager's "Cached": standby lists + modified list
                                   // (mostly inside `available`)
            free: number|null }    // free + zeroed page lists
            // Composition bar (performance.js): free = free; cached = cached, clamped
            // to fit; in use = the rest. Without `free` it falls back to
            // in use = used, cached = cached ?? 0, free = total - used - cached.
            // swapTotal/swapUsed = commitLimit/committed minus physical RAM.
  disk: { activePct: number|null,
          readBps: number|null,    // physical disks (PDH), null if the counter is missing;
          writeBps: number|null }  // never a sum of per-process I/O. Show a dash for null.
  network: { rxBps: number, txBps: number,
             interfaces: { name: string, rxBps: number, txBps: number,
                           totalRx: number, totalTx: number }[] }
  gpu: { usage: number|null, dedicatedUsed: number|null }
  system: { uptimeSecs: number, processCount: number,
            threadCount: number|null, handleCount: number|null }
  processes: ProcessRow[]
}

ProcessRow {
  pid: number
  parentPid: number|null
  name: string                   // "chrome.exe"
  exe: string|null               // full path, null if access denied
  cmd: string                    // command line joined by spaces, "" if unknown
  user: string|null              // "Hesh", "SYSTEM", ...
  cpu: number                    // 0-100, normalised by logical core count
  memory: number                 // bytes (working set)
  diskReadBps: number            // GetProcessIoCounters: ALL I/O (files, pipes, network),
  diskWriteBps: number           // not physical disk. The UI labels these "I/O".
                                 // (IO_COUNTERS Read/WriteTransferCount rates)
  gpu: number|null               // 0-100
  status: string                 // "running" | "sleeping" | "stopped" | "zombie" | "unknown" ...
  startTime: number              // secs since epoch
  kind: "app" | "background" | "windows"
  windowTitle: string|null       // main visible window title if any
  notResponding: boolean
  suspended: boolean             // true if Orbit suspended it (tracked in backend)
}
```

`kind`: `app` = owns a visible, unowned, titled top-level window (EnumWindows);
`windows` = pid 0/4, or exe under `%SystemRoot%` (C:\Windows), or a known core name
(System, Registry, smss.exe, csrss.exe, wininit.exe, services.exe, lsass.exe,
svchost.exe, winlogon.exe, dwm.exe, fontdrvhost.exe, Memory Compression, Secure System);
otherwise `background`. `app` wins over `windows` (explorer.exe with a window is an app).

## Commands (Tauri `#[tauri::command]`, names exact; JS args camelCase)

| command | args | returns | owner |
|---|---|---|---|
| `get_snapshot` | – | `Snapshot` (latest; sample now if none yet) | core |
| `get_static_info` | – | `StaticInfo` | core |
| `set_sampling` | `intervalMs: u64, paused: bool, delta?: bool` (true: emit `SnapshotDelta`; omitted keeps the mode) | `()` | core |
| `end_task` | `pid: u32, startTime?: u64` | `Result<(), String>` | actions |
| `end_tree` | `pid: u32, startTime?: u64` | `Result<u32, String>` (count ended) | actions |
| `suspend_process` | `pid: u32, startTime?: u64` | `Result<(), String>` | actions |
| `resume_process` | `pid: u32, startTime?: u64` | `Result<(), String>` | actions |
| `get_priority` | `pid: u32` | `Result<String, String>` | actions |
| `set_priority` | `pid: u32, priority: String, startTime?: u64` | `Result<(), String>` | actions |
| `get_process_details` | `pid: u32` | `Result<ProcessDetails, String>` | actions |
| `open_location` | `path: String` | `Result<(), String>` (explorer /select,) | actions |
| `search_online` | `query: String` | `Result<(), String>` (default browser, https://www.bing.com/search?q=<urlencoded>) | actions |
| `open_url` | `url: String` (must start with https://) | `Result<(), String>` | actions |
| `restart_as_admin` | – | `Result<(), String>` (async: ShellExecuteEx "runas" self on a blocking thread, then exit 0; "Elevation was cancelled." when the prompt is declined) | actions |
| `export_report` | `fileName: String, contents: String` | `Result<String, String>` full path written in the user's Downloads folder (sanitize fileName, no path separators) | actions |
| `get_icons` | `paths: Vec<String>` | `HashMap<String, String>` path -> `data:image/png;base64,...` (32x32; skip failures; cache in a Mutex<HashMap>) | icons |
| `list_startup` | – | `Result<Vec<StartupEntry>, String>` | startup |
| `set_startup_enabled` | `id: String, enabled: bool` | `Result<(), String>` | startup |
| `list_services` | – | `Result<Vec<ServiceEntry>, String>` | services |
| `control_service` | `name: String, action: String` ("start"\|"stop"\|"restart") | `Result<(), String>` | services |
| `check_update` | – | `Result<Option<UpdateInfo>, String>` (`{ version, currentVersion, notes, date }`, null when up to date) | updates |
| `install_update` | – | `Result<(), String>` (downloads, resumes processes Orbit suspended, installs passively, restarts) | updates |
| `restart_process` | `pid: u32, startTime?: u64` | `Result<u32, String>` new pid (see Additions) | actions |
| `run_task` | `command: String, asAdmin: bool` | `Result<(), String>` (like the Windows Run box) | actions |
| `get_affinity` | `pid: u32` | `Result<AffinityInfo, String>` | actions |
| `set_affinity` | `pid: u32, mask: String` (decimal u64), `startTime?: u64` | `Result<(), String>` | actions |
| `get_startup_measure` | – | `Result<bool, String>` true when a measurement waits for the next sign-in | startup |
| `schedule_startup_measure` | – | `Result<(), String>` writes the one-shot RunOnce value | startup |
| `cancel_startup_measure` | – | `Result<(), String>` removes it (already gone counts as done) | startup |

Priority strings: `"idle" | "belowNormal" | "normal" | "aboveNormal" | "high" | "realtime"`.

Process actions (`end_task`, `end_tree`, `suspend_process`, `resume_process`,
`set_priority`): pass the row's `startTime` with the pid. The backend checks it against the
process it opened and returns "That process has exited." if the pid now belongs to another
process. Omitted or 0 skips the check. They refuse Orbit's own descendants (its WebView2
processes) with "That process is part of Orbit." (resume is still allowed).
`suspend_process` refuses dwm.exe, winlogon.exe, csrss.exe, wininit.exe, smss.exe,
services.exe, lsass.exe, fontdrvhost.exe and any critical process. `end_tree` checks that
the root can be ended before ending any child; if the root then still fails, the error
reads "Ended N child processes, but ...". Processes Orbit suspended are resumed when
Orbit exits (including Restart as administrator). `exe` is filled for SYSTEM and service
processes too (looked up without opening the process), so `kind` and Open file location
work for them unelevated.

```ts
StaticInfo { cpuBrand: string, logicalCores: number, physicalCores: number|null,
  totalMemory: number, hostName: string|null, osName: string|null,
  osVersion: string|null, kernelVersion: string|null, isAdmin: boolean,
  pid: number /*Orbit's own pid, std::process::id()*/,
  bootTime: number, disks: { name: string, mount: string, fileSystem: string,
  total: number, available: number, kind: string /*"SSD"|"HDD"|"Unknown"*/,
  removable: boolean }[] }

ProcessDetails { pid, parentPid: number|null, parentName: string|null, name, exe: string|null,
  cmd: string, cwd: string|null, user: string|null, startTime: number,
  priority: string|null, threads: number|null, handles: number|null,
  memory: number, virtualMemory: number, children: { pid: number, name: string }[],
  diskTotalRead: number, diskTotalWritten: number }

StartupEntry { id: string /*stable, e.g. "HKCU\\Run\\Discord" or "UserFolder\\X.lnk"*/,
  name: string, command: string, location: string /*"HKCU Run", "HKLM Run",
  "HKLM Run (32-bit)", "Startup folder", "Startup folder (all users)"*/,
  enabled: boolean, path: string|null /*resolved exe if parseable*/,
  needsAdmin: boolean }

ServiceEntry { name: string, displayName: string, status: string /*"running"|"stopped"|
  "startPending"|"stopPending"|"paused"|"pausePending"|"continuePending"|"unknown"*/,
  pid: number|null, startType: string /*"auto"|"autoDelayed"|"manual"|"disabled"|
  "boot"|"system"|"unknown"*/, description: string|null /*may be null for speed*/ }
```

Backend module ownership: **core** = Cargo.toml, build.rs, tauri.conf.json,
capabilities, main.rs, lib.rs, model.rs, sampler.rs, pdh.rs, winenum.rs.
**actions** = actions.rs, icons.rs. **system-lists** = startup.rs, services.rs.
Each non-core module exposes its commands as `pub fn` with `#[tauri::command]`;
core's lib.rs declares `mod actions; mod icons; mod startup; mod services;` and lists
every command in `tauri::generate_handler![...]` as `actions::end_task`, etc.
Shared state: core puts `AppState` in `tauri::State` (via `.manage`). `actions` needs
the suspended set: core defines in model.rs
`pub struct SuspendedSet(pub std::sync::Mutex<std::collections::HashMap<u32, u64>>);`
(pid -> start time, Unix seconds)
managed with `.manage(SuspendedSet(Default::default()))`; actions inserts/removes;
sampler reads it to fill `suspended`. Icons cache: icons.rs defines its own
`pub struct IconCache(pub Mutex<HashMap<String,String>>)`; core manages
`icons::IconCache(Default::default())`. Commands that do blocking work should be
`async` or run quickly; `list_services`/`list_startup`/`get_icons` may be
`#[tauri::command(async)]`-style (`pub async fn`) to stay off the main thread.

tauri.conf.json essentials: productName "Orbit", identifier "me.legacytech.orbit",
`build.frontendDist: "../ui"`, `app.withGlobalTauri: true`, `app.security.csp: null`,
one window label "main", title "Orbit", 1320x840, minWidth 960, minHeight 620,
`decorations: false` (custom titlebar), `backgroundColor` "#050b18" if supported,
`bundle.active: false`, `bundle.icon: ["icons/icon.ico", "icons/icon.png"]`.
capabilities/default.json: windows ["main"], permissions: "core:default",
"core:window:allow-minimize", "core:window:allow-toggle-maximize",
"core:window:allow-close", "core:window:allow-start-dragging",
"core:window:allow-is-maximized", "core:event:default".

## Frontend

Plain ES modules, no framework, no build. `index.html` loads `css/tokens.css`,
`css/app.css`, then `<script type="module" src="js/app.js">`. No external network
requests (fonts are local). Everything must also run in a normal browser via the
mock in api.js (used to preview the UI outside Tauri).

### DOM skeleton (owned by foundation, in index.html)

```
body
  header#titlebar [data-tauri-drag-region]   brand (mark + "Legacy<span class=brand-accent>Tech</span> Orbit"),
                                             #global-search (input#search), #live-indicator, #admin-badge,
                                             window buttons #win-min #win-max #win-close
  nav#sidebar        buttons .nav-item[data-view="processes|performance|startup|services|settings"]
                     each with icon + label + micro index ("01")
  main#main
    section#view-processes.view     (empty; filled by views/processes.js)
    section#view-performance.view
    section#view-startup.view
    section#view-services.view
    section#view-settings.view
  aside#details      (drawer; filled by details.js)
  div#palette        (command palette; filled by palette.js)
  div#toasts
  div#context-menu   (generic, used via app.showMenu)
  svg sprite (hidden) with symbols: i-search, i-cpu, i-memory, i-disk, i-network, i-gpu,
     i-list, i-tree, i-grid, i-rocket(startup), i-gear, i-services(layers), i-close, i-check,
     i-x, i-stop(end task), i-pause, i-play, i-folder, i-globe, i-copy, i-pin, i-shield,
     i-arrow-up, i-arrow-down, i-arrow-right, i-chevron, i-more, i-filter, i-download,
     i-refresh, i-bolt, i-minimize, i-maximize, i-clock, i-user, i-spark
     (24px stroke, currentColor, stroke-width 1.6, round caps/joins)
```

Use icons as `<svg class="icon"><use href="#i-cpu"/></svg>`.

### Module APIs (exact exports)

`js/api.js`
```js
export const isTauri            // boolean
export async function call(cmd, args)  // invoke; in mock mode routes to mock handlers
export function onSnapshot(cb)  // subscribe to orbit://snapshot; mock: setInterval generator
export const win = { minimize(), toggleMaximize(), close(), startDragging() } // no-ops in mock
```
Mock mode: realistic ~120 processes (chrome.exe x8, Code.exe, Discord.exe, explorer.exe,
svchost.exe x30, etc.) with drifting values, 8 cores, 16 GB RAM, a few startup entries
and ~40 services, so every view can be previewed in a browser.

`js/store.js`
```js
export const store = {
  snapshot: null,          // latest Snapshot
  staticInfo: null,        // StaticInfo
  history: {               // ring buffers of last 60 samples (numbers)
    cpu: [], mem: [], diskRead: [], diskWrite: [], diskActive: [], rx: [], tx: [], gpu: [],
    perCore: [[]],         // perCore[i] = number[]
    proc: new Map(),       // pid -> { cpu: number[], mem: number[] }
  },
  settings: { theme: 'night'|'void'|'signal'|'daylight', intervalMs: 1000, paused: false,
              compact: false, reducedMotion: false, grouping: 'grouped'|'tree'|'flat',
              watchlist: { pid: number, start: number }[] /*pid pins bound to startTime*/,
              watchNames: string[] /*exe names*/ },
  query: '',               // global search text (shared by all views)
}
export function on(event, cb)       // events: 'snapshot', 'query', 'settings', 'view', 'select'
export function emit(event, data)
export function setSettings(patch)  // merges, persists to localStorage 'orbit.settings', emits 'settings'
export function setQuery(q)         // emits 'query'
export function pushSnapshot(snap)  // updates snapshot + history, emits 'snapshot'
export function selectProcess(pid)  // emits 'select' (details drawer opens)
```

`js/format.js`
```js
export function bytes(n, digits=1)        // "512 KB", "1.4 GB" (1024 base)
export function rate(bps)                 // "3.2 MB/s", "0 B/s"
export function pct(n, digits=1)          // "12.5%"
export function duration(secs)            // "3d 4h", "2h 05m", "45s"
export function time(epochSecs)           // local "14:03, 8 Nov"
export function heat(value, max)          // 0..1 for heat tint
export function escapeHtml(s)
export function fuzzy(haystack, needle)   // case-insensitive; true if all needle chars appear in order OR substring; returns score number or -1
```

`js/charts.js`
```js
export class LineChart {
  constructor(canvas, { series: [{ color: cssVarName, fill: true }], max: 100 | 'auto', grid: true })
  draw(seriesData /* number[][] */)        // handles devicePixelRatio, reads CSS vars at draw time
}
export function sparkline(canvas, data, { color, max })
```

`js/app.js` (foundation) exports, and other modules import from it:
```js
export function toast(message, kind='info'|'success'|'error')
export function showMenu(x, y, items /* [{label, icon, action, danger?, disabled?, separator?}] */)
export function confirmDialog({ title, body, confirmLabel, danger }) -> Promise<boolean>
export function navigate(view)
export const views = {}   // registry
```
View modules export `export function mount(rootEl)` and optionally
`export function onShow()`, `export function onHide()`. app.js imports each view
module, calls `mount(document.getElementById('view-<name>'))` once at boot, and
`onShow`/`onHide` on navigation. Views subscribe to store events themselves and must
only re-render while visible (check a module-level `visible` flag) except the
processes view keeps its data warm.

`js/palette.js`: `export function initPalette()`, `export function openPalette()`.
Palette items: navigation, theme switches, pause/resume, toggle compact, export CSV,
restart as admin, and every running process (typing a name lists "End <name>",
"Details <name>", "Open location <name>"). Arrow keys + Enter + Esc.

`js/details.js`: `export function initDetails()`; listens to store 'select';
renders into `aside#details`: header (icon, name, pid, status chip), sparkline CPU + memory
from `store.history.proc`, facts from `get_process_details`, children list (click to select),
actions row (End task, End tree, Suspend/Resume, Priority select, Open location, Search online,
Copy details, Pin). Close with Esc or the close button.

Process actions shared helper — put in `js/views/processes.js` and export:
```js
export async function endTask(pid, name)     // confirm for 'windows' kind, calls end_task, toast
export async function endTree(pid, name)
export async function toggleSuspend(row)
export async function setPriority(pid, priority)
export function processMenuItems(row)        // items for showMenu
export function exportCsv()                  // builds CSV of current filtered rows, calls export_report, toast with path
```

### Styling contract

`css/tokens.css` defines on `:root[data-theme="night"]` (and the other 3) at least:
`--bg --bg-raised --panel-solid --panel (gradient) --ink --ink-2 --muted --muted-2
--blue --cyan --blue-soft --edge --line --line-soft --line-strong --accent-grad
--shadow --shadow-soft --glow --danger --warn --ok --heat-rgb (r,g,b for heat tints)
--chart-cpu --chart-mem --chart-disk --chart-net --chart-gpu --chart-grid`
plus fixed: `--font-display --font-body --font-mono --radius-sm --radius --radius-lg
--radius-pill --radius-btn --radius-pass --radius-tile --ease --dur-1 --dur-2`.

Night uses the LegacyTech design-system tokens exactly:
bg #050b18, bg-raised #0a1428, panel-solid #0b1529, navy #0a0f1f, ink #f2f5ff,
ink-2 #c3cee6, muted #8b9cbd, muted-2 #6f84a8, blue #4f8cff, cyan #38d5f0,
blue-soft #8db4ff, edge #d6e6ff, line rgba(141,180,255,.12), line-soft rgba(141,180,255,.07),
line-strong rgba(141,180,255,.26), shadow 0 30px 80px rgba(0,0,0,.45),
shadow-soft 0 24px 60px -28px rgba(0,0,0,.7), glow 0 0 40px rgba(79,140,255,.18),
panel = linear-gradient(180deg, rgba(14,26,52,.78), rgba(8,16,34,.55)),
accent-grad = linear-gradient(90deg, #9dc0ff, #8db4ff, #6fd6f4),
radius-sm 6px, radius 14px, radius-lg 22px, radius-pill 999px,
radius-btn 4px 18px 4px 18px, radius-pass 22px 6px 22px 6px, radius-tile 12px 4px 12px 4px,
ease cubic-bezier(0.2,0.75,0.18,1), dur 0.25s / 0.45s.
Void: bg #000000, panels near-black navy, same accents. Signal: high contrast
(ink #ffffff, muted ≥ 9:1, line-strong borders everywhere, cyan focus). Daylight:
bg #f4f6fb, ink #0a0f1f, blue #2f6feb, cyan #0b8db8, muted ~#4a5878 (≥4.5:1),
panels white glass, mark-light.svg. All text ≥4.5:1 in every theme.

Type: Oxanium for display/interface (micro labels 11px 0.22em uppercase, buttons 12px
0.18em uppercase, nav 14px), Manrope for body/table text (13-14px in dense tables),
tabular-nums for numbers. Display never bold (500). No emoji anywhere. Focus: 2px cyan
outline, 3px offset. Background atmosphere: faint 64px pixel grid + two soft glows, CSS only.
Reduced motion: honour `prefers-reduced-motion` and settings.reducedMotion
(`html.reduce-motion`).

Shared component classes (defined in app.css by foundation, used by views):
`.panel` (glass), `.pass` (alternating-corner card), `.tile`, `.btn`, `.btn--primary`,
`.btn--ghost`, `.btn--danger`, `.btn--sm`, `.icon-btn`, `.chip`, `.chip--on`, `.chip--live`,
`.chip--warn`, `.chip--danger`, `.micro`, `.micro--bright`, `.signal` (5px cyan pixel),
`.view-head` (micro label + title + actions row), `.view-title` (Oxanium 28px 500, -0.03em,
second phrase in `<em>` with accent gradient), `.toolbar`, `.table` (sticky header,
sortable headers are `<th aria-sort><button class="lv-sort" data-sort>` (Processes:
`.proc-sort`), `.is-sorted-asc/desc`, rows `.row`, `.row.is-selected`,
`.row.is-dim`, cell `.cell-num` right-aligned tabular), `.heat` (cell background from
`--h` 0..1 using `rgba(var(--heat-rgb), calc(var(--h)*.35))`), `.switch` (toggle),
`.select`, `.input`, `.empty` (empty state), `.kbd`, `.stat` (label + big value),
`.grid-cores`, `.drawer`.

## Additions

What the code has beyond the sections above. These are part of the contract now.

**v0.4: History, Users, Network, Security, autoruns, tools, tray and mini mode**
- Nine views (Processes, Performance, History, Startup, Users, Services, Network,
  Security, Settings; keys 1-9), new snapshot fields, commands, settings keys, palette
  and mock registries: see `docs/V04-CONTRACT.md`, including its "Integration notes".

**v0.5: Optimize view (health report, startup suggestions, disk cleanup, game boost, driver check)**
- Sidebar: a tenth view, **Optimize** (`#view-optimize`, icon `i-gauge`, new sprite symbol), between
  Security and Settings, numbered 09 on key 9; Settings was 10 on key 0 (`KEY_VIEWS` = `VIEW_NAMES`; changed in v0.6, see My Rig). The palette "Go to" group lists it, and the
  palette Actions offer "Optimize: Health / Cleanup / Boost / Drivers".
- Sub-tabs: Health, Cleanup, Boost, Drivers. `views/optimize.js` is only the frame (head, tab
  list, one pane each; last tab in localStorage `orbit.optimizeTab`) and exports
  `openTab(id)`. Each sub-tab is its own module `views/optimize/<id>.js` with
  `mount(paneEl, ctx)` (once, on first open), optional `onShow()` / `onHide()`;
  `ctx = { openTab(id), setBadge(text, kind?) }` (kind `'warn'` or `'danger'` tints the tab
  count). CSS: `css/views/optimize.css` (frame, `op-`, shared `.op-soon` placeholder card) and
  `css/views/optimize/<id>.css` per sub-tab (prefixes `hl-`, `sg-`, `cl-`, `bo-`, `dr-`),
  all linked in `index.html`. Backend: one module per sub-tab (`health.rs` now; `cleanup.rs`,
  `boost.rs`, `drivers.rs` now), registered in `lib.rs`. To add a sub-tab, see
  `docs/features/health.md` "Adding a sub-tab".
- Commands (`health.rs`):

| command | args | returns |
|---|---|---|
| `get_health_report` | – | `Result<HealthReport, String>` (async; WMI + security scan, about 1-3 s) |
| `get_update_status` | `refresh?: bool` (true: start an online search now) | `UpdateStatus` (plain value) |
| `get_startup_suggestions` | – | `Result<StartupSuggestion[], String>` (async) |
| `open_settings_page` | `page: String` (`ms-settings:` + letters, digits, `-`) | `Result<(), String>` |

```ts
HealthReport { generatedAt: number /*Unix secs*/, isAdmin: boolean, items: HealthItem[],
  drives: DriveSpace[], disks: PhysicalDisk[], diskNote: string|null, update: UpdateStatus,
  uptimeSecs: number, restartReasons: string[] }
HealthItem { key: "disks"|"space"|"restart"|"updates"|"devices"|"uptime"|"startup"|"security",
  title: string, status: "good"|"warning"|"problem"|"unknown", text: string,
  detail: string|null, action: HealthAction|null }
HealthAction { label: string, kind: "view"|"optimize"|"settings"|"admin", target: string }
  // view: navigate(target); optimize: open that sub-tab; settings: open_settings_page(target);
  // admin: Restart as administrator. Orbit never restarts the PC.
DriveSpace { mount: string /*"C:\\"*/, name: string, fileSystem: string, total: number,
  available: number, removable: boolean, status: "good"|"warning"|"problem" }
PhysicalDisk { deviceId: string, name: string, mediaType: "SSD"|"HDD"|"SCM"|"Unknown",
  busType: string, size: number, health: "healthy"|"warning"|"unhealthy"|"unknown",
  problems: string[], drives: string[] /*"C:"*/, wearPct: number|null,
  temperatureC: number|null, powerOnHours: number|null, readErrors: number|null,
  writeErrors: number|null }   // the last five need admin
UpdateStatus { state: "idle"|"checking"|"ready"|"error", lastSearch: number|null,
  lastInstall: number|null, pending: number|null, pendingTitles: string[],
  checkedAt: number|null, online: boolean, error: string|null }
StartupSuggestion { id: string /*StartupEntry.id*/, name: string, path: string|null,
  impact: "high"|"medium", cpuMs: number, diskReadBytes: number, publisher: string|null,
  needsAdmin: boolean, measuredAt: number }
```
- Cargo: `windows` features `Win32_System_UpdateAgent` and `Win32_System_Wmi`.
- `api.js` mock: `ctx.call(cmd, args)` lets a mock module use another mock command's result
  (`mock/health.js` reads `list_startup` and `security_scan`). New mock module `mock/health.js`.
- `views/startup.js` shows the suggestions card (`views/optimize/suggestions.js`,
  `createSuggestions(el, { onChange, compact })`) above the Apps table.
- `tests/ui-smoke.mjs` visits Optimize and each sub-tab in every theme.
- Cleanup (7c, `cleanup.rs`, details in `docs/features/cleanup.md`). The UI sends category ids,
  never paths; Rust computes every root and deletes only strictly inside them (checked by
  handle, no reparse points followed or deleted, in-use / read-only files skipped).

| command | args | returns |
|---|---|---|
| `cleanup_scan` | – | `Result<CleanupScan, String>` (async; read only; progress events) |
| `cleanup_clean` | `ids: string[]`, `olderThanDay?: bool` (default true) | `Result<CleanResult, String>` (async; unknown id is an error) |
| `cleanup_cancel` | `job: "scan"\|"clean"\|"folders"` | `Result<(), String>` |
| `cleanup_drives` | – | `DriveInfo[]` |
| `cleanup_folder_sizes` | `path: String`, `fresh?: bool` | `Result<FolderSizes, String>` (async; read only) |

```ts
// ids: user-temp, windows-temp, update-cache, crash-dumps, system-dumps, thumbnails,
//      browser-edge, browser-chrome, browser-brave, browser-firefox, recycle-bin
CleanupScan { scannedAt: number, isAdmin: boolean, categories: CategoryScan[], cancelled: boolean,
  testRoot: string|null /*ORBIT_CLEANUP_ROOT, debug builds only*/ }
CategoryScan { id, label, description: string, needsAdmin: boolean, ageFilter: boolean,
  paths: string[], found: boolean, readable: boolean, bytes: number, files: number,
  oldBytes: number, oldFiles: number /*what the 24 h rule leaves to clean*/,
  unreadable: number, blocked: string|null /*"Edge is open. ..."*/, note: string|null }
CleanResult { freedBytes, deletedFiles, skippedInUse, skippedDenied, skippedOther, keptNew: number,
  cancelled: boolean, categories: CategoryClean[] }
CategoryClean { id, label: string, freedBytes, deletedFiles, skippedInUse, skippedDenied,
  skippedOther, keptNew: number, skipped: string|null /*whole category skipped, why*/ }
FolderSizes { path: string, bytes, files: number, folders: FolderEntry[] /*top 40, biggest first*/,
  otherBytes, otherCount, filesHereBytes, filesHere, unreadable, links: number,
  cancelled: boolean, scannedAt: number, cached: boolean }
FolderEntry { name, path: string, bytes, files, unreadable: number }
DriveInfo { mount: string, name: string, total: number, available: number, removable: boolean }
// event orbit://cleanup-progress
Progress { job: "scan"|"clean"|"folders", label: string, step: number, steps: number,
  files: number, bytes: number, done: boolean }
```
- `api.js`: `onEvent(name, cb)` subscribes to any backend event and returns an unsubscribe
  function. Mock modules get `ctx.emit(name, payload)` to reach it. New mock module
  `mock/cleanup.js`.
- Boost (7d, `boost.rs`, details in `docs/features/boost.md`). Lowers the listed apps to Below
  normal (or pauses them) and switches to Ultimate / High performance; turning off restores
  each process's priority, resumes what it paused and restores the plan. The "before" state is
  saved to `boost-state.json` before any change and restored on the next start after a crash
  (`boost::init` in setup) and on exit (`boost::on_exit` in `exit_cleanup`). Auto mode runs from
  the sampler (`boost::on_snapshot` after each tick). The tray tooltip adds " · Boost on".

| command | args | returns |
|---|---|---|
| `boost_status` | – | `Result<BoostStatus, String>` (async) |
| `boost_configure` | `config: BoostConfig` | `Result<BoostStatus, String>` (async; saved to `boost-config.json`, used from the next boost) |
| `boost_start` | – | `Result<BoostStatus, String>` (async; no-op when already on) |
| `boost_stop` | – | `Result<BoostStatus, String>` (async; an auto boost stopped by hand stays off until the games exit) |
| `boost_suggestions` | – | `Result<Suggestion[], String>` (async; from the latest snapshot) |
| `boost_pick_exe` | – | `Result<string\|null, String>` (async; Windows file picker for an .exe, null when cancelled) |

```ts
BoostConfig { games: string[] /*exe names*/, apps: string[], auto: boolean, pause: boolean,
  power: boolean /*default true*/ }   // names: file name only, case kept, matched ignoring case
BoostStatus { active: boolean, reason: "manual"|"auto"|null, trigger: string|null /*game*/,
  since: number|null, config: BoostConfig, power: PowerStatus, changed: Changed[],
  skipped: ProcNote[], runningGames: string[], lastRestore: RestoreReport|null,
  recovered: RestoreReport|null /*this start undid a boost a crash left on*/ }
PowerStatus { current: Plan|null, target: Plan|null /*Ultimate, else High performance*/,
  before: Plan|null /*while on, the plan to restore*/, note: string|null }
Plan { guid: string /*lowercase, hyphenated*/, name: string }
Changed { pid: number, name: string, action: "lowered"|"paused", before: string|null /*priority*/ }
ProcNote { pid: number, name: string, reason: string }
RestoreReport { at: number, restored: number, exited: number, leftAlone: number /*user changed
  the priority during Boost*/, failed: ProcNote[], power: "none"|"restored"|"left"|"failed",
  powerNote: string|null }
Suggestion { name: string, exe: string|null, cpu: number, memory: number, count: number,
  hasWindow: boolean }
// event orbit://boost: BoostStatus (on/off, config change, running games change)
```
- `store.js` settings: `boostGames: string[]`, `boostApps: string[]`, `boostAuto` (false),
  `boostPause` (false), `boostPower` (true). The Boost tab sends them with `boost_configure`
  when it mounts and on every change. `ORBIT_BOOST_DIR` overrides the folder (tests).
- `actions.rs`: `guard_pid`, `verify_identity`, `refuse_critical_suspend`, `priority_name`,
  `filetime_to_unix`, `lock_suspended`, `describe`, `describe_ntstatus`, `EXITED` and the
  `NtSuspendProcess` / `NtResumeProcess` imports are now `pub(crate)` for `boost.rs`.
  Processes Boost pauses are added to `SuspendedSet`, so the Processes view shows them.
- New mock module `mock/boost.js` (uses the built-in `set_priority` / `suspend_process`).
- `tests/ui-smoke.mjs` checks that Boost shows its status.
- Drivers (7e, `drivers.rs`, details in `docs/features/drivers.md`). Lists every present device
  (SetupAPI `DIGCF_PRESENT | DIGCF_ALLCLASSES`, device properties, `CM_Get_DevNode_Status`,
  `SetupVerifyInfFile` per INF, cached) and flags problem codes, unsigned drivers and
  third-party drivers 5+ years old (never inbox ones for age). Updates: Windows Update driver
  offers (WUA, `Type='Driver'`, search only unless the user installs one) or the maker's page
  (fixed https table, opened with `open_url`). Health gains a "devices" item (problem codes
  other than 22 / 45, button "Go to Drivers" = `optimize` / `drivers`).

| command | args | returns |
|---|---|---|
| `list_drivers` | – | `Result<DriverList, String>` (async; read only; starts a cached WU driver search when stale) |
| `get_driver_updates` | `refresh?: bool` (true: online search now) | `DriverUpdateStatus` (plain value) |
| `install_driver_update` | `id: string` (an offered UpdateID) | `Result<InstallResult, String>` (async; admin only; WUA download + install of that one update; the UI confirms first) |

```ts
DriverList { scannedAt: number, isAdmin: boolean, oldYears: number /*5*/, devices: Device[]
  /*worst first*/, hidden: number /*no-show devices without a problem, left out*/,
  counts: { total, problem, unsigned, old, off: number }, system: SystemInfo,
  updates: DriverUpdateStatus }
Device { id: string /*instance id*/, name: string, class: string /*"Display adapters"*/,
  manufacturer: string|null, provider: string|null, version: string|null, date: number|null
  /*Unix secs*/, inf: string|null /*"oem12.inf"*/, inbox: boolean, signed: boolean|null,
  signer: string|null, signature: "windows"|"whql"|"authenticode"|"signed"|"unsigned"|null,
  problem: number|null /*CM_PROB_*/, problemText: string|null,
  status: "problem"|"unsigned"|"old"|"off"|"ok", flags: string[] /*worst first*/,
  vendor: VendorLink|null, updateId: string|null /*a DriverOffer.id for this device*/ }
VendorLink { name: string, url: string /*https, hard-coded in drivers.rs*/ }
SystemInfo { manufacturer: string|null, model: string|null, vendor: VendorLink|null /*PC maker*/ }
DriverUpdateStatus { state: "idle"|"checking"|"ready"|"error", offers: DriverOffer[],
  checkedAt: number|null, online: boolean, error: string|null, installing: string|null,
  lastInstall: InstallResult|null }
DriverOffer { id: string /*UpdateID*/, title: string, class, provider, manufacturer, model,
  hardwareId: string|null, date: number|null, optional: boolean /*BrowseOnly*/,
  downloaded: boolean, deviceId: string|null, deviceName: string|null }
InstallResult { id, title: string, ok: boolean,
  result: "succeeded"|"succeededWithErrors"|"failed"|"aborted", restartRequired: boolean,
  message: string, at: number }
```
- Cargo: `windows` features `Win32_Devices_DeviceAndDriverInstallation` and
  `Win32_Devices_Properties`. `health.rs` helpers `now_secs`, `com_init`, `ole_date_to_unix`,
  `lock`, `wua_error` are `pub(crate)`.
- New mock module `mock/drivers.js` (fake install, never touches anything); `mock/health.js`
  builds the "devices" item from it.

**v0.6**
- GPU names (8a, `gpu.rs`, details in `docs/features/gpu.md`). DXGI `EnumAdapters1` (no admin),
  read once and cached; software adapters (Basic Render Driver, Microsoft remote/virtual ones
  with no video memory) are left out; the most video memory first. A virtual display driver can
  list one card twice under a second LUID: `get_gpu_info` drops a same-name, same-memory twin
  that has no GPU counters when the other one has them (`gpu::visible`).

| command | args | returns |
|---|---|---|
| `get_gpu_info` | – | `Result<GpuAdapter[], String>` (async; never fails, `[]` without DXGI) |

```ts
GpuAdapter { luid: string /*"0x00000000_0x0000d1a5": LUID high, low (gpu::luid_key)*/,
  name: string, vendor: string|null /*"NVIDIA"|"AMD"|"Intel"|...*/, dedicatedMemory: number,
  sharedMemory: number /*bytes*/, driverVersion: string|null /*"32.0.15.9621", UMD version
  from CheckInterfaceSupport(IDXGIDevice)*/ }
// Snapshot.gpu gains (SnapshotDelta too, same object):
gpu: { usage, dedicatedUsed, adapters: GpuAdapterLoad[] /*per PDH adapter LUID, sorted by luid;
  [] without counters*/ }
GpuAdapterLoad { luid: string, usage: number|null /*busiest engine of that adapter*/,
  dedicatedUsed: number|null }
```
- Cargo: `windows` feature `Win32_Graphics_Dxgi`.
- `views/performance.js`: the GPU pane title is the first adapter's name; a block
  "Adapters / N" lists one card per adapter (name, live %, bar, "8 GB VRAM · Driver x", memory
  in use), matched to `gpu.adapters` by `luid`. The GPU tile's tooltip names the cards.
  CSS `.perf-adapters`, `.perf-adapter`, `.perf-adapter__name`, `.perf-adapter__val`.
- New mock module `mock/gpu.js` (two adapters, `?nogpu` empties it); `mock/data.js` fills
  `gpu.adapters`. `tests/ui-smoke.mjs` checks the adapter list.
- History row menu (8b, details in `docs/features/history.md`). Right-click or Shift+F10 /
  context-menu key on a row: Search online, Open file location, Properties, Copy name, Copy
  path; Show in Processes and End task only while the app runs (`processes.js` `endTask`:
  same confirm and Orbit / critical guards); Remove from history (danger confirm).

| command | args | returns |
|---|---|---|
| `remove_history_app` | `key: String` (a `HistoryRow.key`) | `Result<u32, String>` days that held it (async; Orbit's own history files and pending totals only; unknown key = 0) |
| `show_properties` | `path: String` (absolute, existing) | `Result<(), String>` (async; Explorer's Properties sheet via `SHObjectProperties` on its own STA thread, which pumps messages while it owns the sheet) |

- Mocks: `show_properties` in `api.js`, `remove_history_app` in `mock/history.js`.
  `tests/ui-smoke.mjs` checks the menu (running and idle app, Shift+F10) and Remove.
- My Rig (8c, `rig.rs`, `views/rig.js`, details in `docs/features/rig.md`). Sidebar: an
  eleventh view, **My Rig** (`#view-rig`, icon `i-rig`, new sprite symbol), between Optimize
  and Settings, numbered 10 on key 0. Settings is 11 with no number key: the sidebar, the
  palette and **Ctrl+,** open it (`KEY_VIEWS` = `VIEW_NAMES` without `settings`). Palette:
  "My Rig" in Go to (hint 10, Settings 11) and the action "Copy specs".

| command | args | returns |
|---|---|---|
| `get_rig_specs` | – | `Result<RigSpecs, String>` (async; read once per run and cached, except `gpus`, which is `gpu::visible` of the cached DXGI list against the latest snapshot) |

```ts
RigSpecs { cpu: CpuSpec, memory: MemorySpec, gpus: GpuAdapter[], board: BoardSpec,
  drives: DriveSpec[], displays: DisplaySpec[], windows: WindowsSpec, readAt: number }
CpuSpec { name: string|null /*raw, "12th Gen Intel(R) Core(TM) i5-12400F"*/, cores: number|null,
  threads: number|null /*summed over sockets*/, baseMhz: number|null /*MaxClockSpeed*/,
  sockets: number }
MemorySpec { total: number|null /*bytes, sum of modules*/, modules: number[] /*bytes each*/,
  speed: number|null /*MT/s, ConfiguredClockSpeed else Speed, slowest module*/,
  kind: string|null /*"DDR4"|"DDR5"|"LPDDR5"|...*/, slotsUsed: number, slotsTotal: number|null }
BoardSpec { maker, model, biosVendor, biosVersion, biosDate /*as the firmware writes it*/,
  systemMaker, systemModel /*null when it repeats the board or is a placeholder*/: string|null }
DriveSpec { name: string, size: number /*bytes*/, media: "SSD"|"HDD"|"SCM"|"Unknown",
  bus: string /*"NVMe"|"SATA"|"USB"|...|""*/ }
DisplaySpec { name: string|null /*EDID*/, width: number, height: number,
  refreshHz: number /*2 decimals*/, primary: boolean, internal: boolean }
WindowsSpec { edition: string|null /*"Windows 11 Pro": ProductName, fixed for builds >= 22000*/,
  version: string|null /*"23H2"*/, build: string|null /*"22631.4460"*/ }
```
- Cargo: `windows` feature `Win32_Devices_Display`. `health.rs`: `Wmi` (+ `connect`, `query`),
  `prop`, `variant_string`, `variant_u64`, `read_physical_disks` are `pub(crate)`;
  `drivers.rs`: `is_placeholder` is `pub(crate)`.
- `views/rig.js` exports `tidyName`, `memSize` (binary), `driveSize` (decimal, as on the box),
  `moduleText`, `specsText(specs)` (the Copy specs text). CSS `css/views/rig.css` (prefix
  `rig-`). New mock module `mock/rig.js` (`?rigfail` throws). `tests/ui-smoke.mjs` visits My Rig
  in every theme and checks the cards, Copy specs, key 0 and Ctrl+,.
- Motion (8d, UI only, details in `docs/features/motion.md`). No new commands, events or
  settings: the existing `settings.reducedMotion` (`html.reduce-motion`) is the one switch,
  shown in Settings > Display as **Animations** (on = `reducedMotion: false`, the default;
  `switchHtml(key, id, invert)` / `data-invert`). Palette: "Turn animations off / on".
  `prefers-reduced-motion: reduce` stops the same things with the switch on.
- Tokens (`tokens.css`): `--ease`, `--dur-1/2/3` (0.25 / 0.45 / 0.9 s) as before, plus
  `--lift` (-3px; 0px under `.reduce-motion` and `prefers-reduced-motion`), `--enter-y` (22px),
  `--stagger` (70ms). Hover lifts use `translateY(var(--lift))`.
- CSS: `.btn` lifts 3px, gets an `::after` light sweep (`overflow: hidden` on `.btn`) and turns
  a trailing `i-arrow-right` icon -45deg; `.panel--hover` (now on My Rig cards), `.perf-tile`,
  `.lv-theme` and `.sidebar-foot` lift and glow; `.dr-link` draws its underline in (scaleX);
  process rows fade their hover tint only; the active sidebar marker slides in
  (`orbit-dot-in`). View entrance: `.enter-up` + `--enter-i` (`orbit-enter`, fill `backwards`
  only, so no transform is left behind). All transform/opacity; the global reduce-motion rules
  in `app.css` drop every transition, animation and the sweep layer.
- `js/motion.js` exports `motionOn()`, `enterView(viewEl)` (top-level `.view-head, .toolbar,
  .panel, .pass, .op-tabs, .perf-tiles, .proc-panel, .empty` blocks, stagger capped at 6),
  `countable(text)`, `countUp(el, ms = 450)` (every number in the text from 0, ease-out; stops
  when the view writes a newer value; skips `[aria-live]`), `countIn(viewEl)` (first show:
  `.stat-value, .perf-readout__value, .perf-tile__value, .hl-summary-chips .chip,
  .cl-foot-total strong` as they first get a number, for 6 s; returns a stop function).
  `app.js` `navigate()` calls `enterView` on every switch and `countIn` once per view.
  Nothing starts while `document.hidden`; nothing runs continuously.
- `tests/ui-smoke.mjs` "motion": Animations on by default; with it on, a button and card have
  0.25 s transitions and a sweep, a view switch animates `orbit-enter 0.45s`, `countUp` starts
  from 0, and every block ends at opacity 1; with the switch off and under
  `prefers-reduced-motion`, transitions are 0s, `--lift` is 0px, no sweep, no entrance, no
  count-up.
- Malware scan through Microsoft Defender (8e, `defender.rs`, `views/defender.js`, details in
  `docs/features/defender.md`). No new sidebar view: a "Malware scan" panel
  (`section.panel.df[data-defender]`) sits at the top of Security, above the process-scan
  toolbar. Orbit has no engine of its own and never removes or quarantines anything.

| command | args | returns |
|---|---|---|
| `defender_status` | – | `Result<DefenderStatus, String>` (async; read only: WMI `root\Microsoft\Windows\Defender` `MSFT_MpComputerStatus` + `root\SecurityCenter2` `AntiVirusProduct`) |
| `defender_scan` | `kind: "quick"\|"full"\|"custom"`, `path?: String` (custom: an existing file or folder, plain `X:\` path on a local drive) | `Result<DefenderScan, String>` (async; resolves when the scan ends; Err when one is already running, the path is refused or MpCmdRun.exe is missing) |
| `defender_cancel` | – | `Result<(), String>` (kills the running MpCmdRun; no-op when none) |
| `defender_pick_target` | `folder: bool` | `Result<string\|null, String>` (async; Windows file or folder picker, null when cancelled) |
| `open_windows_security` | `page?: "threats"\|"home"` | `Result<(), String>` (`windowsdefender://threat` / `threatsettings`, falling back to `windowsdefender://` and `ms-settings:windowsdefender`) |

```ts
DefenderStatus { readAt: number, isAdmin: boolean, available: boolean /*MSFT_MpComputerStatus read*/,
  active: boolean /*service + antivirus on, not passive*/, runningMode: string|null /*"Normal"|
  "Passive Mode"|...*/, realTime: boolean|null, signatureVersion: string|null,
  signatureUpdated: number|null, signatureAgeDays: number|null, signaturesOutOfDate: boolean|null,
  quickScanAt: number|null, fullScanAt: number|null /*null = never*/, engineVersion: string|null,
  productVersion: string|null, tamperProtected: boolean|null, products: AvProduct[],
  other: AvProduct|null /*another antivirus that is on while Defender is not active*/,
  canScan: boolean /*active and MpCmdRun.exe found*/, scan: DefenderScan|null /*running, else
  the last one this run*/, error: string|null }
AvProduct { name: string, defender: boolean, state: "on"|"off"|"snoozed"|"expired",
  upToDate: boolean }   // productState bits 12-15 and 4-7
DefenderScan { id: number, kind: "quick"|"full"|"custom", target: string|null,
  state: "running"|"clean"|"threats"|"failed"|"cancelled", startedAt: number, elapsedSecs: number,
  threats: string[] /*"Trojan:Win32/Wacatac.B!ml"*/, message: string|null,
  needsAdmin: boolean /*show Restart as administrator*/, exitCode: number|null }
// event orbit://defender-progress: DefenderScan (state "running" about once a second, then the result)
```
- MpCmdRun.exe: `HKLM\SOFTWARE\Microsoft\Windows Defender` `InstallLocation`, else the newest
  `%ProgramData%\Microsoft\Windows Defender\Platform\*\`, else `%ProgramFiles%\Windows Defender\`.
  Run as `-Scan -ScanType 1|2|3 [-File <path>]` (path one argument, `CREATE_NO_WINDOW`). Exit 0 =
  clean, 2 = threats (names from its "Threat : <name>" lines); otherwise failed, with
  `hr = 0x80070005` = needs admin. `exit_cleanup` calls `defender::on_exit` (kills a running
  MpCmdRun). `rig.rs`: `reg_sz` is `pub(crate)`.
- `views/defender.js` exports `mountDefender(el)`, `showDefender()`, `loadStatus(force?)`,
  `defenderStatus()`, `currentScan()`, `onDefender(fn)`, `startScan(kind, path?)`, `cancelScan()`,
  `openWindowsSecurity(page?)`, `scanWithDefender(path, name?)` (toasts the result),
  `defenderMenuItem(path)` (the menu item; disabled with hint "Defender is off" when it can't
  scan), `baseName`, `elapsedText`, `whenText`. Processes (row and group menus) and History add
  **Scan with Defender** after Search online / Properties. Palette: "Quick scan with Defender",
  "Full scan with Defender", "Scan a file with Defender" (only when Defender can scan). A full
  scan asks first (`confirmDialog`).
- CSS in `css/views/security.css` (prefix `df-`). New mock module `mock/defender.js` (quick 3 s,
  full 6 s, custom 1.5 s; a file named like a crack / keygen has two threats; the file picker
  returns `...\Downloads\game-keygen.exe`; `?otherav` = Norton 360 active, Defender passive).
  `tests/ui-smoke.mjs` "defender": status facts, quick scan running + elapsed + clean, full scan
  cancelled, picked file with threats, Processes menu scan, History menu item, `?otherav`.

- Guided debloat (8f, `src-tauri/src/debloat/`, `views/optimize/debloat.js`, details and the
  full reviewed list in `docs/features/debloat.md`). A fifth Optimize sub-tab, **Debloat**
  (`#op-pane-debloat`, palette "Optimize: Debloat"). Verdicts come only from `debloat/known.rs`
  (82 entries); `debloat/guard.rs` refuses its never-offer list at analysis and again before
  every change. Real Windows calls sit behind `debloat::Backend` (`system::Real`); tests use a
  fake.

| command | args | returns |
|---|---|---|
| `debloat_analyze` | – | `Result<DebloatAnalysis, String>` (async; read only: startup entries + impact, every scheduled task, services, this user's main app packages) |
| `debloat_preview` | `ids: String[]` (`DebloatItem.id`, 1-500) | `Result<DebloatPreview, String>` (async; changes nothing, writes nothing) |
| `debloat_apply` | `ids: String[]` | `Result<DebloatApplyResult, String>` (async; only Safe, on, unguarded items; each change logged as pending with its previous state before the Windows call) |
| `debloat_changes` | – | `Result<DebloatChange[], String>` (async; newest first; a removed app installed again is marked undone) |
| `debloat_undo` | `id: number` (`DebloatChange.id`) | `Result<DebloatChange, String>` (async; startup/task on again, service start type restored; app: opens its Store page, state stays "applied") |
| `debloat_undo_all` | – | `Result<DebloatUndoAllResult, String>` (async; newest first, skips removed apps) |

```ts
DebloatAnalysis { readAt: number, isAdmin: boolean, counts: DebloatCounts,
  groups: DebloatGroup[] /*startup, task, service, app*/, errors: string[] /*"Services: ..."*/ }
DebloatCounts { off: number, keep: number, unknown: number, ready: number /*safe, on, unguarded*/ }
DebloatGroup { id: "startup"|"task"|"service"|"app", title: string /*"Startup apps"...*/,
  counts: DebloatCounts, items: DebloatItem[] /*safe+on first, safe+off, keep, unknown; by name*/ }
DebloatItem { id: string /*"startup:<StartupEntry.id>"|"task:<path>"|"service:<name>"|
  "app:<package family>"*/, group, name: string /*plain*/, raw: string /*Windows' name*/,
  what: string|null, verdict: "off"|"keep"|"unknown", note: string|null /*what you lose / why
  keep / the guard's reason*/, on: boolean, actionable: boolean, needsAdmin: boolean,
  change: string|null /*"Turn off at sign-in"|"Turn off the task"|"Set to Manual"|"Remove for
  this user"*/, detail: string|null /*command, task program, "Starts: Automatic", app name*/,
  impact: "high"|"medium"|"low"|null, suggested: boolean /*unknown startup app picked by
  health::pick_suggestions*/, search: string, family: string|null, guarded: boolean }
DebloatPreview { changes: DebloatPlanned[], refused: DebloatRefusal[], isAdmin: boolean }
DebloatPlanned { id, group, name, raw, action: string, from: string /*"On at sign-in"|"On"|
  "Automatic"|"Automatic (delayed)"|"Installed"*/, to: string /*"Off"|"Manual"|"Removed for
  you"*/, needsAdmin: boolean }
DebloatRefusal { id, name, reason: string, needsAdmin: boolean /*refused only for rights*/ }
DebloatChange { id: number, item: string /*DebloatItem.id*/, group, name, raw, target: string
  /*startup id | task path | service name | package full name*/, action, from, to,
  before: string /*"enabled"|"auto"|"autoDelayed"|"installed"*/, family: string|null,
  storeUrl: string|null /*ms-windows-store://pdp/?PFN=<family>*/, needsAdmin: boolean,
  at: number, state: "pending"|"applied"|"undone", undoneAt: number|null }
DebloatApplyResult { applied: DebloatChange[], failed: { id, name, error }[],
  refused: DebloatRefusal[], changes: DebloatChange[] /*all, newest first*/ }
DebloatUndoAllResult { undone: DebloatChange[], failed: { id, name, error }[],
  storeApps: DebloatChange[] /*removed apps still waiting for the Store*/, changes: DebloatChange[] }
```
- Change log: `<app_local_data_dir>\debloat\debloat-changes.json`
  `{ version: 1, nextId, changes: DebloatChange[] }` (atomic writes). Debug builds honour
  `ORBIT_DEBLOAT_DIR`.
- Changes: startup via `startup::set_startup_enabled` (StartupApproved); tasks via Task
  Scheduler COM `GetTask(path)` + `put_Enabled`; services via `services::set_start_type`
  (`ChangeServiceConfigW` Manual / Automatic + `SERVICE_CONFIG_DELAYED_AUTO_START_INFO`; never
  Disabled, never stopped); apps via WinRT `PackageManager.RemovePackageAsync(fullName)`
  (current user only). Errors: "Access denied. Try Restart as administrator.", "This task no
  longer exists. Analyze again.", "Needs administrator rights. Restart Orbit as administrator,
  then undo.", "That change was already undone."
- Cargo: `windows` features `ApplicationModel`, `Foundation`, `Foundation_Collections`,
  `Management_Deployment`. `autoruns.rs`: `all_tasks()` / `TaskBasic`, `auto_services()` /
  `RawService` are `pub(crate)` (the task walk is a shared visitor). `services.rs`:
  `collect_services`, `forget_config` `pub(crate)`, new `set_start_type`. `actions.rs`:
  `shell_open` and `OwnedHandle` used from `debloat`.
- Health: the "startup" item's action is `{ label: "Review in Debloat", kind: "optimize",
  target: "debloat" }` when it is a warning (high-impact apps Orbit would suggest); otherwise
  Go to Startup as before.
- UI: `views/optimize/debloat.js` (sub-tab API, CSS `css/views/optimize/debloat.css`, prefix
  `db-`). Fresh results use motion.js `enterView` and `countUp` on the summary title. The tab
  badge is `counts.ready`. New mock module `mock/debloat.js` (a fixed PC with every verdict in
  every group; a removed app's Undo counts as reinstalled on the next list). `mock/health.js`
  mirrors the Health link. `tests/ui-smoke.mjs` "debloat".

**v0.7**
- Sidebar logo (9a, UI only, details in `docs/features/logo.md`). No new commands, events or
  settings; it follows `settings.reducedMotion`, `prefers-reduced-motion` and `document.hidden`
  through `motion.js` `motionOn()`.
- DOM: in `#sidebar` between `.nav-list` and `#mini-readout`, `.sidebar-logo-slot` (flex 1 1 0,
  `container-type: size`, margin-top -12px) holding `button#sidebar-logo.sidebar-logo`
  (`aria-label="LegacyTech"`, 76px, margin-top 12px; starts with `.is-assembling`): an `svg.lt-ring` (gradient
  `#lt-ring-fade`) and `span.lt-mark` with seven `.lt-px` (sixth `.lt-px--blue`, seventh
  `.lt-px--cyan` holding `.lt-sig`). `@container (max-height: 100px)` hides the button. Click:
  `navigate('settings')` and scroll `.lv-about` into view.
- Tokens (`tokens.css`, every theme): `--logo-ink`, `--logo-blue`, `--logo-cyan` (logo kit
  color-dark in Night, Void, Signal; color-light in Daylight).
- CSS animations: `lt-assemble` (`.is-assembling .lt-px`, stagger `--lt-i` x 110ms),
  `lt-ring-in`, `lt-pulse` (`.is-pulsing .lt-sig`, 3.2 s, 1.1 s delay, infinite).
- `js/logo.js` exports `initLogo()` (called in `boot()` after the settings listener; toggles
  `.is-pulsing`, drops `.is-assembling` after 1.5 s), `logoTick(snap)` (called from
  `handleSnapshot` while the window shows; sets the `lt-pulse` animation's playbackRate) and
  `pulseRate(cpu)` (1 at 0% to 3 at 100%, 0.25 steps).
- `tests/ui-smoke.mjs` "<theme>: logo" and "logo".
- CPU and GPU temperatures (9b, `sensors.rs`, details in `docs/features/sensors.md`). No new
  command or event: `Snapshot.sensors` (and `SnapshotDelta.sensors`, same object) carries the
  latest reading, taken on the `orbit-sensors` thread at most once a second while the sampler
  ticks (`sensors::SensorHub::poll`, never waits; a reading older than 5 s is dropped).

```ts
Snapshot.sensors: Sensors { cpu: CpuSensors|null /*no source*/, gpus: GpuSensors[] }
CpuSensors { source: "LibreHardwareMonitor"|"OpenHardwareMonitor"|"HWiNFO"|"ACPI",
  approximate: boolean /*true for ACPI: motherboard zone*/, package: number|null /*°C*/,
  cores: Reading[] /*°C, "Core 1"|"P-core 0"|"E-core 4"|"CCD 1"*/, power: number|null /*W*/,
  clocks: Reading[] /*MHz per core*/ }
Reading { name: string, value: number }
GpuSensors { luid: string|null /*DXGI adapter, gpu::luid_key*/, name: string, source: "NVML",
  temp, memoryTemp, slowdownTemp /*°C*/, fanPct /*0-100*/, powerW, powerLimitW,
  coreMhz, memoryMhz: number|null }
```
- Sources: NVML loaded at run time (System32, else NVSMI); CPU from LibreHardwareMonitor /
  OpenHardwareMonitor WMI `Sensor`, HWiNFO shared memory `Global\HWiNFO_SENS_SM2`, else (elevated
  only, once its value moved) ACPI `MSAcpi_ThermalZoneTemperature`. No kernel driver. A value a
  source does not give is null; temperatures outside 0-150 °C count as missing.
- Cargo: `windows` feature `Wdk_Graphics_Direct3D` (D3DKMT adapter address, to match NVML's PCI
  bus to a DXGI LUID). `health.rs`: `Wmi::query_timeout(wql, row_ms)` (`query` = 10 s per row).
- Settings: `settings.tempUnit` `'c'|'f'` (default `'c'`), Settings > Display "Temperature"
  (`select#set-temp[data-temp-unit]`). Readings stay °C; only the text changes.
- `js/temps.js` exports `TEMP_LIMITS` (`cpu` warm 80 / hot 90, `gpu` warm 80 / hot 87, °C),
  `SOURCE_NAMES`, `tempLevel(kind, c)` -> `'normal'|'warm'|'hot'|null`, `tempText(c, unit?, dp?)`
  ("62°C", "144°F", "–"), `setLevel(el, level)` (class `temp--normal|warm|hot`).
- `motion.js` exports `enterBlock(el)`: the `.enter-up` fade and rise for a block that appears
  after its view showed (the Sensors body when a reading arrives); nothing when `motionOn()` is false.
- `views/performance.js`: CPU and GPU tiles hold `.perf-tile__line` > `.perf-tile__value` +
  `.perf-tile__temp[data-ref="t"]` (hidden without a reading). CPU and GPU panes: block
  `.perf-sensors` (refs `sens`, `sensHead`, `sensSource`, `sensBody`, `sensTemp`, `sensLabel`,
  `tempChart`, `tempChartTop`, `sensPeak`, `sensFacts`, `sensCores` (CPU), `sensEmpty`), placed
  before the facts / top consumers. CPU cells `.perf-sensor-core` (`__name`, `__temp`, `__clock`).
  Adapter lines gain `.perf-adapter__temp` and `.perf-adapter__note` ("Temperature not available
  on this GPU"). Links `a.perf-link[data-url]` open through `open_url`. The "not available"
  texts wait for 3 snapshots. Temperature history (60 readings, real ones only) lives in the view.
- CSS (performance.css): `.temp--normal` (`--ok`), `.temp--warm` (`--warn`), `.temp--hot`
  (`--danger`), `.perf-tile__line`, `.perf-tile__temp`, `.perf-sensors__body|now|temp|label|
  facts|cores|empty|source`, `.perf-sensor-core*`, `.perf-adapter__temp`, `.perf-adapter__note`,
  `.perf-link`.
- New mock module `mock/sensors.js` (`sensorsFor(snap)`, called by `mock/data.js`): CPU from
  LibreHardwareMonitor with 6 cores, NVIDIA with every sensor, the Intel card without one;
  `?nocputemp` (cpu null), `?hot` (past the hot thresholds), `?nogpu` (gpus []).
  `tests/ui-smoke.mjs` "<theme>: temperatures" and "temperatures".

**v1.1: Restart, Run new task, affinity, always on top, startup impact**
- Types: `AffinityInfo { processMask: string, systemMask: string, logicalCores: number }`
  (masks are decimal u64 strings, bit n = CPU n; JS uses BigInt). `StartupEntry` gains
  `impact: "high"|"medium"|"low"|null` and `impactMeasuredAt: number|null` (Unix seconds).
- `restart_process`: same identity check and guards as `end_task` (critical processes,
  Orbit itself and its WebView2 helpers). Reads the exe path, the raw command line
  (`NtQueryInformationProcess` class 60, falls back to sysinfo's) and the working folder
  (sysinfo, as the drawer does), ends the process, waits up to 5 s for it to exit, then
  `CreateProcessW` with the original command line and folder (exe path alone if the command
  line is unknown; the exe's folder if the old folder is gone; `CREATE_NEW_CONSOLE`).
  Errors: "Orbit can't read this process's path. Try Restart as administrator.",
  "Store apps can't be restarted this way. Open it again from Start." (WindowsApps or
  SystemApps), "Windows needs this process. Orbit can't restart it." (critical or dwm-like),
  "Services can't be restarted this way. Use the Services view." (session 0),
  "Ended X, but could not start it again: ...". The new process inherits Orbit's
  environment and elevation.
- Explorer (`%SystemRoot%\explorer.exe`): `restart_process` ends it, then waits up to 3 s for
  a new explorer.exe in the same session (or a taskbar owned by another process); if none
  appears it starts `%SystemRoot%\explorer.exe` itself. `end_task` on it runs the same
  fallback on a background thread (one at a time), so the taskbar always comes back.
- `run_task`: parses like the Run box (quoted path + args; URL or `scheme:`; unquoted path
  with spaces = longest existing prefix; else first word + rest), expands `%VARS%`,
  `www.` gets `https://`, then `ShellExecuteExW` ("runas" or "open", SW_SHOWNORMAL, user
  profile as working folder) on an STA thread. Errors: "Elevation was cancelled.",
  "Windows can't find '<x>'. Check the name and try again."
- `set_affinity` refuses 0 ("Pick at least one processor."), bits the system lacks, Orbit's
  own processes and critical ones ("Windows needs this process. Orbit won't change it.").
- Orbit's own subtree (refused for actions) is now only its `msedgewebview2.exe` children
  and their descendants: programs Orbit starts (Run new task, Restart) stay manageable.
- Startup impact: `orbit.exe --measure-startup` (checked in `lib::run` before the Tauri
  builder, so no window) samples every 2 s for 120 s (`ORBIT_MEASURE_SECS` overrides, for
  tests), tracks processes whose image is an enabled startup entry's resolved path and that
  are at most 90 s old when first seen, keeps the sample closest to 60 s after each one
  started (CPU = kernel + user time, disk = `IO_COUNTERS.ReadTransferCount`, so all reads),
  sums per exe and writes `%LOCALAPPDATA%\Orbit\startup-impact.json`
  `{ measuredAt, entries: { "<lowercased exe path>": { cpuMs, diskReadBytes, impact } } }`.
  High = > 1 s CPU or > 3 MB read; Medium = 300 ms-1 s or 300 KB-3 MB; Low = below both
  (1 MB = 1024 KB). `list_startup` recomputes the label from the numbers. The RunOnce value
  is `HKCU\...\CurrentVersion\RunOnce\OrbitStartupImpact` = `"<orbit.exe>" --measure-startup`.
- Capability: `core:window:allow-set-always-on-top`.
- `api.js`: `win.setAlwaysOnTop(on)` (no-op in the mock). Mock answers every new command,
  with sample impacts on the startup entries.
- `store.js`: `settings.alwaysOnTop` (boolean, default false), applied at boot.
- `app.js`: `formDialog({ label, title, body, okLabel, render(fieldsEl, ctl), onSubmit(ctl) })`
  -> Promise<boolean> (the confirm dialog's layer and style; `ctl.setOk(bool)`,
  `ctl.error(msg)`; `onSubmit` returns true to close or an error string to show inline; Tab
  cycles all fields, Enter submits, Esc cancels). `openRunTask()` (Ctrl+N, the Processes
  toolbar "Run task" button, the palette): input with the last 10 commands as a datalist
  (localStorage `orbit.runHistory`), "Run as administrator" checkbox (checked and disabled
  when Orbit is elevated). Ctrl+N's default (new window) is always prevented. Titlebar
  `#win-pin` (aria-pressed) toggles always on top.
- `views/processes.js`: `restartProcess(pid, name, startTime?)` -> new pid or null (no confirm
  for Explorer; toast "Restarted <name>."), `setAffinity(row)` (dialog: "All processors" +
  one checkbox per logical CPU, OK disabled when none), `whenListed(pid, ms?)`. Menus:
  Explorer rows and groups show "Restart" first; other rows show it after End process tree;
  every row has "Set affinity". Drawer actions add Restart (follows the new pid) and Set
  affinity. Palette: "Restart <name>", "Run new task", "Always on top".
- `views/startup.js`: Impact column (sortable, High/Medium/Low chips =
  `chip--danger`/`chip--warn`/`chip--ok`, otherwise muted "Not measured"), a micro label
  "Impact / <time>", "Measure at next sign-in" button and a note with Cancel while scheduled.
- CSS: `.dialog--form`, `.dialog__fields`, `.dialog__field`, `.dialog__input`,
  `.dialog__error`, `.dialog__grid`, `.check` (+ `.check--strong`), `.titlebar-pin` (app.css);
  `.c-impact`, `.lv-impact-none`, `.lv-impact-when`, `.lv-note`, `.lv-note-text` (lists.css).

**Late additions (finish pass)**
- `views/processes.js`: `endTask(pid, name, startTime?)`, `endTree(pid, name, startTime?)`,
  `setPriority(pid, priority, startTime?)`; without a start time they use the current row's.
- `store.js`: `applyDelta(delta)` returns the merged Snapshot, or null when a resync
  (`get_snapshot`) is needed; `isDelta(payload)`. The UI always calls `set_sampling` with
  `delta: true`.
- Backend refuses `end_task`, `end_tree` and `suspend_process` on critical Windows processes
  (csrss, wininit, smss, lsass, services, winlogon under System32; System, Registry,
  Secure System, Memory Compression) with "Windows needs this process. Ending it would
  crash the PC."
- `get_icons` caches only found icons; the UI retries missing ones after 4 s and 8 s.

**Behaviour**
- Per-process `diskReadBps`/`diskWriteBps` are shown as "I/O" (column, drawer "I/O total",
  Performance "Top I/O", CSV "I/O read/write KB/s"). The system-wide Disk tile stays "Disk".
- `export_report` adds the one UTF-8 BOM for `.csv`; the UI sends contents without one.
- Orbit's own row (`pid === StaticInfo.pid`): End task, End tree, Suspend and Priority are
  disabled and the UI never calls the backend for it (the backend refuses anyway).
- 'End all' is refused for csrss.exe, wininit.exe, smss.exe, lsass.exe, services.exe and
  winlogon.exe; other Windows groups get the "part of Windows" warning.
- `confirmDialog({ danger: true })` focuses Cancel; other confirms focus the OK button.
- Space pauses unless focus is on a control reached by keyboard (`:focus-visible`) or a switch.
- Only the titlebar `.signal` beats once per snapshot (`.is-beat`, 600 ms); every other
  `.signal` is static. Per-tick meters have no CSS transitions.
- Jumping to a process (service PID, palette "Details") clears the global search first.
- Mock: `?nogpu` in the URL makes `gpu.usage` and every row's `gpu` null together.

**Exports**
- `api.js`: `win.isMaximized()`.
- `app.js`: `currentViewName()`, `restartAsAdmin()`; `toast` kinds also include `'warn'`;
  `showMenu` items also take `hint`.
- `store.js`: `HISTORY_LEN` (60), `pinStartMatches(start, row)`, `isPidPinned(row, list)`,
  `pinEntry(row)` -> `{ pid, start }`. `store.selected` = pid shown in the drawer.
- `format.js`: `IO_TITLE` (I/O tooltip text), `historySpan(intervalMs, long?)` ("60 s", "5 min").
- `palette.js`: `openPalette(initial?)`, `togglePalette()`, `isPaletteOpen()`.
- `views/processes.js`: `PRIORITIES`, `statusLabel(row)`, `isPinned(row)`, `togglePin(row)`,
  `copyDetails(row)`, `copyText(text)`.

**CSS variables** (all four themes unless noted)
- Colour: `--cyan-ink` (cyan for text and focus rings; darker than `--cyan` only in Daylight),
  `--navy`, `--primary-bg`, `--on-primary`, `--overlay`, `--grid-line`, `--atmo-a`,
  `--atmo-b`, `--vignette`.
- Fixed: `--radius-tile-sm`, `--dur-3`, `--titlebar-h`, `--sidebar-w`, `--drawer-w`,
  `--row-h` (26px with `html.compact`), `--z-drawer`, `--z-titlebar`, `--z-menu`,
  `--z-palette`, `--z-dialog`, `--z-toast`.
- Set inline by views: `--h` (heat 0..1), `--d` (tree depth), `--proc-row-h`,
  `--proc-cols`, `--proc-edge`, `--tile-accent`.
- Focus rings use `--cyan-ink` (same as `--cyan` in Night, Void and Signal).
