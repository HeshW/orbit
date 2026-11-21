# Feature: network usage over time (v1.5)

The Network view's Usage tab: how much this PC downloaded and uploaded in the last hour, 24
hours, 7 days or 30 days, and which apps used it. Command and shapes in `docs/CONTRACT.md`
(v1.5); ROADMAP section 17.

Files: `src-tauri/src/netusage.rs` (tests in `netusage/tests.rs`), `ui/js/views/netusage.js`,
`ui/css/views/netusage.css`, `ui/js/mock/netusage.js`; the tabs live in `views/network.js`.

## Backend (`netusage.rs`)

| item | how |
|---|---|
| `record(snapshot, interval)` | Sampler thread, every tick, right after App history. Skipped while App history's switch is off (`history::is_enabled`). Adds `rate x interval` to the 5-minute bucket the tick falls in: the machine from `snapshot.network.rxBps/txBps` (the adapters, virtual ones filtered as in the sampler; no admin needed) and each app from its rows' `netRxBps/netTxBps` (only filled while per-app network runs, so admin only). Apps use App history's key (lowercased exe path, else lowercased name); the System Idle Process is skipped. The machine total is also added to its local day. |
| storage | `<history dir>/network.json` (`ORBIT_HISTORY_DIR`, else `%LOCALAPPDATA%\me.legacytech.orbit\history`): `{ version, names, buckets, days }`. Buckets are kept while they overlap the last 24 hours, days for 30 local days (today counts as one), names only while a bucket uses them. Read once when the folder is known (ticks recorded before that are added to it), rewritten whole at most once a minute, when recording is turned off and on exit (temp file + rename). A broken file is skipped with a log line. |
| `get_network_usage(period)` | `spawn_blocking`. `hour` / `day`: the buckets that overlap the last 3600 / 86400 s, so the edge is rounded to 5 minutes. `week` / `month`: machine totals of the last 7 / 30 local days, and per-app bytes from `history::history_rows(7 / 30)` (App history's day files plus what is still in memory). `since` is the earliest tick counted in the period. Apps sorted by down + up. Any other period is "Pick a period: ...". |
| Delete history / Remove from history | `clear_history` also empties memory and deletes `network.json`; `remove_history_app` drops that key from the buckets and rewrites the file. |
| locking | One mutex. `usage()` lets go of it before reading App history, and App history never holds its own locks while asking for this one except in Delete/Remove (history `IO` first, then this), so there is no lock-order cycle. |

## UI (`views/netusage.js`)

- Network view: tabs Connections / Usage under the head (remembered in
  `localStorage["orbit.networkTab"]`). On Usage the connection counts in the head hide and
  the 2 s connection refresh stops.
- Toolbar: Last hour / 24 hours / 7 days / 30 days (remembered as `orbit.netUsagePeriod`,
  default 24 hours), "N apps", refresh.
- Totals: Download, Upload, Apps, Since ("Today 14:03", "Yesterday 09:10", "8 Nov").
- One note: recording off (Turn on recording, which sets `historyEnabled`); no per-app numbers
  and not admin (Run as admin); some per-app numbers but not admin now ("cover only the time
  Orbit ran as administrator"); otherwise that it counts only while Orbit runs and stays on
  this PC. It updates when a snapshot's per-app status changes.
- Table: App (icon, path tooltip), Download, Upload, Total with a bar, Share of all apps in
  the period (hidden under 900 px). Sortable; the global search filters by name and path.
  Empty states: reading, error with Try again, no search match, no per-app numbers, no app
  used the network.
- Reloads on show, on period change, every 30 s while visible, and when the window shows again.
- Palette: "Network usage" opens the tab; "Network connections" and "Find port" switch back.

## Mock (`mock/netusage.js`)

Machine totals per day from a fixed seed; per-app bytes from the History mock (so they match
it): hour and 24 hours only after the mock's Run as admin, 7 and 30 days from the history days
with network bytes. Delete history empties it. Flags: `?nonetusage`, `?netusagefail`.

## How to test

- `cargo test --lib netusage`: bucket and day placement, a clock set back, the window overlap
  for an hour and 24 hours, day totals, pruning (24 h, 30 days, unused names), merging the
  file into ticks recorded before it, forgetting an app, the file round trip and a broken
  file, grouping rows by app (case-insensitive paths, idle process and NaN skipped), and an
  unknown period.
- `npm run test:ui`: the Usage tab in every theme (totals, 30 days lists apps with a share,
  the admin note, back to Connections), then as admin (last hour lists apps, the note
  changes), `?nonetusage` and `?netusagefail`.
- Live: start the exe with `ORBIT_HISTORY_DIR` on a scratch folder and CDP on,
  `await __TAURI__.core.invoke('get_network_usage', { period: 'hour' })`, wait a minute for
  `network.json`.

Checked: `cargo test` (321 passed, 14 ignored), `npm run test:ui` (92 checks), the mock in the
browser at 1320x840 and 960x620 unelevated and as admin, no console errors.

## Not verified

- The real exe: the owner's installed Orbit was running, so no test copy could start.
  `record` on real snapshots, `network.json` being written, read back after a restart and
  deleted by Delete history were not run live.
- Per-app bytes from real ETW rates (needs elevation; the 0.4 per-app path itself is still
  untested elevated).
- A real midnight, the 24-hour prune over a long session, and a clock change.
