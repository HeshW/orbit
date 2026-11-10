# Feature: CPU and GPU temperatures (v0.7, 9b), Performance

The CPU and GPU tiles show the temperature next to the load, and the CPU and GPU panes have a
Sensors block: the temperature now, a 60-second graph with the peak, and whatever else the
source reports (per-core temperatures and clocks, package power, fan, power draw and limit,
GPU clocks). Every block names its source. Shapes are in `docs/CONTRACT.md` ("v0.7").

Files: `src-tauri/src/sensors.rs`, `ui/js/temps.js`, `ui/js/mock/sensors.js`. Shared files
touched: `lib.rs` (module), `Cargo.toml` (`Wdk_Graphics_Direct3D`), `model.rs`
(`Snapshot.sensors`, `SnapshotDelta.sensors`), `sampler.rs` (owns the `SensorHub`),
`health.rs` (`Wmi::query_timeout`), `history.rs` (test helper), `views/performance.js`,
`views/settings.js` (Temperature unit), `store.js` (`tempUnit`), `motion.js` (`enterBlock`),
`css/views/performance.css`, `mock/data.js`, `tests/ui-smoke.mjs`.

## Where the numbers come from

| Part | Source | Admin | Notes |
|---|---|---|---|
| NVIDIA GPU | NVML (`nvml.dll` from System32, else `Program Files\NVIDIA Corporation\NVSMI`) | No | Loaded at run time with `LoadLibraryExW`; no link-time dependency. Missing DLL or a failed `nvmlInit_v2` = no GPU sensors, no error. |
| AMD / Intel GPU | none yet | | "Temperature not available on this GPU" on the adapter line. ADL / ADLX and the Intel library could not be tested on the dev PC (NVIDIA), so they are not shipped. |
| CPU | LibreHardwareMonitor or OpenHardwareMonitor WMI (`root\LibreHardwareMonitor`, `root\OpenHardwareMonitor`, class `Sensor`) | No | Only while the tool runs. |
| CPU | HWiNFO shared memory `Global\HWiNFO_SENS_SM2` | No | Only with HWiNFO running and **Shared Memory Support** on (HWiNFO settings; the free version turns it off after 12 hours). |
| CPU | ACPI thermal zone (`root\WMI` `MSAcpi_ThermalZoneTemperature`) | Yes | Only when Orbit is elevated and no tool above runs; shown as "Approximate (motherboard zone)" and only once a zone's value has changed (many boards report a constant). |

Never: a kernel driver. Orbit does not install, bundle or load WinRing0, PawnIO or any
other driver. A future route is PawnIO (signed, used by newer LibreHardwareMonitor) when the
user has installed it themselves; not used now.

## Rules

- Read on the `orbit-sensors` thread, at most once a second, only while the sampler ticks
  (`SensorHub::poll` each tick asks for a reading and returns the latest without waiting). When a
  read has been running for over 5 s (a hung source) the last reading is dropped; a slow update
  interval or a pause never hides one. With updates paused there are no reads. On exit
  (`exit_cleanup`) the thread is stopped and shuts NVML down, waiting at most 1 s.
- Cost on the dev PC: first read (NVML init, DXGI match, probing the tools) about 25 ms, then
  0.5 to 1 ms a read.
- Nothing is guessed. A value a source does not give is `null` and its row is hidden.
  Temperatures outside 0 to 150 °C, NaN and NVML error codes count as missing.
- NVML device to DXGI adapter: the adapter's PCI bus and device number come from D3DKMT
  (`D3DKMTOpenAdapterFromLuid` + `KMTQAITYPE_ADAPTERADDRESS`) and must equal NVML's
  `nvmlDeviceGetPciInfo` bus and device. Without an address, a name only one NVML device has.
  Two identical cards without addresses stay unmatched rather than risk swapped numbers. The
  UI also matches by name when the LUID is a hidden twin (gpu.rs `visible`).
- NVML calls: `nvmlDeviceGetTemperature` (GPU sensor), `GetTemperatureThreshold` (slowdown,
  read once), `GetFanSpeed`, `GetPowerUsage` and `GetEnforcedPowerLimit` (mW), `GetClockInfo`
  (graphics and memory), `GetFieldValues` field 82 (memory temperature). NVML does not publish
  the hot spot temperature, so Orbit does not show one.
- CPU tool order: LibreHardwareMonitor, OpenHardwareMonitor, HWiNFO, ACPI. Absent tools are
  looked for every 15 s; a tool that closes is dropped on the next read.
- Tool readings: the first CPU only (`/intelcpu/0`, `/amdcpu/0`; HWiNFO `CPU [#0]` sensors).
  Package = "CPU Package", else Tctl/Tdie, "Package", "CPU Die (average)", "Core Max", else
  the hottest core. Cores = "Core #n" / "CPU Core #n" / "P-core n" / "E-core n"; AMD chips
  without per-core temperatures show "CCD n". Power = the package power. Clocks per core when
  the tool lists them; otherwise the block shows Windows' CPU speed (`cpu.freqMhz`).
  HWiNFO in °F is converted back to °C.
- HWiNFO layout (SDK, packed): header 44 bytes (`HWiS` signature, version, revision,
  poll time, then offset / size / count for sensors and readings); sensor element 264 bytes
  (two ids, two 128-byte names); reading element 316 bytes (type, sensor index, id, two
  128-byte labels, 16-byte unit, value, min, max, avg). Strides use the sizes in the header,
  so newer versions with extra UTF-8 fields still parse. A poll time more than 60 s off, or
  "DEAD", means no reading.

## UI

- Thresholds in one place, `ui/js/temps.js` `TEMP_LIMITS`: CPU warm 80 °C, hot 90 °C; GPU
  warm 80 °C, hot 87 °C. Colours: normal `--ok`, warm `--warn`, hot `--danger`
  (`.temp--normal|warm|hot` in performance.css).
- Tiles: "34% 62°C". The GPU tile uses the first adapter's reading.
- CPU Sensors block: big package temperature, graph, Hottest core, Peak in window, Package
  power, Average clock (or Speed), then one cell per core with temperature and clock.
- GPU Sensors block: big GPU temperature, graph, Peak in window, Memory temp, Slows down at,
  Fan, Power ("41 W of 115 W", or Power limit alone), Core clock, Memory clock. Each adapter
  line in Adapters shows its own temperature.
- No CPU source: "Windows doesn't report CPU temperature on its own. Run HWiNFO (with Shared
  Memory Support on) or LibreHardwareMonitor and Orbit will show it." The names link to
  https://www.hwinfo.com/ and https://github.com/LibreHardwareMonitor/LibreHardwareMonitor
  (opened with `open_url`). Shown after 3 snapshots, since the first one has no reading yet.
- Settings > Display > Temperature: Celsius or Fahrenheit (`settings.tempUnit`).
- A Sensors block that appears after the pane was drawn fades in with `motion.js`
  `enterBlock` (the 0.6 `.enter-up` animation); off with Animations off or
  prefers-reduced-motion.
- Mini window: unchanged (the five rows have no room for a second value).

## Readings on the dev PC (RTX 4060, not elevated, no tools running)

`cargo test real_sensors -- --ignored --nocapture`:

- nvml.dll loaded from `C:\WINDOWS\system32`; RTX 4060 at PCI bus 1, device 0, matched to
  DXGI LUID `0x00000000_0x00016769` (the SuperDisplay twin `0x..2224d` reports bus 0 /
  device 65535 and is not matched).
- temp 37-38 °C, fan 43%, slowdown 95 °C, power limit 115 W, core 210 MHz and memory 405 MHz
  at idle (P8, same as `nvidia-smi`); power draw `null` (nvidia-smi also says N/A on this
  card); memory temperature `null` (not supported).
- CPU: `null` (no LibreHardwareMonitor / OpenHardwareMonitor namespace, no HWiNFO shared
  memory, not elevated).

## How to test

- `cargo test sensors::` (NVML values and struct sizes, LUID parsing, NVML-to-DXGI matching,
  core labels, LibreHardwareMonitor rows for a 6-core Intel, OpenHardwareMonitor and AMD rows,
  the HWiNFO layout from a byte fixture incl. °F and stale / dead / short buffers, ACPI tenths
  of a kelvin and the "moved" rule).
- `cargo test real_sensors -- --ignored --nocapture` prints what this PC returns.
- Browser preview: CPU from LibreHardwareMonitor with 6 cores, NVIDIA with every sensor, the
  Intel card without a library. `?nocputemp`: no CPU source (the hint). `?hot`: CPU and GPU
  past the hot thresholds. `?nogpu`: no GPU at all. `tests/ui-smoke.mjs` "<theme>:
  temperatures" and "temperatures".
