# Feature: game / focus boost (v0.5, 7d), Optimize > Boost

"Boost now", or auto mode when a listed game starts, gives the game more of the PC:
- the apps on the user's list get **Below normal** priority, or, with the pause option, are
  **paused** (suspended);
- the power plan switches to **Ultimate Performance**, else **High performance** (a toggle
  skips this);
- the Boost tab gets an "On" badge and the tray tooltip says "Orbit · Boost on · ...".

Turning it off (by hand, when the last listed game exits, when Orbit exits, or on the next
start after a crash) puts back each process's own priority, resumes what Boost paused and
restores the power plan. Shapes and commands are in `docs/CONTRACT.md` ("v0.5").

Files: `src-tauri/src/boost.rs`, `ui/js/views/optimize/boost.js`,
`ui/css/views/optimize/boost.css`, `ui/js/mock/boost.js`. Shared files touched: `lib.rs`
(module, 6 commands, `boost::init` in setup before the sampler, `boost::on_exit` in
`exit_cleanup`), `sampler.rs` (`boost::on_snapshot` after each tick), `shell.rs` (tooltip),
`actions.rs` (helpers made `pub(crate)`), `store.js` (settings), `mock/index.js`,
`tests/ui-smoke.mjs`.

## The tab

- **Status card**: off, it says what Boost will do and when it last turned off (and what it
  put back). On, it lists what changed, grouped by exe ("chrome.exe ×8 Below normal (was
  Normal)", "Power plan High performance (was Balanced)"), what it left alone and why, and a
  Turn off button.
- **Apps to calm**: the list (remove with ×), a picker of running apps (apps with a window
  first; Windows' own exes are not offered) and "Busy right now": up to 8 of the heaviest
  apps in the user's session by CPU plus memory (1 GB counts like 10 % CPU), from the latest
  snapshot, each with Add.
- **Games**: "Turn on when a game starts" switch, the list (a running one shows "Running"),
  a running-app picker and **Browse** (Windows file picker, `.exe` only). Only the file name
  is kept. A name is never on both lists: adding it to one removes it from the other.
- **While Boost is on**: Below normal / Pause (switching to Pause asks first and the warning
  stays visible while it is chosen), and "Switch the power plan" with the current and target
  plan named.
- After a crash recovery, a banner says what the start put back.
- Changing the lists while Boost is on applies from the next boost (a note says so), except
  that new processes of listed apps are picked up while it is on (see below).

## Rules (unit-tested with fakes in `boost.rs`)

Which processes:
- Only processes whose exe name is on the list (ignoring case), in Orbit's own session.
  Session 0 (services) and rows with no session are never touched.
- Never: rows grouped as Windows, any exe under `%SystemRoot%` (explorer.exe has a window
  and is grouped as an app), a listed game, Orbit (its pid, any `orbit.exe`), pid 0 and 4.
- Then the OS checks shared with the Processes view: `actions::guard_pid` (Orbit and its
  WebView2 helpers) and `actions::refuse_critical_suspend` (the desktop-critical list,
  `WINDOWS_CRITICAL` and the critical-process flag). Refusals, access denied and the like are
  listed as "left alone" with the reason.
- Priority is lowered only from Normal, Above normal or High. Idle and Below normal stay (so
  Boost never raises anything), and Realtime is never touched.
- Pause skips processes Orbit already suspended. Processes Boost pauses are added to
  `SuspendedSet`, so the Processes view shows them as suspended and `resume_all` also wakes
  them.
- While Boost is on, new processes of listed apps (a browser opening tabs) are handled on the
  next snapshot, in the mode the boost started with.

Power plan:
- Plans come from `PowerEnumerate` / `PowerReadFriendlyName`. Target: Ultimate Performance
  (GUID `e9a42b02-...`, or a plan named "Ultimate Performance", since the plan is usually a
  copy with a new GUID), else High performance (`8c5e7fda-...` or that name). Names are only
  matched in English; other languages rely on the GUIDs.
- No target plan: nothing changes and the note says so. Already on it: nothing to switch or
  restore. A failed switch is noted; Boost still runs.

Saving and restoring:
- The plan (every process with its original priority, and both plan GUIDs) is written to
  `boost-state.json` **before the first change**. If that write fails, Boost does not start
  and nothing changes. After applying, the file is written again with only what took effect.
  Writes go to a temp file and are renamed over the old one.
- Restore checks each pid against its start time (as every Orbit action does), so a pid that
  Windows reused is never touched. Exited processes are counted and skipped quietly.
- A process whose priority is no longer Below normal (the user changed it during Boost) is
  left as it is. Paused ones are always resumed.
- The plan is restored only if the active plan is still the one Boost set. If the user picked
  another plan meanwhile, that one stays ("The power plan was changed while Boost was on, so
  Orbit left the new one."). If the active plan can't be read, the old one is set anyway.
- `boost::init` (setup, before the sampler) restores a `boost-state.json` a crash left behind
  and deletes it, also when something could not be restored, so a start never loops. The
  status's `recovered` report drives the banner. `boost::on_exit` (from `exit_cleanup`)
  restores only this process's own boost, never someone else's file.
- Files: `ORBIT_BOOST_DIR` when set, else `<app local data>\boost` (`%LOCALAPPDATA%\
  me.legacytech.orbit\boost`): `boost-config.json` (last config from the UI) and
  `boost-state.json` (only while on).

Auto mode:
- No thread of its own: the sampler calls `on_snapshot` after each tick. On when a listed
  game is in the process list and Boost is off; off when an auto boost sees no listed game.
- A manual boost is never turned off automatically. An auto boost turned off by hand stays
  off until the listed games have all exited. If an auto start fails it is not retried until
  the games exit.
- Turning auto mode off during an auto boost still ends it when the game exits.
- The config lives in the UI settings (`boostGames`, `boostApps`, `boostAuto`, `boostPause`,
  `boostPower`). The Boost tab sends it with `boost_configure` when it mounts and on every
  change, and Rust saves a copy, so auto mode works from the next start before the tab is
  opened.

## How to test

- `cargo test boost::` (18 tests, plus the ignored live one): target rules, lowering and exact restore, idle never
  raised, exited / reused-pid / user-changed processes, pause mode, refusals and failed
  changes, "nothing changes when the plan can't be saved", power switch / restore / left /
  missing / already on / refused, plan choice, GUID format, auto decisions, running games,
  config cleaning, suggestions, crash recovery through the state file (including a damaged
  file), the `ORBIT_BOOST_DIR` rule, a read-only check of the real power API, and the real
  guard refusing Orbit and System.
- `cargo test boost::tests::live -- --ignored --nocapture --test-threads=1` changes only
  `ping.exe` processes the test starts: two lowered and one at Idle (left alone), one killed
  mid-boost, restored through the crash-recovery path; then one paused for 5 s (a 3 s ping
  must still be running) and resumed. With `ORBIT_BOOST_LIVE_POWER=1` it also does one real
  power plan switch to High performance (Ultimate is hidden from it, so it switches even on
  a PC already on Ultimate) and restore, and compares `powercfg /getactivescheme` before and
  after. A guard puts the plan back if the test panics. Run the power part once per check-up.
- `$env:ORBIT_BROWSER="msedge"; npm run test:ui` checks that Boost shows its status in
  every theme.
- Mock (serve `ui/`): Balanced is on and High performance exists. Boost now changes the mock
  process list through the built-in `set_priority` / `suspend_process`, so the Processes
  view shows it. Browse "picks" `hollow_knight.exe`. Auto mode reacts only when the config
  changes.

## Results on the dev PC

Active plan: a copy of Ultimate Performance (`76c3466b-...`), so a normal boost switches
nothing here.

- Live test with `ORBIT_BOOST_LIVE_POWER=1` (run once): Ultimate -> High performance ->
  Ultimate; `powercfg /getactivescheme` the same before and after. That run also found a bug:
  a process that had exited but whose handle was still held (a zombie) counted as restored.
  `open_verified` now treats an exited process as gone. The rerun without the power part
  passed: 2 lowered, the Idle one left alone, the killed one counted as exited, the paused
  ping stayed paused 5 s and finished after resume.
- Release exe, run live with its own WebView2, history, shell and boost folders, driven over
  the DevTools port, apps list = only `boostping.exe` (a copy of ping.exe outside Windows,
  since `C:\Windows\System32\PING.EXE` is correctly refused as Windows), power switch off:
  - Boost now: both test pings went Normal -> BelowNormal (checked with Get-Process), state
    file written, tab badge "On". Turn off: both Normal, state file gone.
  - Boost on, then Orbit killed by PID: pings stayed BelowNormal, state file present. On the
    next start both were Normal again, file gone, and the banner said "Orbit closed while
    Boost was on. When it started again it put back 2 processes."
  - Auto mode with a ping copy named `boostgame.exe` as the game: on within one tick
    (reason auto, the game itself left at Normal), off and restored after the game was
    killed.
  - The power plan stayed Ultimate Performance throughout.

## Not verified

- A real game: auto mode was run live with a ping copy standing in for the game.
- Pause mode in the release exe (it was run in the live cargo test, not through the UI).
- Exit restore through `exit_cleanup` (closing the window normally): the live run killed
  Orbit, which tested the crash path instead.
- The tray tooltip text was unit-tested only, not looked at live.
- Elevated runs: an elevated Orbit can lower elevated apps; an unelevated restore after an
  elevated crash fails for those and reports them.
- The Browse file picker was not opened live.
- Non-English plan names other than the built-in GUIDs.
- Auto mode stops reacting while sampling is paused (Space / Pause updates), since it reads
  the sampler's snapshots.
