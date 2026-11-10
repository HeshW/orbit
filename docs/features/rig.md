# Feature: My Rig (v0.6, 8c)

A one-page spec sheet for the PC, with a **Copy specs** button for forums and support chats.
Read only, no admin, nothing leaves the PC. Shapes and the command are in `docs/CONTRACT.md`
("v0.6").

Files: `src-tauri/src/rig.rs`, `ui/js/views/rig.js`, `ui/css/views/rig.css`,
`ui/js/mock/rig.js`. Shared files touched: `lib.rs` (module + `get_rig_specs`), `Cargo.toml`
(`Win32_Devices_Display`), `health.rs` (WMI helpers and `read_physical_disks` made
`pub(crate)`), `drivers.rs` (`is_placeholder` made `pub(crate)`), `index.html` (nav item,
section, `i-rig` symbol, CSS link), `app.js` (view list, number keys, Ctrl+,), `palette.js`,
`views/settings.js` (shortcut list, "11 / Settings"), `mock/index.js`, `tests/ui-smoke.mjs`.

## Navigation

- Sidebar: My Rig sits after Optimize as **10** (key **0**). Settings moves to **11** and has
  no number key: the sidebar, the palette ("Go to" > Settings) and **Ctrl+,** open it.
  `KEY_VIEWS` is `VIEW_NAMES` without `settings`.
- Palette: "My Rig" (Go to, hint 10) and "Copy specs" (Actions; opens My Rig first if the
  specs are not loaded yet).

## The page

- Head: "10 / My Rig", "Your PC. On one page." and four readouts: Cores / threads, Memory,
  VRAM (largest card), Storage (all drives, decimal).
- Toolbar: "Read from Windows on this PC. Nothing is sent anywhere." and **Copy specs**.
- Cards (panel style, micro labels, tabular numbers):
  - **Processor**: name without (R)/(TM) and the "CPU @ x GHz" tail; Cores, Threads, Base clock.
  - **Memory**: installed total, modules ("2 x 16 GB"); Type, Speed (MT/s), Slots used
    ("2 of 2").
  - **Graphics**: one row per adapter (same list as Performance > GPU): name, VRAM, driver.
  - **Motherboard**: maker and model ("In <PC model>" under it for brand PCs); BIOS, BIOS
    date, BIOS maker.
  - **Storage**: one row per physical disk: name, size as on the box (decimal), bus and
    SSD / HDD chips.
  - **Displays**: one row per active display: EDID name ("Built-in display" for a laptop panel
    without one), resolution, refresh rate, "Main" when there are several.
  - **Windows**: edition; Version, Build.
- Loading: "Reading your hardware." Error: the message and Try again.

Copy specs text (one plain line per part, only lines that have data):

```
PC: Legion Pro 5 16IRX8
CPU: 13th Gen Intel Core i7-13700H (14 cores, 20 threads, 2.40 GHz base)
Memory: 32 GB DDR5 5200 MT/s (2 x 16 GB, 2 of 2 slots)
GPU: NVIDIA GeForce RTX 4070 Laptop GPU (8 GB, driver 32.0.15.6094)
Motherboard: LENOVO LNVNB161216 (BIOS LPCN49WW, 03/14/2025)
Drive: SAMSUNG MZVL21T0HCLR-00BL2, 1 TB NVMe SSD
Display: 2560 x 1600 at 240 Hz
Windows: Windows 11 Home 24H2 (build 26100.2314)
```

## Sources (`rig.rs`)

| part | source |
|---|---|
| CPU | WMI `Win32_Processor`: Name, NumberOfCores and NumberOfLogicalProcessors (summed over sockets), MaxClockSpeed (the rated base clock). |
| Memory | WMI `Win32_PhysicalMemory`: Capacity per module, ConfiguredClockSpeed (else Speed; the slowest module), SMBIOSMemoryType (20 DDR ... 26 DDR4, 34 DDR5, 35 LPDDR5). `Win32_PhysicalMemoryArray` MemoryDevices (Use = 3) for the slots on the board. |
| Graphics | `gpu::adapters` + `gpu::visible` (see `docs/features/gpu.md`). |
| Motherboard, BIOS | Registry `HKLM\HARDWARE\DESCRIPTION\System\BIOS` (Windows' copy of SMBIOS): BaseBoardManufacturer / Product, BIOSVendor / Version / ReleaseDate, SystemManufacturer / ProductName (kept only when they differ from the board). Placeholders ("Default string", "To be filled by O.E.M.") are dropped. |
| Drives | `health::read_physical_disks` (Storage WMI `MSFT_PhysicalDisk`: FriendlyName, MediaType, BusType, Size). |
| Displays | `QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS)`: source mode size, target refresh rational, `DisplayConfigGetDeviceInfo(GET_TARGET_NAME)` for the EDID name; primary = source at (0, 0); internal = INTERNAL / DisplayPort or UDI embedded. |
| Windows | Registry `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion`: ProductName (builds 22000+ say "Windows 10" there: renamed to Windows 11), DisplayVersion (else ReleaseId), CurrentBuild + UBR. |

Everything except the GPU list is read once (a `Mutex<Option<RigSpecs>>`, so a second caller
waits for the first read) on a blocking thread; the view asks once and keeps the answer.

## How to test

- `cargo test rig::` (memory types, Windows 11 name, refresh rates, and a read of this PC
  printed with `--nocapture`).
- Browser preview: My Rig shows the mock laptop (two GPUs, two drives, two displays);
  `?rigfail` shows the error state. `tests/ui-smoke.mjs` visits it in every theme and checks
  the cards, Copy specs, key 0 and Ctrl+,.
