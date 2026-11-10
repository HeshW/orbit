# Feature: disk cleanup (v0.5, 7c), Optimize > Cleanup

Two panels. **Junk files** sizes a fixed list of junk categories and deletes only the ones the
user ticks, after a confirm. **Biggest folders** measures a drive or folder and lets the user
drill down and open folders in File Explorer. It never deletes anything. Health's "Free up
space" button opens this tab. Shapes and commands are in `docs/CONTRACT.md` ("v0.5").

Files: `src-tauri/src/cleanup.rs`, `ui/js/views/optimize/cleanup.js`,
`ui/css/views/optimize/cleanup.css`, `ui/js/mock/cleanup.js`. Shared files touched: `lib.rs`
(module and 5 commands), `api.js` (`onEvent(name, cb)` plus the mock event bus `ctx.emit`),
`mock/index.js`, `tests/ui-smoke.mjs` (checks that Cleanup lists categories).

## Categories

The UI sends only category ids. Rust works out the folders.

| id | what is cleaned | admin | 24 h rule |
|---|---|---|---|
| `user-temp` | contents of `%TEMP%` (`std::env::temp_dir`) | no | yes |
| `windows-temp` | contents of `<Windows>\Temp` | yes | yes |
| `update-cache` | contents of `<Windows>\SoftwareDistribution\Download` | yes | yes |
| `crash-dumps` | contents of `%LOCALAPPDATA%\CrashDumps` | no | no |
| `system-dumps` | contents of `<Windows>\Minidump`, and the file `<Windows>\MEMORY.DMP` only | yes | no |
| `thumbnails` | files `thumbcache_*.db` directly in `%LOCALAPPDATA%\Microsoft\Windows\Explorer` | no | no |
| `browser-edge` / `-chrome` / `-brave` | in `...\User Data`: `ShaderCache`, `GrShaderCache`, and for each profile folder (one that has a `Preferences` file) `Cache`, `Code Cache`, `GPUCache` | no | no |
| `browser-firefox` | `%LOCALAPPDATA%\Mozilla\Firefox\Profiles\<profile>\cache2` (the Local profile holds only caches) | no | no |
| `recycle-bin` | `SHQueryRecycleBinW` / `SHEmptyRecycleBinW` (every drive, no UI, no sound) | no | no |

`<Windows>` comes from `GetSystemWindowsDirectoryW`, `%LOCALAPPDATA%` from the known folder.

Assumptions and choices that differ from the brief:
- Crash dumps are two categories. The user's own dumps don't need admin, so they can be
  cleaned without elevation. Windows' dumps (`system-dumps`) do need admin.
- The 24-hour rule also covers the Windows Update download cache, so a download in progress
  stays. Windows fetches anything removed again.
- Browser caches are skipped only when that browser's exe (`msedge.exe`, `chrome.exe`,
  `brave.exe`, `firefox.exe`) is running. Edge often runs in the background (startup boost),
  so its cache is often blocked. The UI says "Edge is open. Close it to clear its cache."

## Safety rules (unit-tested in `cleanup.rs`)

- **Allow-list.** Clean deletes only inside the roots that `targets()` computes for the given
  ids. Unknown ids are an error ("Unknown cleanup category."), so a path can't be passed in.
- **Never the root.** `is_within(root, path)` is strict. It compares whole components,
  ignores case, and treats `\\?\`, `\\?\UNC\` and `/` the same as plain paths. A path with
  `.` or `..` components, a relative path, a device path or a volume-GUID path never counts
  as inside.
- **Root checks.** A root is refused when it is a drive root, less than two folders deep, or
  equal to (or a parent of) a protected folder: the profile, Desktop, Documents, Downloads,
  Pictures, Music, Videos, LocalAppData, Roaming, ProgramData, Program Files (both),
  System32 or Windows. `%TEMP%` must also contain a `Temp` or `Tmp` folder in its path, so a
  broken TEMP variable can't point the clean at the profile. A root that is itself a reparse
  point is refused. The root's real location (`GetFinalPathNameByHandleW`) gets the same
  checks again.
- **No link following.** Folders are walked with `read_dir`. Reparse points (junctions,
  symlinks, cloud placeholders) are never entered and never deleted. They are counted as
  "links or other".
- **Checked delete by handle.** Each file, or empty folder, is opened with `DELETE` and
  `FILE_FLAG_OPEN_REPARSE_POINT`. Then Orbit checks that it is not a reparse point, that it
  is the expected kind, and that it is not read-only. Its real location must be strictly
  inside the root's real location. For `MEMORY.DMP` and the thumbnail cache, the file must
  sit directly in that folder and match the name. Only then is it deleted through that
  same handle (`FileDispositionInfo`). So a junction swapped into a parent folder
  mid-clean can't redirect a delete. Paths go to `CreateFileW` in `\\?\` form, so long
  paths work.
- **Never forced.** A sharing or lock violation is counted as in use. Access denied and
  read-only files are counted as denied. Orbit never changes attributes or kills handles.
- **24-hour rule.** A file stays when the newer of its created and modified times is less
  than 24 hours old. That way an installer's freshly unpacked files (old modified time, new
  created time) stay too. Folders are removed only when empty, and only if they were
  created more than 24 hours ago.
- **Admin.** Unelevated, admin categories are skipped ("Needs administrator rights."). The
  UI disables their tick box and shows "Restart as administrator".

### Test override

Debug builds only: `ORBIT_CLEANUP_ROOT=<folder>` maps every root into that folder:
`<f>\User\Temp`, `<f>\Windows\...`, `<f>\LocalAppData\...`, and the Recycle Bin becomes the
plain folder `<f>\RecycleBin`. Release builds ignore it. The scan result then has `testRoot`
set, and the UI shows a "Test mode" line. Tests build this layout directly with
`Layout::under`. `clean_all` asserts the test layout under `cfg(test)`, and emptying the real
Recycle Bin panics under `cfg(test)`.

## Jobs and progress

`cleanup_scan`, `cleanup_clean` and `cleanup_folder_sizes` run on blocking threads, one of
each kind at a time ("A scan is already running."). They send `orbit://cleanup-progress`
`{ job, label, step, steps, files, bytes, done }` at most every 120 ms, plus once per step and
at the end. `cleanup_cancel(job)` stops at the next file. For clean, what was already
deleted stays deleted, and the result has `cancelled: true`.

Biggest folders: sizes are the files' logical sizes. Cloud-only placeholders (recall or
offline attributes) count as 0. Directory links are not followed. Folders that can't be read
are counted and shown. The top 40 subfolders are listed and the rest are summed. A scan
keeps every folder up to 2 levels below the scanned one for 10 minutes, so drilling down
is instant. Deeper folders are measured on demand. The UI remembers results for Up and the
breadcrumbs. "Measure again" passes `fresh: true`. "Open folder" calls the existing
`open_location`, which opens Explorer with the folder selected.

## How to test

- `cargo test cleanup::` (23 tests). They cover path parts and containment (traversal,
  sibling prefixes, case, verbatim and UNC forms, long paths), root rules, temp shape, name
  patterns, the direct-child rule, id checks, and that the test and real layouts' roots pass
  the rules (the real one is read only). Real deletes, all in a test folder under
  `CARGO_TARGET_DIR\cleanup-tests`, cover: every category, junctions inside a root and as
  the root, a junction in the middle of a path caught by the handle check, the age rule,
  in-use and read-only files, a path over 400 characters, browser profile files kept and a
  running browser skipped, admin skipped, cancel, the scan, and folder sizes.
- `cargo test cleanup::tests::env_override -- --ignored --test-threads=1` runs the full
  `ORBIT_CLEANUP_ROOT` path (`Layout::current`) and cleans user temp and the Recycle Bin
  folder in the test layout.
- `cargo test cleanup::tests::live -- --ignored --nocapture` gives a read-only scan of this
  PC plus folder sizes of `src-tauri`.
- `$env:ORBIT_BROWSER="msedge"; npm run test:ui` checks that Cleanup lists categories in
  every theme.
- Mock (serve `ui/`): Edge is "open", Brave is missing, admin categories are locked until
  "Run as admin". Clean frees space with some files skipped. Biggest folders has a made-up
  tree on C: and D:.

Measured on the dev PC (unelevated, read-only live scan, 1.5 s): user temp 2.6 GB / 13,202
files (2.1 GB older than 24 h); Windows temp and Windows dumps unreadable without admin;
update cache 388 MB (23 MB older than 24 h); app crash dumps 336 MB; thumbnails 105 MB;
Edge 1.06 GB and Chrome 1.11 GB (both open, so blocked); Brave and Firefox not installed;
Recycle Bin 8 MB.

Live run of the release exe: unelevated, with its own WebView2, history and shell folders,
driven read-only over the DevTools port. Release builds ignore `ORBIT_CLEANUP_ROOT`, so the
scan showed the real folders. The scan took 1.3 s, with numbers matching the live test.
Progress events showed, and cancelling a C: measure after 3 s worked ("Scan cancelled.").
Measuring the repo folder and drilling into `src-tauri` worked, and the drill-down came from
the parent's cache. Clean and Open folder were not clicked.

## Not verified

- No real clean was run on this PC, as required: not on temp folders, not on browser
  caches, not on the Recycle Bin. `SHEmptyRecycleBinW` has never been called. Deleting was
  tested only in the test layout.
- Elevated runs (Windows temp, update cache, Windows dumps) were not seen live.
- A symlink (as opposed to a junction) inside a root: creating one needs Developer Mode or
  admin. It takes the same reparse-point path as junctions.
