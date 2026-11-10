# Feature: Malware scan through Microsoft Defender (v0.6, 8e), Security

Orbit does not ship an antivirus engine. The Security view gets a **Malware scan** panel at the
top that shows Microsoft Defender's status and asks Defender to scan; Processes and History
get **Scan with Defender** in their right-click menus. Shapes, commands and the event are in
`docs/CONTRACT.md` ("v0.6").

Files: `src-tauri/src/defender.rs`, `ui/js/views/defender.js`, `ui/js/mock/defender.js`.
Shared files touched: `lib.rs` (module + five commands), `rig.rs` (`reg_sz` now `pub(crate)`),
`views/security.js` (panel mount and `showDefender` on show), `views/processes.js` and
`views/history.js` (menu item), `css/views/security.css` (`df-`), `mock/index.js`,
`tests/ui-smoke.mjs`.

## What it shows

- Defender is the antivirus: "Microsoft Defender is on" (or "Real-time protection is off", in
  the warning tone), the line "Scans use Microsoft Defender, built into Windows.", then
  Real-time protection, Definitions (Up to date / N days old / Out of date, warning from 7 days,
  with the version under it), Last quick scan, Last full scan ("Today, 10:22", "Yesterday, ...",
  "3 days ago", "Never") and the Defender app version with the engine version under it.
  Buttons: **Quick scan**, **Full scan** (asks first: it can take an hour or more), **Scan a
  file or folder…** (a small menu: A file… / A folder…, then the Windows picker).
- Another antivirus is on and Defender is not: "<Name> protects this PC", "Microsoft Defender
  is off because <Name> is your antivirus.", that product's status and definitions, and
  Defender as Passive / Off. No scan buttons; **Open Windows Security** instead. The menu item
  is disabled with the hint "Defender is off".
- Defender off and nothing else on: "Microsoft Defender is off" in the danger tone, one line
  saying to turn it on in Windows Security, and the Open Windows Security button.
- While a scan runs: "Quick scan running 0:42" (or "Scanning setup.exe 0:05") with a spinner
  and **Cancel**; the scan buttons are disabled. Results: "No threats found. Quick scan, 4:12."
  / "No threats found in setup.exe. Custom scan, 0:01." / "Defender found 2 threats in
  x.zip. Orbit doesn't remove anything; handle them in Windows Security." with each threat name
  as a chip and **Open Windows Security** (`windowsdefender://threat`) / "<kind> cancelled." /
  the failure message, with **Restart as administrator** when Defender refused for rights.
- From a menu, the scan reports by toast ("Scanning Code.exe with Microsoft Defender." then
  "No threats found in Code.exe." or "Defender found 2 threats in ..." with an Open Windows
  Security button). Quick / full / picked scans also toast when Security is not on screen.

## Rules

- Status: WMI `root\Microsoft\Windows\Defender` `SELECT * FROM MSFT_MpComputerStatus` (so a
  property missing on older Windows does not fail the query). Dates are CIM_DATETIME strings
  (`dmtf_to_unix`, offset in minutes); `FullScanEndTime` is empty when no full scan ever ran.
  Read only, no admin, about 50 ms.
- Other products: `root\SecurityCenter2` `AntiVirusProduct`. `productState` bits 12-15 are the
  state (0 off, 1 on, 2 snoozed, 3 expired), bits 4-7 the definitions (0 up to date). Defender
  is recognised by `pathToSignedProductExe` `windowsdefender://` or a name with "Defender" and
  "Windows" / "Microsoft" (not Bitdefender).
- Active = `AMServiceEnabled` and `AntivirusEnabled` and `AMRunningMode` is Normal. Without
  Defender's WMI (a third-party antivirus can turn it off), SecurityCenter2's Defender entry
  decides. `other` = a non-Defender product that is on, only while Defender is not active.
  `canScan` = active and MpCmdRun.exe found.
- Scanner: `MpCmdRun.exe` from `HKLM\SOFTWARE\Microsoft\Windows Defender` `InstallLocation`,
  else the newest `%ProgramData%\Microsoft\Windows Defender\Platform\<version>\`, else
  `%ProgramFiles%\Windows Defender\`. Run with `std::process::Command` (`-Scan -ScanType 1|2|3
  [-File <path>]`, the path as one argument, never a shell string), `CREATE_NO_WINDOW`, stdout
  and stderr piped, on a blocking worker. One scan at a time (a static slot; a second request
  gets "A Defender scan is already running."). Cancel sets a flag; the worker kills the child.
  Closing Orbit kills a running MpCmdRun too (`defender::on_exit` in `exit_cleanup`).
- Result: exit 0 = clean; exit 2 or any "Threat : <name>" line = threats (names in order, no
  repeats, at most 50); otherwise failed with the `hr = 0x...` code in the message
  (`0x80070005` = "Defender needs administrator rights for this scan." and `needsAdmin`). The
  output is ANSI (UTF-16LE is decoded too).
- Custom-scan paths: trimmed, `/` turned into `\`, must start with a drive letter and `:\`
  (no UNC, `\\?\`, `\\.\`, relative or drive-relative paths), no `"*?<>|`, control
  characters, alternate data streams or `.` / `..` parts, a local drive type (removable,
  fixed, CD, RAM disk; not network), and must exist as a file or folder. A trailing backslash
  is dropped except on a drive root.
- Orbit never removes, quarantines or restores anything and passes no remediation flags:
  Defender acts on what it finds by its own settings, and Windows Security shows the rest.
  Nothing is uploaded anywhere.

## How to test

- `cargo test defender::` (product states, Defender names, CIM dates, platform folder order,
  output decoding, result parsing for clean / threats / threats without names / access denied
  / other errors, path rules on real temp files, status combining incl. passive mode and no
  Defender WMI, one scan at a time) and `reads_real_status` with `--nocapture` (prints this
  PC's status and the MpCmdRun path, read only).
- One live custom scan of a harmless file, opt in only:
  `ORBIT_DEFENDER_SCAN_FILE=<file> cargo test real_custom_scan -- --ignored --nocapture`.
- Browser preview: Security shows the panel; Quick scan runs about 3 s, Full scan 6 s (Cancel
  works), "Scan a file or folder… > A file…" finds two threats; `?otherav` shows Norton 360.
  `tests/ui-smoke.mjs` "defender" covers all of these plus the Processes and History menus.

Measured on the dev PC (Windows 10 22H2, unelevated): Defender active, Normal mode, real-time
on, tamper protection on, definitions 1.463.223.0 from the same day, last quick scan the day
before, never a full scan, app 4.18.26100.4, engine 1.1.26100.4, SecurityCenter2 lists only
"Windows Defender" (397568: on, up to date). MpCmdRun found in the 4.18.26100.4-0 platform
folder. One custom scan of a small text file (path with spaces) unelevated: exit 0, "found no
threats", under 1 s, parsed as clean.

## Not verified

- Quick and full scans on a real PC (not run by rule; the mock covers the flow). Whether a
  standard user may start them depends on policy; a refusal shows Restart as administrator.
- A real detection (no test malware or EICAR was used): threat parsing is unit-tested on
  MpCmdRun's documented "Threat : <name>" output only.
- Cancel on a real scan: killing MpCmdRun ends Orbit's wait; Defender's service may finish
  the scan in the background.
- Another antivirus, passive mode and Defender switched off (covered by unit tests on
  `combine` and by the `?otherav` mock).
- The live exe: not started in this task (the owner's Orbit was running), so the panel, the
  picker and Open Windows Security were not seen in the real app.
