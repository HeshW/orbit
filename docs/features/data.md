# Feature: data (v0.4 snapshot fields)

Fills the new `Snapshot` / `ProcessRow` fields from `docs/V04-CONTRACT.md` with real
values in the sampler. No shapes or names changed.

## What it does

| field | source | notes |
|---|---|---|
| `ProcessRow.sessionId` | `ProcessIdToSessionId`, falling back to one `WTSEnumerateProcessesW` list | Read once per process and cached per (pid, start time) with a name guard, like `exe_cache`. Unelevated, `ProcessIdToSessionId` refuses about 190 SYSTEM processes; the WTS list covers them without opening anything, and it runs at most once per tick, only when a new process was refused. Idle (0) and System (4) are always 0. Live: every row filled (546/546). |
| `Snapshot.disks` | PDH `\PhysicalDisk(*)\% Idle Time`, `Disk Read Bytes/sec`, `Disk Write Bytes/sec` | Wildcard counters in the existing system query, joined by instance name, `_Total` dropped, sorted by name ("0 F:", "1 D: E:", "2 C:"). `activePct` = 100 - idle, clamped 0..100, 2 decimals; null if that disk has no idle value. Byte rates are whole numbers. |
| `Snapshot.gpuEngines` | the existing `\GPU Engine(*)\Utilization Percentage` wildcard | Per single engine (adapter + engine index): summed over pids, as the GPU total already was. Then per engine type: the busiest engine of that type (two Copy engines do not add up), clamped 0..100. Friendly names: `VideoDecode` -> "Video Decode", `Compute_0` -> "Compute 0", `LegacyOverlay` -> "Legacy Overlay", `OFA_0` -> "OFA 0"; "3D", "VR" unchanged. Order: 3D, Copy, Video Encode, Video Decode, Video Processing, Compute *, then by name. Lists every engine type some process has opened, idle ones included (this PC: 8). Empty when GPU counters are missing. |
| `Snapshot.battery` | `GetSystemPowerStatus` | null when BatteryFlag is 128 (no battery) or 255 (unknown), or the percent is unknown. `charging` = flag bit 8, `acOnline` = ACLineStatus 1, `secondsLeft` null while plugged in or when Windows gives no estimate. |
| `netRxBps` / `netTxBps`, `network.perApp` | `network::per_app_rates()` / `network::per_app_status()` | Already wired by the scaffold. Kept as-is, no change needed from the network feature. |

`history::record(&snapshot, interval)` stays where the scaffold put it: after `sample()`
returns the finished snapshot, before the emit. It takes a reference, so this feature
adds no cost to it.

Files: `src-tauri/src/pdh.rs`, `src-tauri/src/sampler.rs`, `src-tauri/src/winenum.rs`,
`ui/js/mock/data.js`.

## Tick cost

Measured with the harness `cargo test --lib tick_cost -- --ignored --nocapture
--test-threads=1` (debug build, about 530 processes, 10 ticks at 1 s):

| | avg | max |
|---|---|---|
| before (scaffold) | 40.5 ms | 50.4 ms |
| after | 40.5 ms | 45.6 ms |

The difference is within noise. The first tick is 91.5 ms: the one-off session lookup
for every process plus one WTS list. The battery call runs every tick and costs
microseconds.

## Mock (`ui/js/mock/data.js`)

Matches the real shapes: two-decimal `activePct` and engine usage, whole-number byte
rates, engines in the real order (3D, Copy, Video Encode, Video Decode, Security, idle
ones at 0). The signed-in session's `csrss.exe` and `winlogon.exe` (SYSTEM) are session
1, as on a real PC. New URL flag `?nobattery` makes `battery` null (most desktops,
including the dev PC); `?nogpu` already empties `gpuEngines`.

## How to test

- `cargo test` in `src-tauri`: new unit tests `names_engine_types`,
  `totals_engine_types_by_busiest_engine`, `merges_physical_disks` (pdh),
  `reads_battery_flags`, `reads_own_session` (winenum).
- Live: start the debug exe with CDP on, then in the page
  `await __TAURI__.core.invoke('get_snapshot')`, and check `disks`, `gpuEngines`, `battery`
  and that every `processes[].sessionId` is a number. Delta events carry the same fields
  (`rows[].sessionId` on every row).
- Mock: serve `ui/`, open `/`, `/?nobattery`, `/?nogpu`.

## Not verified

- Battery on a laptop: the dev PC is a desktop (battery null). The flag mapping is unit
  tested only.
- Elevated run (ProcessIdToSessionId should then succeed for almost everything; the WTS
  fallback stays as a safety net).
- Hot-plugging a disk while Orbit runs. PDH wildcard counters normally pick up new
  instances at the next collect; a new disk's first value is skipped (no baseline yet).
- Per-app network values: the network feature isn't built yet, so the rows stay null and
  `perApp.reason` reads "Per-app network is not built yet.".
