# Feature: health (v0.5, 7a + 7b) and the Optimize view

A new sidebar view, **Optimize**, with four sub-tabs: **Health** (built here), **Cleanup**,
**Boost** and **Drivers** (placeholders for 7c, 7d, 7e). Health answers "is anything wrong
with this PC?"; startup suggestions (7b) show in Health and in the Startup view. Shapes
and commands are in `docs/CONTRACT.md` ("v0.5").

Files: `src-tauri/src/health.rs`, `ui/js/views/optimize.js`,
`ui/js/views/optimize/{health,suggestions,cleanup,boost,drivers}.js`,
`ui/css/views/optimize.css`, `ui/css/views/optimize/{health,cleanup,boost,drivers}.css`,
`ui/js/mock/health.js`. Shared files touched: `lib.rs` (module + 4 commands), `Cargo.toml`
(2 `windows` features), `index.html` (CSS links, `i-gauge` icon, nav item, section),
`app.js` (`VIEW_NAMES`, `KEY_VIEWS`), `palette.js` (Go to entry), `api.js` (mock `ctx.call`),
`mock/index.js`, `views/startup.js` (suggestions card), `tests/ui-smoke.mjs`.

## What the Health tab checks

Nothing here changes the system. Registry keys are opened read-only, WMI and Windows
Update are only queried.

| check | how | status |
|---|---|---|
| Disk health | Storage WMI (`ROOT\Microsoft\Windows\Storage`): `MSFT_PhysicalDisk` (HealthStatus, OperationalStatus, media and bus type), `MSFT_Partition` (drive letters), `MSFT_StorageReliabilityCounter` (wear, temperature, power-on hours, uncorrected errors). COM via the `windows` crate, `CoSetProxyBlanket` per proxy (no process-wide COM security change). | problem: unhealthy, predictive failure / error states, wear 90%+. warning: HealthStatus warning, other non-OK states, wear 70%+. Unelevated the counters are refused, so the item says "Restart as administrator to see wear, temperature and error counts." |
| Free space | sysinfo disks, per volume. Removable drives listed but not judged. | warning below 10% of the drive (at least 5 GB, at most 25 GB); problem below 3% (1 to 5 GB). Button: Free up space (Cleanup tab). |
| Restart | `HKLM\...\Component Based Servicing\RebootPending`, `...\WindowsUpdate\Auto Update\RebootRequired` (key exists), `Session Manager\PendingFileRenameOperations` (non-empty). | warning with one plain sentence per flag. No button: Orbit never restarts the PC. |
| Windows Update | Windows Update Agent COM: `IUpdateSearcher.Search("IsInstalled=0 and IsHidden=0 and Type='Software' and BrowseOnly=0")`, feature upgrades (Upgrades category) and duplicate titles left out; `IAutomaticUpdates2.Results` for the last search / install dates. Search only, never download or install. | warning: updates waiting, or Windows has not checked in 14+ days. Button: Open Windows Update (`ms-settings:windowsupdate`). "Check again" asks the server. |
| Uptime | `GetTickCount64` | warning from 14 days. |
| Startup load | `startup::collect_startup` + `impact.rs` measurement | unknown until measured; warning when high-impact apps exist that Orbit can suggest turning off. Button: Go to Startup. |
| Security scan | `security::security_scan` (its signature cache makes repeats fast) | problem with high findings, warning with medium; info findings do not count. Button: Go to Security. |

Windows Update is slow (seconds to minutes), so it runs on its own thread
(`orbit-update-check`, MTA). The first report starts a search from Windows' own cache
(offline, no network); the result is kept for 30 minutes. "Check again" starts an online
search. While a search runs the report says "Checking", and the tab polls
`get_update_status` every 3 s while visible, then reloads the report.

## Startup suggestions (7b)

`get_startup_suggestions`: enabled entries with a measured impact of medium or high,
ranked by first-minute cost (CPU seconds + MB read / 3), top 3. Never suggested: anything
under `%SystemRoot%`, anything signed by Microsoft (the WHQL "Hardware Compatibility
Publisher" signature does not count as Microsoft), antivirus / security software (a fixed
word list matched against name, path and signer: Defender, Malwarebytes, Norton, Avast,
Bitdefender, "security", "antivirus", ...), and Orbit itself. Low-impact apps are not
suggested.

The card ("These 3 apps add the most to your sign-in. Turn them off?") has one "Turn off"
button per app, which calls the existing `set_startup_enabled`; a turned-off app stays in
the card with Undo until the page reloads. Entries that need admin have the button
disabled with "Needs administrator rights." Nothing is ever turned off automatically. In
the Startup view the card sits above the Apps table (without the explanation line) and the
table refreshes after a change.

## Adding a sub-tab (for the 7c / 7d / 7e agents)

1. Replace the placeholder `ui/js/views/optimize/<id>.js` (keep `mount(paneEl, ctx)`,
   `onShow()`, `onHide()`). `mount` runs once, the first time the tab opens. Re-render only
   between `onShow` and `onHide`. `ctx.setBadge(text, 'warn'|'danger'|'')` sets the count
   next to the tab name; `ctx.openTab(id)` switches tab. Other views can call
   `openTab(id)` from `views/optimize.js`.
2. Styles in `ui/css/views/optimize/<id>.css` (already linked), prefix `cl-`, `bo-`, `dr-`.
3. Backend in `src-tauri/src/<id>.rs`, `pub mod` + commands in `lib.rs`.
4. Mock in `ui/js/mock/<id>.js`, added to `mock/index.js`; `ctx.call(cmd)` reaches other mock
   commands.
5. The smoke test already opens every sub-tab and checks it shows text.
6. A new tab in `TABS` (`views/optimize.js`) needs a pane id `op-pane-<id>` (made for you)
   and an entry in `OPTIMIZE_TABS` in `tests/ui-smoke.mjs`.

Notes for 7e: Windows Update driver offers are excluded here (`Type='Software'`). Health's
"Free up space" button opens the Cleanup tab.

## How to test

- `cargo test health::` (14 tests: thresholds, item texts, suggestion filtering and
  ranking, settings-page validation, OLE dates, live read-only probes of drives, WMI and
  the restart flags). `cargo test health::tests::live -- --ignored --nocapture` prints what
  this PC reports, including an offline Windows Update search.
- `$env:ORBIT_BROWSER="msedge"; npm run test:ui` visits Optimize and each sub-tab in all
  four themes.
- Mock: serve `ui/`, open Optimize. Windows Update shows "Checking" for about 3 s, then two
  pending updates. "Turn off" works on the mock startup entries.

Measured on the dev PC (unelevated, via the live test, which builds the same report the
command returns): 4 physical disks found with health and drive letters, no counters (as
expected); 6 drives; a pending file-rename restart flag; offline update search about 7 s,
2 pending after the Windows 11 upgrade offer was left out; startup not measured yet.

The release exe was built but not launched: the owner's elevated Orbit was running, and a
second Orbit hands its launch to the running one (single instance), which would have
touched the owner's window. The security item was therefore only checked through
`security_scan`'s own tests and the mock.

## Not verified

- Elevated run: the reliability counters (wear, temperature) were not seen live.
- A failing disk, a full drive or a pending CBS / Windows Update restart on a real PC
  (unit tests only).
- "Check again" (online search) and "Open Windows Update" on the real app were not
  clicked, to avoid touching the owner's update state and Settings.
- "Turn off" against real startup entries (tested in the mock only, as required).
