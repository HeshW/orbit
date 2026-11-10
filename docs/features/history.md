# Feature: history (v0.4)

A History view (03) like Task Manager's App history, recorded by Orbit itself while it
runs: no admin, no SRUM. Per app: CPU time, active time, average and peak memory, I/O
read and write, network (with admin), first and last seen. Commands and shapes as in
`docs/V04-CONTRACT.md`; nothing renamed.

Files: `src-tauri/src/history.rs`, `ui/js/views/history.js`, `ui/css/views/history.css`,
`ui/js/mock/history.js`.

## Backend (`history.rs`)

| item | how |
|---|---|
| `record(snapshot, interval)` | Sampler thread, every tick. Skipped while recording is off (`AtomicBool`). Rows are grouped by app key (lowercased exe path, else lowercased name; the System Idle Process is skipped). A process is looked up by (pid, start time) in a cache, so only a new process builds its key string; per-app tick sums live in a reused vector. Exited processes leave the cache every 64 ticks. |
| units | **CPU time = seconds of one logical core**: a row's `cpu` is percent of the whole machine, so a tick adds `cpu / 100 * logical cores * interval` (one core busy for an hour = 3600 s on any PC). Memory = sum of the app's working sets; max = largest tick, average = time-weighted over the seconds the app ran. I/O and network bytes = rate x interval (network only while per-app rates exist, so 0 unelevated). **Active** = the app's CPU above 0.5% of the machine, or it owns a window (`kind == app`). |
| storage | One JSON file per local day, `<dir>/YYYY-MM-DD.json` (`{ version, date, apps: [...] }`, each app with `memByteSeconds` and `runSeconds` so averages merge across days). `dir` = `ORBIT_HISTORY_DIR` if set, else `app_local_data_dir()/history` = `%LOCALAPPDATA%\me.legacytech.orbit\history` (set in `init`, `OnceLock`). Memory holds only what was recorded since the last write; a write reads the day file, adds to it and replaces it (temp file + rename). |
| when it writes | At most once a minute (on the sampler tick that is due), at local midnight (the pending totals go to the old day, then a fresh day starts), on exit (`flush`) and when recording is turned off. Local day = UTC + the offset from `GetLocalTime`/`GetSystemTime` (follows DST; no date crate). |
| pruning | Day files older than 90 days (today counts as one) are deleted at start (background thread) and at each new day. Only `YYYY-MM-DD.json` names are touched. |
| `get_history(days)` | `spawn_blocking`. Days clamped 1..90; merges the files of the last N local days plus what is still in memory, sorted by CPU time. A broken file is skipped with a log line. |
| `clear_history` | Deletes every day file (and half-written `.tmp`) in the folder and empties memory. Recording carries on if it is on. |
| `remove_history_app(key)` | v0.6. `spawn_blocking`, under `IO`. Drops that app's pending totals (`Recorder::forget`; a running app is counted again from the next tick) and removes it from every day file (`remove_from_files`: rewrite atomically, delete a day left empty, other files untouched). Returns the number of days that held it; an unknown key is not an error. Orbit's own files only. |
| `set_history_enabled` | Now `async` (allowed by the contract). Stores the flag; turning it off writes what was pending. |
| locking | One `IO` mutex around file reads/writes, always taken before the `STATE` mutex; ticks that do not write take only `STATE`. |

Cost (debug build, `cargo test --lib record_tick_is_cheap -- --nocapture`): about
170-190 us per tick for 600 processes in 150 apps, with no allocation once the processes
are known; `take` 70-130 us; the once-a-minute file merge 16 ms for 150 apps (debug).
Live day file: 180 apps, 78 KB. `get_history(1)` round trip from the UI: 10 ms.

## UI (`views/history.js`)

- Head stats for the range (after search): Apps, CPU time, I/O, Since (earliest first seen).
- Toolbar: range chips Today / 7 days / 30 days / Custom ("Last N days of M", 1..
  `settings.historyDays`; presets longer than `historyDays` hide), "N of M apps",
  "Record app history" switch bound to `settings.historyEnabled`, "Delete history"
  (`confirmDialog({ danger: true })`), refresh.
- Note with the lock icon: "History stays on this PC." plus what Orbit counts (time Orbit
  was closed is missing), or that recording is off.
- Table (sortable, default CPU time descending): Name (icon, full path tooltip), CPU time
  (bar relative to the top app; "1h 02m", full "1h 02m 05s" in the tooltip), Active, Avg
  mem, Max mem, I/O read, I/O write (labelled I/O as the contract's Behaviour section
  requires: these are IO_COUNTERS, not physical disk), Network (down + up; muted "Needs
  admin" when an app has no bytes and per-app network is unavailable), Last seen ("Just
  now", "12 min ago", "Today 14:03", "Yesterday 09:10", "8 Nov"). Below 1300 px Max mem
  and I/O write hide; below 1080 px Last seen hides.
- Global search filters by name and path. Reloads on show, range change, every 30 s while
  visible and not hidden. Empty states: reading, error with Try again, nothing recorded,
  recording off (with "Turn on recording"), no search match.
- `set_history_enabled` is sent at mount and whenever `historyEnabled` changes.
- Range is remembered in settings as `historyRange` ("today"|"7"|"30"|"custom") and
  `historyCustomDays` (store.js keeps extra keys).
- Palette: "App history", "Turn off/on app history recording", "Delete app history" (danger).
- Row menu (v0.6, 8b): right-click a row, or Shift+F10 / the context-menu key on the focused
  row (rows are focusable: one roving `tabindex="0"`, arrows / Home / End move, the
  selection survives the 30 s refresh). Items: Search online, Open file location,
  Properties (`show_properties`, Explorer's Properties sheet), Copy name, Copy path; while
  the app runs (a snapshot process whose lowercased exe path, else name, equals the key):
  Show in Processes (clears the search, opens Processes and the details drawer) and End
  task (`processes.js` `endTask` on the process with a window, else the root of the app's
  tree: same confirm for Windows processes and the same refusal for Orbit itself); then
  Remove from history (danger confirm, `remove_history_app`).

## Mock (`mock/history.js`)

25 apps over 30 days from a fixed seed (stable between reloads): daily, weekday, weekend
and occasional apps, Premiere first seen 21 days ago, Orbit 6 days ago; today is partial.
Network bytes only on days 9-11 ago (as if Orbit ran elevated then), plus today after the
mock's Run as admin. Delete empties it; `remove_history_app` hides one key until reload.
Flags: `?nohistory`, `?historyfail`.

## How to test

- `cargo test --lib history -- --nocapture`: date maths and file names, per-app totals
  (CPU units, time-weighted average, active rule, case-insensitive keys, idle process
  skipped), network and NaN handling, process cache pruning, file merge across writes and
  days, broken file skipped, pruning at 90 days, delete leaves other files, the global
  record/flush/read/disable/clear path (with `ORBIT_HISTORY_DIR` on a scratch folder) and
  the tick cost.
- Live: start the debug exe with `ORBIT_HISTORY_DIR` set to a scratch folder and CDP on,
  `await __TAURI__.core.invoke('get_history', { days: 1 })`, open History, wait a minute
  for the day file.

Checked: `cargo build` with no warnings, `cargo test` (82 passed, 3 ignored), `node --check`
on every `ui/js` file. Mock (served over HTTP): every range, custom days clamped to
`historyDays`, sort, search, switch off/on, Delete history (danger confirm focuses
Cancel), palette, both flags, 1320x840 and 960x620 with no clipped headers or cells, all
four themes, contrast >= 4.5:1 for every text style in the view (lowest 4.80, Night micro
labels), the other eight views open, no console errors. Live (debug exe, unelevated, own
scratch history folder): 179 apps after a few seconds, view and range switch, the day file
written after one minute (version 1, 180 apps, 78 KB), totals frozen while the switch is
off and growing again after, `get_history(5000)` clamped, closing the window rewrote the
file (exit flush), a relaunch read the day back (180 apps), `clear_history` deleted the
file and recording continued, all four themes, no console errors.

## Not verified

- The default folder `%LOCALAPPDATA%\me.legacytech.orbit\history`: every live run used
  `ORBIT_HISTORY_DIR`, so the `app_local_data_dir` path was not exercised live.
- Midnight rollover and the 90-day prune at a new day: unit tested with explicit days, not
  run across a real midnight. A DST change was not run.
- Network bytes from real per-app rates (needs elevation).
- Release build timings (only debug measured).
