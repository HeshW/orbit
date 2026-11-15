# Feature: per-app rules (v0.9, 11a and 11b), Optimize > Rules

Set an app's CPU priority, which processors it may use, or efficiency mode once. Orbit applies
it every time the app starts. Example: "League of Legends.exe" sometimes starts at Low; a rule
sets it to High on every launch.

Files: `src-tauri/src/rules.rs` (tests in `src-tauri/src/rules/tests.rs`), `ui/js/rules.js`
(status for the badge, the rule dialog, the summary text), `ui/js/views/optimize/rules.js`,
`ui/css/views/optimize/rules.css` (prefix `ru-`), `ui/js/mock/rules.js`. Shared files touched:
`lib.rs` (module, 12 commands, `rules::init` after `boost::init`), `sampler.rs`
(`rules::on_snapshot` after `boost::on_snapshot`, and `rules::light_tick` while paused),
`actions.rs` (`ACCESS_DENIED`, `validate_mask`, `read_affinity` made `pub(crate)`;
`refuse_critical_change`, `process_elevated` added), `boost.rs` (`pick_exe` takes the dialog
title), `Cargo.toml` (`Win32_UI_Accessibility`), `views/optimize.js` (the tab),
`views/optimize/boost.js` and `css/views/optimize/boost.css` (a rule button per game),
`panels/backup.js` (rules in export and import), `views/processes.js` (menu item, badge),
`css/views/processes.css`, `index.html`, `mock/index.js`, `mock/boost.js` (`boostActive()`),
`tests/ui-smoke.mjs`.

## What a rule is

`{ id, match: { kind: "path" | "name", value }, priority?, affinity?, efficiency, inFront,
behind?, boostOnly, enabled }`

- **match**: `name` compares the exe file name (any folder); `path` compares the full path.
  Both ignore case; paths are normalised (slashes, `\\?\` prefix). A path rule beats a name
  rule for the same process. A process whose path Windows hides can only match by name.
- **priority**: `low | belowNormal | normal | aboveNormal | high`. **Never Realtime**: the
  backend rejects it ("Orbit never sets Realtime. It can freeze Windows."), the UI does not
  offer it, and the OS layer refuses it again before any call.
- **affinity**: a decimal core mask string (like `set_affinity`). Must keep at least one core
  and fit the PC's logical processors.
- **efficiency** (Windows 11, build 22000 and later; the checkbox is hidden on Windows 10):
  EcoQoS (`SetProcessInformation(ProcessPowerThrottling)`, execution speed throttled) plus Low
  priority, as Task Manager's efficiency mode does. The rule's priority must be empty (or Low);
  it can't be combined with "only while in front".
- **inFront**: the priority applies only while the app's window is in front; while it is
  behind the app runs at **behind** (`normal`, the default, or `belowNormal`). Needs a priority
  different from the behind one.
- **boostOnly**: the rule applies only while Boost is on. When Boost turns off, what the rule set
  goes back to Normal priority and every core ("Boost ended. Put back to ...").
- At least one of priority, affinity and efficiency. One rule per app (same kind and value).
- Stored in `rules.json` in `<app local data>\rules` (`{ rulesEnabled, rules }`). Debug builds
  only: `ORBIT_RULES_DIR` overrides the folder. Bad or refused entries in the file are dropped
  on load; a 0.9 part 1 file (without the new fields) loads as it was.

## How it applies (no WMI, no ETW, no polling)

The sampler calls `rules::on_snapshot` after each tick, the same list Boost reads.

1. **New process**: a row whose (pid, start time) is not known yet is checked against the
   rules. A match is applied in that same tick. A row with no rule goes into a "checked" set,
   so it costs two hash lookups on later ticks (the set is pruned when it outgrows the process
   list). The guard list is only consulted for rows that match a rule. Saving, switching or
   deleting a rule clears what was learned, so the running processes are compared again at
   once: a new rule applies to the running app right away. Boost turning on or off does the same.
2. **Paused or slow updates**: the sampler's interval can be 0.25 to 10 s, and Pause stops it.
   While an enabled rule exists and no full sample is due within a second, the sampler calls
   `rules::light_tick` about once a second instead: one Toolhelp snapshot, and only pids not
   seen before are opened (start time and path). The start time is the same conversion sysinfo
   uses, so a process keeps its key when updates resume (unit tested against sysinfo).
3. **Kept applied**: each tick, for processes that hold a rule only, one handle reads the
   priority class, the affinity mask and (efficiency rules) the power throttling state. Orbit
   remembers what it set last. If the process differs from that, something else changed it:
   Orbit applies it again.
4. **Give up**: a process that is changed back 3 times within 60 s is left alone for the rest
   of that run ("Kept being changed back by something else."). The first two are re-applied.
   A new run of the app (new start time) gets the rule again. Orbit's own changes (the window
   moving, Boost ending) never count, because only a difference from what Orbit set last is a
   revert.
5. **In front**: one thread holds an out-of-context `SetWinEventHook(EVENT_SYSTEM_FOREGROUND)`
   and sleeps in `GetMessageW`; it wakes only when the window in front changes. Every process of
   the rule that owns the front window counts as in front (an app can have several). The
   processes that moved get their priority at once; the next tick catches up if the hook ever
   misses one. If the hook can't be installed, in-front rules keep the "behind" priority.
6. **Failure**: recorded once per process instance, with a plain reason; no retry loop.
   - Access denied, Orbit not elevated, and the app runs elevated (or its token can't be read):
     "This app runs as administrator. Restart Orbit as administrator to change it."
     (`needsAdmin: true`; the Rules tab offers Restart as administrator).
   - Access denied while an anti-cheat process runs (vgc.exe for Riot Vanguard, Easy Anti-Cheat,
     BattlEye, FACEIT, ...): "Riot Vanguard (anti-cheat) blocked this change. Orbit won't keep
     trying."
   - Any other access denied: "Windows or the app's protection blocked this change."
   - The process ended meanwhile: nothing is said.
7. Changes go through `actions::set_priority` / `actions::set_affinity` (so `guard_pid` applies)
   after `actions::refuse_critical_change` (csrss and friends in System32, or a process Windows
   marks critical). Reads verify the start time on the handle.

## Never touched

A rule cannot be created for, and the engine skips: Orbit (`orbit.exe`, its pid, its WebView2
helpers by `guard_pid`), Windows' own processes (csrss, wininit, smss, lsass, lsaiso,
services, winlogon, svchost, dwm, fontdrvhost, System, Registry, Memory Compression, ...) and
anything under `%SystemRoot%` (checked on the running process's path too, so a name rule for
notepad.exe never touches Windows' own copy), rows grouped as Windows, Windows Security
(MsMpEng, NisSrv, MpDefenderCoreService, SecurityHealthService, Sense, and anything in a Windows
Defender folder), and anti-cheat (vgc.exe, vgtray.exe, `vgk*`, `EasyAntiCheat*`, `EAC_*`,
`BEService*`, `BEDaisy*`, `BattlEye*`, `EAAntiCheat*`, `EQU8*`, `xigncode*`, `gameguard*`,
`npgg*`, `faceit*`, `PnkBstr*`, `SGuard*`, `ACE-*`, `mhyprot*`). The create command returns the
plain reason (see `rules::refusal`).

This feature never writes Image File Execution Options (the "apply even without Orbit" idea,
11c, was declined) and installs no driver.

## The tab, the menu and Boost

- **Optimize > Rules** (after Boost): one row per rule (icon, name, "Any folder" or the path,
  what it sets, last applied or failed with the reason, a Running chip, on/off switch, edit,
  Reset, delete with a confirmation), **Add rule** (pick a running app or Browse for an exe),
  the **Rules** master switch (off: "Rules are off. Orbit isn't changing any app."), and a
  collapsible **Log** (last 200 lines: applied, blocked, changed back, put back).
- **Reset** (per rule, with a confirmation): the running app goes back to Normal priority, every
  core and efficiency mode off (only what the rule sets), and the rule switches off so Orbit
  doesn't apply it again. Delete leaves a running app as it is.
- **Processes right-click > "Always use this priority..."** (single processes and app
  groups): the same dialog, filled in for that exe (name or this file only, priority without
  Realtime, only while in front, efficiency mode on Windows 11, processors, only while Boost
  is on). If a rule already covers the process, it opens that rule to edit.
- **Boost**: each game in Boost's list has a rule button. "Add rule" opens the dialog for that
  game with "Only while Boost is on" ticked; a game that has a rule shows "Rule" (tooltip: what
  it sets) and opens it.
- **Badge**: a small bolt on the row (green when applied, amber when it failed or stopped).
  Tooltip: "Rule: High priority, 2 cores. Applied 4 min ago." or the reason. The statuses
  arrive in `orbit://rules` only when one changes, so a row costs one map lookup per redraw.
- **Backup**: Settings > Backup > Export writes the rules under `rules` (`rules_export`).
  Import asks Rust what would change (`rules_import` with `apply: false`), lists "Per-app rules:
  2 new, 1 changed", the master switch if it differs, and the rules it leaves out with the
  reason, then applies on confirm. A rule for an app that already has one replaces its settings.

## Tests

- `cargo test rules::`: matching (name, path, normalisation, precedence, Boost-only),
  validation (Realtime, nothing to set, cores, duplicates, in front, efficiency mode), summaries,
  the guard list, Realtime never reaching the OS, the revert counter (3 in 60 s, old ones
  expire), the engine against a fake OS (apply, already set, re-apply twice then stop,
  affinity, access denied with the administrator / anti-cheat / blocked texts and no retry,
  exited, System32 copies skipped, master switch, log cap), in front (switching many times is
  never a revert, a real change still is, the tick catches up without the hook), Boost-only
  (applied when Boost starts, put back when it ends), efficiency mode (set, watched, unreadable),
  Reset, import merge (added, replaced, unchanged, skipped with reasons, the rule limit), the file
  round trip (with the new fields, and a part 1 file), and `engine_against_a_real_scratch_process`:
  copies `PING.EXE` to a scratch folder as `orbit-rule-test.exe`, runs it hidden, checks the light
  tick's start time against sysinfo, sets High and one core, changes them back from outside
  (re-applied twice), a third change stops it, Reset puts it back to Normal and every core, an
  in-front rule follows a simulated window switch, and on Windows 11 efficiency mode sets Low;
  then kills that pid and deletes the folder. It never touches another process.
- `tests/ui-smoke.mjs` "rules": create from the Processes menu (Realtime not offered), badge,
  Rules list, edit, switch, refused names, log, delete. "rules 11b": the master switch (badges
  go and come back), in front through the dialog, Reset (Normal priority, rule off, log line), the
  new refusals, a game's rule from Boost, Export containing the rules and Import through the file
  input (one added, csrss left out with its reason), and efficiency mode only with `?win11`.

## Needs a real Windows PC

CI runs the unit tests on Windows Server, so these are checked by hand before a release:
an app running as administrator (the message, then Restart as administrator applying it),
League of Legends with Riot Vanguard (vgc.exe refused; the Vanguard message if League itself
is protected), real foreground switching between a game and another window, and efficiency mode
on Windows 11 (the leaf in Task Manager).

## Notes

- Boost and rules both change priority. A rule does not know about Boost's app list: if Boost
  lowers an app a rule raised, the rule puts it back (one revert counts toward the 3).
- The master switch off stops applying rules; it does not put apps back (use Reset per rule).
