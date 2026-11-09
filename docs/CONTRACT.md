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
| `restart_as_admin` | – | `Result<(), String>` (ShellExecuteW "runas" self, then exit 0) | actions |
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
