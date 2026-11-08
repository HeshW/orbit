# Orbit — a LegacyTech task manager

> Every process. *In orbit.*

Orbit is a Windows task manager written in Rust (Tauri 2), styled with the LegacyTech
design system: the night-sky console, glass panels, Oxanium + Manrope, the pixel-L mark
and the cyan signal pixel. It does what Task Manager does, and makes finding and acting on
things faster.

## Why not just Task Manager

| Task Manager | Orbit |
|---|---|
| No real search on most tabs | One search bar on every tab, plus a command palette (Ctrl+K) |
| Fixed look | 4 themes built from LegacyTech tokens |
| Process info scattered over tabs | One details drawer: path, command line, user, parent, children, live history |
| Hard to spot what changed | Heat-tinted CPU/memory/I/O cells, sparkline history per process |
| Startup tab can't search, services tab is bare | Search, filters and one-click toggles on both |
| No export | Export the process list to CSV |

## Sections

**01 / Processes**
- Grouped like Task Manager (Apps / Background / Windows) or a parent-child tree, or a flat list.
- Columns: name, PID, CPU, memory, I/O, GPU, user, status. Click to sort.
- Search matches name, PID, window title, path, command line and user. Filter chips:
  High CPU, High memory, Not responding, Mine only.
- Actions: End task, End process tree, Restart (same command line and folder; Explorer
  restarts like Task Manager's and always gets its taskbar back), Suspend / Resume, Set
  priority, Set affinity (pick CPUs), Open file location, Search online, Copy details,
  Pin to watchlist.
- Run new task (Ctrl+N): a program, folder, document or website, optionally as
  administrator, with the last 10 commands remembered.
- Details drawer with the last 60 samples of CPU/memory history for that process
  (60 seconds at the default 1 s update speed).

**02 / Performance**
- Live CPU (total + per-core grid), Memory, Disk, Network and GPU graphs, 60 s history.
- System facts: uptime, processes, threads, handles, committed memory, CPU model,
  core counts.

**03 / Startup**
- Every startup entry (HKCU/HKLM Run keys and both Startup folders), on/off toggle
  using the same switch Windows itself uses (StartupApproved), search.
- Startup impact (High / Medium / Low) with Task Manager's thresholds. Windows keeps its
  own numbers private, so "Measure at next sign-in" has Orbit measure them once (CPU time
  and disk reads in each app's first minute), with no window and nothing left behind.

**04 / Services**
- All services with status, start type and PID. Start / Stop / Restart. Search and
  "running only" filter. Jump from a service to its process.

**05 / Settings**
- Themes: Night (default, the design system as-is), Void (pure black for OLED),
  Signal (high contrast), Daylight (light, built from the logo kit's light colours).
- Update speed (0.5 s / 1 s / 2 s / 5 s), pause, compact rows, reduced motion, always on
  top (also the pin in the title bar).
- Admin badge and "Restart as administrator" (some actions need it).

**Everywhere**
- Ctrl+K command palette: jump to any process or action by typing ("end chrome",
  "theme void", "performance").
- Shortcuts: `/` search, `Del` end task, `Ctrl+N` run new task, `1–5` switch section,
  `Space` pause updates, `Esc` close.
- Watchlist: pin processes to the top; Orbit warns (in-app toast) when a pinned
  process passes 50% CPU for 10 seconds.

## Design system use

- Colours, type, radii, shadows: the LegacyTech tokens exactly for Night. The other
  three themes reuse the same token names, so every screen works in all four.
  Daylight is an extension: the website is dark-only, so it uses the logo kit's
  light colours (`logo-light-bg`, `logo-blue-light`, `logo-cyan-light`).
- Alternating corners (`4px 18px` buttons, `22px 6px` passes, `12px 4px` tiles),
  1px `edge` highlights, `glow` on hover, the cyan `signal` pixel for "live".
- Micro labels with slash grammar: "01 / Processes", "Live / 1.0 s".
- 24px stroke icons, no emoji. Focus is a 2px cyan outline with 3px offset.
- Fonts (Oxanium, Manrope, OFL) are bundled so the app works offline.

## Limits (honest)

- Per-process **network** usage needs Windows event tracing (ETW); not in v1. Total
  network is shown.
- Some system processes are protected: even as admin, Windows won't let anyone end or
  inspect them. Orbit shows "Access denied" instead of failing silently.
- Memory per process is the working set (Task Manager shows private working set, so
  numbers can differ slightly).
- GPU numbers come from Windows performance counters, same source as Task Manager.
- The per-process **I/O** column counts all reads and writes (files, pipes, network),
  not just physical disk, so it is labelled I/O. The Disk tile is real disk activity.
- Without admin, Windows hides the owner of some SYSTEM processes, so their user shows
  as blank.

## Tech

- Rust backend: `sysinfo` (processes, CPU, memory, network, disks), `windows` crate
  (end task, priority, suspend, windows/"not responding", services, startup registry,
  icons, admin), Windows PDH counters (GPU, disk activity, threads, handles).
- UI: plain HTML/CSS/JS modules inside Tauri's WebView2. No Node build step.
- A sampler thread pushes one snapshot per tick to the UI.
