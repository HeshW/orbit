# Feature: autoruns (v0.4)

The Startup view (04) becomes "Autoruns lite": the existing Run keys / Startup folder
table stays as the **Apps** tab, unchanged, and four read-only tabs are added:
**Scheduled tasks**, **Services**, **Logon** and **Image hijacks**. `list_autoruns` is
built as in `docs/V04-CONTRACT.md`. No shapes or names changed.

Files: `src-tauri/src/autoruns.rs`, `ui/js/views/startup.js`, `ui/css/views/autoruns.css`,
`ui/js/mock/autoruns.js`.

## Backend (`autoruns.rs`, read only)

Nothing here writes anything. Registry keys are opened with `KEY_QUERY_VALUE |
KEY_ENUMERATE_SUB_KEYS` only; the service manager with connect/enumerate and services
with `SERVICE_QUERY_CONFIG`; Task Scheduler is only read. A category that cannot be
read comes back empty and the others still load. Runs in `spawn_blocking`.

| category | how |
|---|---|
| `tasks` | Task Scheduler 2.0 COM (`ITaskService`, MTA on the blocking thread), every folder from `\` down (`GetFolders`, `GetTasks(TASK_ENUM_HIDDEN)`). A task is kept when an **enabled** trigger is At log on or At system start; disabled triggers are skipped. `triggers` lists every enabled trigger in plain words: "At log on", "At system start", "Daily at 03:00", "Weekly on Monday, Friday at 03:00" ("Weekly on every day at ..."), "One time at 02:00", "Monthly at ...", "When idle", "When created or changed", "On an event", "On lock" / "On unlock" / "On local connect" ..., "Custom trigger". `command` = each action joined by "; " (exec: path + arguments; COM handler: "COM handler {CLSID}"). `path` = the first exec action's program: env vars expanded, quotes stripped, bare names found in System32 / Windows, ".exe" added when Windows would. `author` from RegistrationInfo. |
| `services` | `EnumServicesStatusExW(SERVICE_WIN32 \| SERVICE_DRIVER)`, then `QueryServiceConfigW` per service; kept when the start type is auto, autoDelayed, boot or system. `path` = the binary parsed from the image path (`lpBinaryPathName`, i.e. ImagePath): `\??\`, `\SystemRoot\`, relative `System32\...`, forward slashes, quoted and unquoted-with-spaces paths, a missing ".exe"; a driver with no ImagePath = `System32\drivers\<name>.sys`; an svchost service = its `ServiceDll` (Parameters key first). Services whose binary is signed by Microsoft are dropped, so `microsoft` is always `false` in the result. |
| `logon` | `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon` (64-bit view) `Shell` and `Userinit`. `shellOk` = value is exactly "explorer.exe" (trimmed, any case). `userinitOk` = "<SystemRoot>\system32\userinit.exe" with or without the trailing comma, any case (also accepts the literal `C:\Windows\...`). |
| `imageHijacks` | `HKLM\...\Image File Execution Options`, 64-bit and 32-bit views, every subkey with a non-empty `Debugger` value; de-duplicated, sorted by image. |

Signatures: `crate::security::signatures_for` (the security feature's `pub(crate)` fn), so
the WinVerifyTrust/catalog code and its cache are shared, not duplicated. "Microsoft" =
status signed and the signer starts with "Microsoft", **except** "Microsoft Windows
Hardware Compatibility Publisher": that is the WHQL signature on third-party drivers
(NVIDIA, ASUS, Google Drive ...), so those stay listed and show that signer as publisher.

## UI (`views/startup.js`)

- A tab list under the view head: Apps, Scheduled tasks, Services, Logon, Image hijacks,
  each with a count of rows matching the global search (Logon shows the number of values
  that are not the Windows default, in the warning colour, and nothing when both are
  fine). Arrow keys / Home / End move between tabs (one tab stop). The Enabled/Disabled
  stats only show on Apps.
- Apps: the existing table, filters, impact column, "Measure at next sign-in" and menus,
  untouched (the old `render` is now `renderApps`, called from `render`).
- Tasks / Services / Image hijacks: a help line ("... Read only."; Tasks adds "Tasks of
  other accounts show after Restart as administrator." when unelevated), "N of M",
  refresh, and a table: name (task leaf + folder, service display name + service name,
  image), command / path / debugger, triggers or start type, publisher (signer, or
  "Unsigned" warn chip, "Invalid signature" danger chip, "Can't check" when the file
  can't be opened, "No file" for COM handlers), task status, and three row buttons:
  Open file location, Search online, Check on VirusTotal (`checkOnVirusTotal` from
  `views/security.js`: `file_sha256` + `open_url`, only the hash leaves the PC). File
  buttons are disabled when there is no file. Image hijack publishers come from
  `get_signatures` on the debugger's program.
- Logon: an OK or warning panel with the text "Windows starts these at sign-in. Anything
  after explorer.exe here also runs at every sign-in.", both actual values, a
  Default / Not the default chip each and the Windows default under a changed one. No fix
  is offered: "Orbit does not change these values."
- Search uses `store.query` (names fuzzy; commands, paths, authors, publishers, triggers
  and start types by substring). Loading, error (with Try again) and empty states per tab.
  The list loads the first time the view opens, alongside `list_startup`.

## Mock (`mock/autoruns.js`)

Shaped like the real result: 8 tasks with log-on/boot triggers (Microsoft, COM handler,
Google, a disabled NVIDIA one, an unsigned AppData one, one that can't be checked),
8 non-Microsoft services (WHQL driver, system-start driver, unsigned, missing file), a
logon Shell with one extra program (warning), no image hijacks. URL flags: `?hijacks`
(two hijacks), `?logonok` (defaults), `?autorunsfail` (command throws).

## How to test

- `cargo test` in `src-tauri`: `autoruns::tests` (command/path parsing incl. `\SystemRoot\`,
  `\??\`, forward slashes, svchost ServiceDll lookup; logon default checks; Microsoft vs
  WHQL; trigger time and weekday text; live read of all four categories).
- `cargo test --lib dump_autoruns -- --ignored --nocapture` prints what this PC returns.
- Live: debug exe with CDP, `await __TAURI__.core.invoke('list_autoruns')`, then open
  Startup and each tab.
- Mock: serve `ui/`, open Startup, try the URL flags above and the global search.

Measured on the dev PC (debug build, unelevated): 50 tasks, 52 services, 0 hijacks;
first call 1.2-1.4 s (signatures), repeat 0.5-0.7 s (signature cache). Logon on this PC
is **not** the default (see below).

## Not verified

- Elevated run (tasks of other accounts; every service config should already be readable).
- A PC with real image hijacks (rendering checked with the mock only).
- "Open file location", "Search online" and "Check on VirusTotal" on the live app: not
  clicked, to avoid opening Explorer or the owner's browser. They call the existing
  `open_location` / `search_online` / `file_sha256` + `open_url` commands; checked in the mock.
- Per-user overrides (`HKCU\...\Winlogon\Shell`) and other Winlogon values are out of scope.

## Found on the dev PC (not changed)

`HKLM\...\Winlogon\Shell` on the owner's PC is
`explorer.exe, "C:\Users\Hesh\AppData\Local\google\chrome\user data\wzone.exe" "C:\Users\Hesh\AppData\Local\google\chrome\user data\wtime.cmd" wlocale.cmd`
(Userinit is the default). Programs placed in a Chrome "user data" folder and started
from the Shell value is a known malware persistence pattern. At the time of the check the
three files did not exist at those paths (nor `wlocale.cmd` in the Windows folders). Orbit
only reads this value; nothing was written.
