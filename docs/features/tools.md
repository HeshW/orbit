# Feature: tools (v0.4)

File locks ("What's locking this?"), End not responding, and settings Backup. Commands and
shapes as in `docs/V04-CONTRACT.md`; nothing renamed.

Files: `src-tauri/src/tools.rs`, `ui/js/tools.js`, `ui/js/views/processes.js`,
`ui/js/panels/backup.js`, `ui/css/views/tools.css`, `ui/js/mock/tools.js`.

## Backend (`tools.rs`)

`find_file_lockers(path)` runs on `spawn_blocking`. No admin needed.

| step | how |
|---|---|
| path | Trims spaces and surrounding quotes (Explorer's "Copy as path"), expands `%VARS%` (`startup::expand_env`), must be absolute. Errors: "Type the path of a file or folder.", "Type the full path, starting with a drive letter like C:\\.", "Orbit can't find that file or folder. Check the path and try again.", "Access denied. Try Restart as administrator." |
| a file | Restart Manager: `RmStartSession`, `RmRegisterResources` with the file, `RmGetList` with the two-call pattern (retries on `ERROR_MORE_DATA`), `RmEndSession` (in a `Drop` guard). |
| a folder | Walks the files inside (recursive, no symlinks, depth 32, at most 2,000). Restart Manager is far too slow for that many files: it spends about 80 ms per registered file (639 files in System32\drivers took 55 s). The cost is Windows' own "who has this file open" query (`NtQueryInformationFile`, `FileProcessIdsUsingFileInformation`, the same thing Restart Manager asks per file), so Orbit asks it directly on 32 threads, opening each file for attributes only with full sharing (never blocks another program; Orbit's own handle is not listed). The pids found are then registered with Restart Manager as applications (pid + start time, 16 per session) to get their app types, plus up to 25 files Orbit could not open itself. A failing Restart Manager session only loses app types; the pids are still listed. |
| result | One `Locker` per pid. Name = exe file name (`QueryFullProcessImageNameW`), else the Toolhelp name, else Restart Manager's app name. `exe` is null when Orbit can't open the process. `appType` from `RM_APP_TYPE` (main/other window = `app`); pids Restart Manager did not describe get `critical` (pid <= 4, "System") or `unknown`. Orbit itself is `app` (Restart Manager calls the asking process critical). Apps first, then by name. |

`ntdll!NtQueryInformationFile` is linked with `#[link(kind = "raw-dylib")]`, so no Cargo
change was needed.

Timing (debug build, this PC): a single file 50-180 ms; a small folder 140-260 ms;
System32\drivers (639 files) 6.3 s; %TEMP% (capped at 2,000 files, 86 processes) 24 s.
The busy state says a big folder can take up to half a minute.

Tests (`cargo test --lib tools::`): own pid found on an open file and gone after close;
locked file found inside a nested folder given as a quoted path; the pid probe lists only
real users; empty folder; error strings; walk cap; app type names. `folder_timing` is
`#[ignore]`d (prints phase timings for `ORBIT_LOCK_DIR`, default System32\drivers).

## UI

**`tools.js`**: `init()` registers the palette command "What's locking a file" (icon
`i-lock`). `openLockFinder({ path?, run? })` opens an `app.formDialog` ("Find locks" /
"What's locking this?", OK = Find). The input is prefilled with the last path checked
(localStorage `orbit.lockPath`). Find keeps the dialog open and shows: Checking, an error
(red, `role=alert`), "Nothing has it open." or the list (name, type chip, PID, full path or
"Path hidden. Try Restart as administrator." / "Part of Windows.", Details and End task).
End task uses `processes.endTask` with the row's start time; it is disabled for Orbit
itself and for `critical` lockers. When endTask asks first (Explorer, Windows processes)
its confirm replaces the dialog, so the finder reopens with the same path and checks again;
otherwise the list refreshes 0.6 s after ending. Details closes the dialog, clears a
search that would hide the row, goes to Processes and opens the drawer. Drop: in Tauri,
while the dialog is open, `tauri://drag-drop` fills the path and runs Find
(`tauri://drag-enter/leave` show a dashed outline). The browser preview has no file paths,
so a drop there shows "A dropped file's path isn't available here. Type or paste it
instead."

**`views/processes.js`** (everything else unchanged): toolbar buttons "Find locks" and
"End not responding" (ghost buttons before Run task; below 1240 px only their icons
show, the label stays for screen readers). End not responding is enabled only while some
row is `notResponding` (Orbit and the NO_END_ALL Windows processes are never included),
its tooltip names the app or the count. One `confirmDialog({ danger: true })` lists each
app (`name: window title (pid)`), then each is ended through `endTask(pid, name,
startTime, { confirm: false, quiet: true })` and one toast sums up ("Ended 2 apps that
were not responding.", "Ended 1 of 2. X: error", or the error). `endTask` gained an
optional fourth argument: `confirm: false` skips its own confirm, `quiet: true` shows no
toast and throws the error instead; existing callers are unaffected. New export
`endNotResponding()`.

**`panels/backup.js`**: Backup panel with Export and Import. Export sends
`JSON.stringify(store.settings, null, 2)` to `export_report` as
`orbit-settings-YYYY-MM-DD.json` (local date) and toasts the saved path. Import opens a
file picker (`.json`, `.txt`), max 512 KB, strips a BOM, parses, and keeps only valid
values by the same rules as `store.js` (theme, interval 250-10000, grouping, booleans,
historyDays clamped 1-90, watchlist/watchNames cleaned; other plain JSON keys that views
remember are kept). Not JSON or no known key: "<file> isn't an Orbit settings file. Pick
a file made with Export." Nothing different: "<file> matches your current settings."
Otherwise a confirm lists each change ("Theme: Night to Void", other view keys as "N other
remembered view choices", skipped invalid values named), then `setSettings` with only the
changed keys and a toast. Exports `validateSettings(raw)` and `describeChanges(patch)`.

**`mock/tools.js`**: sample lockers from the mock snapshot.
`E:\Work\Edits\client-reel.prproj` = Premiere (not responding) + Explorer; a folder under
`E:\Work` = Code + Explorer; `*.log` = svchost (service); other paths = nothing. Paths
containing "missing" or "denied" return those errors; relative paths are refused.

## Verified

- `cargo test` (89 passed, 4 ignored), `cargo build` with no warnings, `node --check` on
  every `ui/js` file.
- Browser preview with the mock: dialog states (results, empty, not found, relative path,
  folder), End task (no confirm and Explorer confirm, then reopen), Details, palette
  command, End not responding with one and two apps, Backup export, import (preview,
  apply, not JSON, array, unchanged), contrast of every new text style >= 4.5:1 in all four
  themes (lowest 6.58:1), all nine views open, no console errors.
- Live debug exe (CDP): a PowerShell started by the test holding a scratch file is found
  (file and its folder, `console` type); the same path filled through a simulated
  `tauri://drag-drop` event; End task on it (Windows confirm, then the finder reopened and
  showed "Nothing has it open."); System32\drivers shows System (End task disabled, "Part
  of Windows.") and WUDFHost; End not responding enabled for a deliberately hung test
  window (WinForms + busy loop), the confirm listed only it, ended, toast, button disabled
  again; Backup export wrote the file and import via a file input applied theme Void, then
  restored; no console errors.

## Not verified / known gaps

- A real OS file drop onto the window: only the `tauri://drag-drop` event was simulated.
- `export_report` only allows `.csv` and `.txt`, so the export is saved as
  `orbit-settings-YYYY-MM-DD.json.txt` until the contract change below lands. Import
  accepts both.
- Folders: a process whose current folder is the folder itself (not a file inside) is not
  found; files past the first 2,000 are not checked.
- Elevated runs were not tested (never triggered UAC).
