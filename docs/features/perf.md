# Feature: perf (v0.4 Performance view polish)

The Performance view (02) now uses the 0.4 snapshot fields: a tile per physical disk, a tile
per network interface, GPU engines next to the GPU chart, and a Battery tile on PCs with a
battery. No contract shapes changed.

Files: `ui/js/views/performance.js`, `ui/css/views/performance.css`.

## What it does

| tile | when | tile shows | detail pane |
|---|---|---|---|
| CPU, Memory | always | as before | as before (uptime and facts unchanged) |
| Disk N (letters) | one per `Snapshot.disks` entry ("0 C: E:" -> "Disk 0" + "C: E:") | active %, R/W, active-time sparkline (R+W sparkline when `activePct` is null) | the selected disk: title "Disk 0 (C: E:)", active-time chart, read/write chart, capacity bars for the volumes on that disk (matched by drive letter; "No volumes with a drive letter on this disk." otherwise), facts. Top I/O stays system-wide ("By I/O rate, all disks"). |
| Disk | `disks` is empty | the old system-wide tile | as before (every volume) |
| one per interface | interfaces with traffic since boot (`totalRx + totalTx > 0`), sorted by name | R+S rate, R/S, its own sparkline | the selected interface: title, receive/send chart, facts for that interface; its row is highlighted in the interface table |
| Network "All" | 0 or 2+ active interfaces | total, as before | total, as before |
| GPU | once GPU counters were seen | as before | main chart plus an "Engines / N" list (`Snapshot.gpuEngines`, backend order): name, small bar, percent. Beside the chart when the panel is 720 px or wider, a grid below it otherwise. New fact "Busiest engine". "No per-engine counters reported." when the list is empty. |
| Battery | `Snapshot.battery` not null | percent; "2h 31m left" on battery, else Charging / Plugged in / Fully charged | charge chart over 2 hours (one point a minute, the newest follows the live value; a note until the second point), facts: Charge, Status, Time left, Power source |

- History for each disk (active, read, write), interface (rx, tx) and battery is kept inside
  `performance.js` and recorded on every snapshot, visible or not (a few array pushes). Drawing
  still happens only while the view is visible and the window is not hidden.
- The tile list is rebuilt only when the set of tiles changes (a disk or interface appears or
  goes, the battery appears or goes, GPU counters first seen). Keyboard focus stays on the same
  tile; arrow keys, Home and End move through all tiles.
- The selection is remembered by key (`orbit.perf.resource`: `cpu`, `disk:0 C: E:`,
  `net:Ethernet`, `battery`, ...). If that tile is missing, the first tile of the same kind is
  shown (or CPU), and the remembered choice comes back when the tile does.
- Layout: the 960 px compact grid (`auto-fit, minmax(120px, 1fr)`) wraps the extra tiles into
  more rows, no horizontal scroll. Tile names ellipsize with the full name in the tooltip.
  With more than six tiles the wide tile column is no longer sticky, so its last tiles stay
  reachable.
- Battery colour is `--ok`; everything else reuses the existing chart tokens.
- Copy fix: the Network pane's top-consumers note now points to the Network view (per-app
  rates there when elevated) instead of saying the data is not collected yet.

## How to test

- Mock: serve `ui/`, open Performance. Two disks, Network All + Ethernet + vEthernet (WSL),
  GPU with 5 engines, Battery. `?nobattery` removes the battery tile, `?nogpu` the GPU tile and
  engines. Try it at 960 px wide and in all four themes.
- Live: debug exe with CDP; open Performance and click each tile.

## Verified

- `node --check` on every `ui/js` file; `cargo build` (debug, no warnings) and `cargo test`
  (96 passed, 4 ignored).
- Mock (built-in browser): all tiles and panes at 1440x900 and 960x700, no horizontal scroll,
  per-disk volume filtering, interface highlight, keyboard arrows/Home, the selection fallback
  with `?nobattery&nogpu` (remembered "battery" shows CPU), Night, Void, Signal and Daylight,
  0 console errors.
- Live (debug exe, CDP 9409, unelevated): this PC reports 4 physical disks, 1 interface
  (so no total tile), 8 GPU engines and no battery. All tiles and panes rendered with real
  values, 0 console errors. The instance was stopped by its PID afterwards.

## Not verified

- A real battery (the dev PC is a desktop): only the mock battery was seen.
- Two or more live interfaces (the total tile live) and hot-plugging a disk or adapter while
  the view is open.
- Text contrast was not re-measured: new text uses existing tokens and classes (`--ink`,
  `--ink-2`, `--muted`) on the same backgrounds as the existing tiles and facts.

## Noticed, not changed

- Live `gpu.dedicatedUsed` reads 23.0 GB on this PC, which looks like more than dedicated
  VRAM (data feature, `pdh.rs`/`sampler.rs`).
- `charts.js` keeps observing sparkline canvases after a tile rebuild removes them (a few
  detached canvases per rebuild, rare). An `unobserve` export in `charts.js` would fix it.
