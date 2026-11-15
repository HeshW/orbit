# Feature: per-app rules (v0.9, 11a), Optimize > Rules

Set an app's CPU priority (and which processors it may use) once. Orbit applies it every
time the app starts. Example: "League of Legends.exe" sometimes starts at Low; a rule sets
it to High on every launch.

Files: `src-tauri/src/rules.rs`, `ui/js/rules.js` (status for the badge + the rule dialog),
`ui/js/views/optimize/rules.js`, `ui/css/views/optimize/rules.css` (prefix `ru-`),
`ui/js/mock/rules.js`. Shared files touched: `lib.rs` (module, 9 commands, `rules::init` after
`boost::init`), `sampler.rs` (`rules::on_snapshot` after `boost::on_snapshot`), `actions.rs`
(`ACCESS_DENIED`, `validate_mask`, `read_affinity` made `pub(crate)`), `boost.rs` (`pick_exe`
takes the dialog title), `views/optimize.js` (the tab), `views/processes.js` (menu item,
badge), `css/views/processes.css`, `index.html`, `mock/index.js`, `tests/ui-smoke.mjs`.

## What a rule is

`{ id, match: { kind: "path" | "name", value }, priority?, affinity?, enabled }`

- **match**: `name` compares the exe file name (any folder); `path` compares the full path.
  Both ignore case; paths are normalised (slashes, `\\?\` prefix). A path rule beats a name
  rule for the same process. A process whose path Windows hides can only match by name.
- **priority**: `low | belowNormal | normal | aboveNormal | high`. **Never Realtime**: the
  backend rejects it ("Orbit never sets Realtime. It can freeze Windows.") and the UI does not
  offer it.
- **affinity**: a decimal core mask string (like `set_affinity`). Must keep at least one core
  and fit the PC's logical processors.
- At least one of priority and affinity. One rule per app (same kind and value).
- Stored in `rules.json` in `<app local data>\rules` (`{ rulesEnabled, rules }`). Debug builds
  only: `ORBIT_RULES_DIR` overrides the folder. Bad or refused entries in the file are dropped
  on load.

## How it applies (no thread, no WMI, no ETW)

The sampler calls `rules::on_snapshot` after each tick, the same list Boost reads.

1. **New process**: a row whose (pid, start time) is not known yet is checked against the
   rules. A match is applied in that same tick. A row with no rule goes into a "checked" set,
   so it costs nothing on later ticks (the set is pruned when it outgrows the process list).
   Saving, switching or deleting a rule clears what was learned, so the running processes are
   compared again at once: a new rule applies to the running app right away.
2. **Kept applied**: each tick, for processes that hold a rule only, read the priority
   (`GetPriorityClass`) and the affinity mask. If one differs, apply it again.
3. **Give up**: a process that is changed back 3 times within 60 s is left alone for the rest
   of that run ("Kept being changed back by something else."). The first two are re-applied.
   A new run of the app (new start time) gets the rule again.
4. **Failure**: recorded once per process instance, with a plain reason; no retry loop.
   - Access denied and Orbit not elevated: "This app runs as administrator. Restart Orbit as
     administrator to change it." (`needsAdmin: true`; the Rules tab offers Restart as
     administrator).
   - Access denied and Orbit elevated: "Windows or the app's protection blocked this change."
   - The process ended meanwhile: nothing is said.
5. Changes go through `actions::set_priority` / `actions::set_affinity` (so `guard_pid` and the
   critical-process guard apply). Reads verify the start time on the handle.

## Never touched

A rule cannot be created for, and the engine skips: Orbit (`orbit.exe`, its pid, its WebView2
helpers by `guard_pid`), Windows' own processes (csrss, wininit, smss, lsass, lsaiso,
services, winlogon, svchost, dwm, fontdrvhost, System, Registry, Memory Compression, ...) and
anything under `%SystemRoot%`, rows grouped as Windows, Windows Security (MsMpEng, NisSrv,
MpDefenderCoreService), and anti-cheat (vgc.exe, vgtray.exe, `vgk*`, `EasyAntiCheat*`,
`BEService*`, `BEDaisy*`, `EQU8*`, `xigncode*`, `gameguard*`, `faceit*`). The create command
returns the plain reason (see `rules::refusal`).

This feature never writes Image File Execution Options (the "apply even without Orbit" idea,
11c, was declined).

## The tab and the menu

- **Optimize > Rules** (after Boost): one row per rule (icon, name, "Any folder" or the path,
  what it sets, last applied or failed with the reason, a Running chip, on/off switch, edit,
  delete with a confirmation), **Add rule** (pick a running app or Browse for an exe), and a
  collapsible **Log** (last 200 lines: applied, blocked, changed back).
- **Processes right-click > "Always use this priority..."** (single processes and app
  groups): the same dialog, filled in for that exe (name or this file only, priority without
  Realtime, optional processor picker). If a rule already covers the process, it opens that
  rule to edit.
- **Badge**: a small bolt on the row (green when applied, amber when it failed or stopped).
  Tooltip: "Rule: High priority, 2 cores. Applied 4 min ago." or the reason. The statuses
  arrive in `orbit://rules` only when one changes, so a row costs one map lookup per redraw.

## Tests

- `cargo test rules::`: matching (name, path, normalisation, precedence), validation
  (Realtime, nothing to set, cores, duplicates), the guard list, the revert counter (3 in 60 s,
  old ones expire), the engine against a fake OS (apply, already set, re-apply twice then
  stop, affinity, access denied both ways with no retry, exited, master switch, log cap),
  the file round trip, and `engine_against_a_real_scratch_process`: copies `PING.EXE` to a
  scratch folder as `orbit-rule-test.exe`, runs it hidden, sets High and one core, changes
  them back from outside (re-applied twice), a third change stops it, then kills that pid and
  deletes the folder. It never touches another process.
- `tests/ui-smoke.mjs` "rules": create from the Processes menu (Realtime not offered), badge,
  Rules list, edit, switch, refused names, log, delete.

## For the next part

- The master switch exists in the backend (`rules_set_enabled`, `rulesEnabled` in
  `rules_list`); there is no switch in the UI yet.
- Boost and rules both change priority. A rule does not know about Boost: if Boost lowers an
  app a rule raised, the rule puts it back (one revert counts toward the 3).
- "Reset to default" per rule is not built: deleting a rule leaves a running app as it is.
