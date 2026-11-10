# Feature: driver check (v0.5, 7e), Optimize > Drivers

Lists every present device with its driver and flags the ones worth a look: devices with a
problem code (Device Manager's yellow "!"), unsigned drivers and old third-party drivers.
Updates come from two places only: drivers Windows Update itself offers for this PC, and
the maker's official support page. Shapes and commands are in `docs/CONTRACT.md` ("v0.5").

Files: `src-tauri/src/drivers.rs`, `ui/js/views/optimize/drivers.js`,
`ui/css/views/optimize/drivers.css`, `ui/js/mock/drivers.js`. Shared files touched: `lib.rs`
(module + 3 commands), `Cargo.toml` (2 `windows` features), `health.rs` (the "Devices" item,
5 helpers made `pub(crate)`), `mock/index.js`, `mock/health.js`.

## The tab

- **Summary**: "N devices need a look" with chips per flag (a chip filters the list), and
  "This PC" (SMBIOS maker and model) with the PC maker's support page when it is one Orbit
  knows.
- **Drivers from Windows Update**: what Windows Update offers (title, device, provider,
  date, "Optional" for BrowseOnly offers), each with Install. "Check again" asks the server.
  The last install result stays under the list ("Installed. Restart the PC to finish.").
- **Devices**: search (name, class, maker, provider, version, INF, signer), a filter (All,
  Needs a look, Problems, Unsigned, Old drivers, Turned off, Windows Update has one) and
  "Group by type" (Device Manager's class names, groups in order of their worst device).
  Each row: name, flag chips, the problem in plain words with its code, provider, version,
  date, INF, maker and signature. Buttons: Update (when Windows Update offers a driver for
  this device) and "<Maker> page".
- The tab badge counts devices that need a look (red when one has a problem).

## Rules

Listing (read only, off the main thread):
- `SetupDiGetClassDevsW(DIGCF_PRESENT | DIGCF_ALLCLASSES)`, then per device
  `SetupDiGetDevicePropertyW` for FriendlyName / DeviceDesc, Class, Manufacturer,
  DriverProvider, DriverVersion, DriverDate (FILETIME), DriverInfPath, HardwareIds and
  CompatibleIds; class names from `SetupDiGetClassDescriptionW`; `CM_Get_DevNode_Status` for
  `DN_HAS_PROBLEM` and the problem code.
- Devices with `DN_NO_SHOW_IN_DM` and no problem are left out and counted ("37 hidden
  system devices"), as Device Manager does. A hidden device with a problem is shown.
- Unnamed devices show as "Unknown device" with their instance id.

Flags (worst first; a device's status is its worst flag):
- **problem**: any problem code except 22 (turned off) and 45 (not connected), which show
  as **off** and are not counted as problems. Codes 1, 10, 12, 14, 18, 19, 21, 22, 24, 28,
  31, 39, 43, 45, 52 have their own sentence; others say "This device reports a problem
  (code N)."
- **unsigned**: `SetupVerifyInfFileW` on `%SystemRoot%\INF\<inf>` failed for a reason other
  than "file not found". Signed kinds from the signer score: Came with Windows (inbox), WHQL
  (also logo standard / premium), Authenticode (also "trusted publisher"), Signed. Results
  are cached per INF name for the life of the process (a new driver gets a new oemNN.inf).
- **old**: driver date 5+ years ago (365.25-day years), only for third-party drivers
  (oemNN.inf). Drivers that came with Windows are serviced by Windows and many carry 2006
  dates, so age alone never flags them.

Maker pages (`MAKERS` in `drivers.rs`, https only, hard-coded):
NVIDIA, AMD, Intel, Realtek, Qualcomm, MediaTek, Broadcom, Logitech; PC makers Dell, HP,
Lenovo, ASUS, Acer, MSI. A device matches by its driver provider, else its manufacturer, on
whole words ("Intel(R) Corporation" is Intel, "Intelligent" is not). The PC maker comes from
`HKLM\HARDWARE\DESCRIPTION\System\BIOS` SystemManufacturer, or BaseBoardManufacturer when
the system name is a placeholder ("System manufacturer", "To Be Filled By O.E.M."); only the
six PC makers count. Buttons call the existing `open_url` (https only).

Windows Update drivers:
- Search: WUA `IUpdateSearcher.Search("IsInstalled=0 and IsHidden=0 and Type='Driver'")` on
  its own MTA thread (`orbit-driver-updates`), first from Windows' cache (offline), then
  cached 30 minutes; "Check again" searches online. Health's Windows Update item stays
  `Type='Software'`. Per offer: `IWindowsDriverUpdate` class, provider, manufacturer, model,
  hardware id and date; `IUpdate3.BrowseOnly` for "Optional". An offer is linked to the
  present device whose hardware or compatible ids contain its hardware id (ignoring case).
- Install (`install_driver_update(id)`): the UI asks first. Not elevated, it offers
  "Restart as administrator" instead (the existing pattern). Rust checks, before touching
  Windows: the id is a GUID (it goes into a search string), the id is one of the offers of
  the last search, Orbit is elevated, and no other install runs. Then one update: search
  `UpdateID='<id>' and IsInstalled=0 and Type='Driver'` (offline), refuse when it has a
  license not yet accepted (Orbit never accepts one for the user), `UpdateCollection` with
  that one update, `IUpdateDownloader.Download`, `IUpdateInstaller.Install`. The result says
  installed / installed with errors / failed (with the HRESULT) / cancelled, and "Restart
  the PC to finish." when Windows wants a restart. Orbit never restarts the PC.
- Never: drivers from any other source, silent or background installs, auto-update.

Health: a **Devices** item ("No device reports a problem." / "Webcam reports a problem." /
"3 devices report a problem.", detail with the first five reasons, button "Go to Drivers").
It reuses the SetupAPI scan without the signature checks.

## How to test

- `cargo test drivers::` (14 tests): problem texts, maker matching on whole words, PC maker
  from SMBIOS (placeholders, board fallback), the age rule (inbox never old), signature
  classification, flags / status / sort / counts, offer-to-device matching, UpdateID
  validation (rejects injection), the Health item texts, FILETIME conversion, a live read-only
  device scan, and the install path through a fake `UpdateInstaller` only: refusals happen
  before Windows is called, every result code is worded, the busy flag is released.
- `cargo test drivers::tests::live -- --ignored --nocapture` prints this PC's devices,
  flagged ones and an offline Windows Update driver search. It never installs.
- `$env:ORBIT_BROWSER="msedge"; npm run test:ui` opens the tab in every theme.
- Mock (serve `ui/`): 20 devices with every flag; the search takes 2.5 s (5 s online) and
  offers two drivers. Install as admin asks to restart as administrator (the mock flips to
  admin), then Install confirms and pretends for 4 s. Nothing is installed anywhere.

## Results on the dev PC

Live cargo probe (debug build): 275 present devices, 238 shown, 37 hidden; device scan about
2.4 s, signature checks of 106 INFs about 4 s the first time (cached after). Flagged: 6
problems (5 `ROOT\NET\000N` devices and a "Synthetic Gamepad" without a driver, code 28), 0
unsigned, 19 old (Razer 2017 HID drivers, SuperDisplay, DroidCam, AMD Special Tools, ViGEm
bus). System: Gigabyte B760M H DDR4, which is not in the PC maker table, so no system page.
Offline driver search about 15 s: one offer (Samsung Android USB driver, 2017), not matched
to a present device.

Release exe, run live (unelevated, own WebView2 / history / shell / boost folders, driven
over the DevTools port, nothing clicked but the tabs): list on screen after 2.4 s, offline
driver search done after 8.3 s. Same counts: 238 devices, 6 problems, 19 old, 0 unsigned
(160 came with Windows, 71 WHQL, 7 without a driver). Maker pages offered for Intel (48
devices), Realtek 5, NVIDIA 4, Logitech 4, AMD 1, Lenovo 1. The Samsung offer showed with
"Install as admin". Health showed "Devices: 6 devices report a problem." with Go to Drivers.

## Not verified

- Installing a driver: by design never run for real. The install path is unit-tested with
  a fake and exercised in the mock only; the real `WuaInstaller` compiles but was never
  called.
- "Check again" (online search) and the maker pages were not clicked in the live app.
- An elevated run.
- A device with code 52 or an unsigned driver on a real PC (unit tests only).
