# Orbit roadmap — future features

## Status (after 0.5.0)

| # | Item | Status |
|---|------|--------|
| 1 | App history | Shipped in 0.4.0 (Orbit records its own). SRUM back-fill not done. |
| 2 | Users tab | Shipped in 0.4.0. |
| 3 | Per-app network (ETW) | Shipped in 0.4.0, admin only. The elevated path is not tested on a real PC yet. |
| 4 | Ctrl+Shift+Esc takeover | Dropped. Replaced by the Ctrl+Shift+O hotkey in 0.4.0. |
| 5 | Auto-start elevated | Not planned: Orbit installs to a user-writable folder, so a silent elevated task would be a privilege-escalation hole. |
| 6 | v1 gaps | Per-process network done (#3). The rest are Windows limits, unchanged. |
| 7 | **Optimize** | Shipped in 0.5.0: health report, startup suggestions, cleanup, boost, driver check. See section 7 and `docs/features/`. |
| 8 | **0.6 list** | Shipped in 0.6.0: GPU name, History right-click, My Rig, motion, Defender scan, guided debloat. See section 8 and `docs/features/`. |
| 10 | **0.8 list** | Planned: Start in tray fix, dismissable startup suggestions, Startup list that stays in sync. See section 10. |
| 9 | **0.7 list** | Shipped in 0.7.0: animated logo in the sidebar, CPU and GPU temperatures. See section 9 and `docs/features/`. |

Also still open: code signing (needs the owner's country, see `RELEASE-PLAN.md`), and the
real-PC checks 0.4.0 couldn't do here (per-app network as admin, Run as admin end to end,
a real Ctrl+Shift+O press, Start with Windows, backup import, Users actions, battery).

---

The sections below were written before 0.4.0. Sections 1-6 are kept for the reasoning;
section 7 is new.

Planning only. No code yet. This file describes six things the owner wants to add,
how each would fit Orbit's real architecture (see `CONTRACT.md`), what admin rights
each needs, the risks, and a rough size (S / M / L). A recommended order is at the end.

Technical claims are checked against Microsoft's docs where noted. Anything I could
not verify is marked **(unverified)**.

Quick orientation on the current build, so the plan lands in the right place:

- The backend is split into modules owned separately: **core** (`sampler.rs`, `pdh.rs`,
  `winenum.rs`, `model.rs`, `lib.rs`), **actions** (`actions.rs`, `icons.rs`),
  **system-lists** (`startup.rs`, `services.rs`). Every Tauri command is registered in
  `lib.rs` `generate_handler![...]`.
- One background thread in `sampler.rs` (`run_loop`) samples once per tick and emits
  `orbit://snapshot`. That is the only live data pump; a new per-app counter that has to
  update every second belongs here, not in a command.
- The UI is plain HTML/CSS/JS. A new tab = a new `nav-item`, a `section#view-*` in
  `index.html`, a `js/views/*.js` module with `mount()`, and a CSS file. Commands are
  called through `api.js` `call(cmd, args)`.
- Orbit already carries the pieces several of these features need: `isAdmin` in
  `StaticInfo`, a working **Restart as administrator** (`actions::restart_as_admin`,
  ShellExecute "runas"), the `Win32_System_RemoteDesktop` feature is already in
  `Cargo.toml`, and the registry read/write plumbing lives in `startup.rs`.

A general note the owner already knows: Orbit ships unelevated by default. Four of these
six features need admin for at least part of what they do. The pattern to reuse is the
one Startup and Services already use — show the data we can read unelevated, and put a
clear "Try Restart as administrator" on the actions that need it, rather than demanding
elevation up front.

---

## 1. App history (30-day per-app usage)

**What it does for the user.** A tab like Task Manager's "App history": per app, totals
over ~30 days for CPU time, network data, metered network, and tile updates. Good for
"what's been eating my data cap this month".

**Where Windows keeps this.** Task Manager reads **SRUM** (System Resource Usage
Monitor), an ESE/"Jet Blue" database at `C:\Windows\System32\sru\SRUDB.dat`. Confirmed:
the file path and ESE format, that Task Manager's App history and the battery-usage
screens are generated from it, and that it holds separate tables for application resource
usage and for per-app network bytes sent/received (verified via multiple forensic write-ups;
the exact table GUIDs are documented but I'd confirm them against a live system — **(unverified)** at the GUID level).

The catch: the file is **held open by the DPS service** (Diagnostic Policy Service) while
Windows runs, and reading the ESE database needs the ESE API plus admin. Three ways in:

1. **Read SRUM directly (ESE API).** Open the live DB read-only, or make a volume-shadow
   copy / plain copy first and read that. This gives true 30-day history on first run
   with no waiting. But: needs admin; needs an ESE reader (either the `windows`/ESENT
   bindings or a crate); SRUM only flushes to disk periodically (roughly hourly
   **(unverified)**), so recent activity may be missing; and the table schema is
   undocumented and has shifted between Windows builds, so parsing is fragile. This is
   also exactly the access pattern security tools watch for, so antivirus may look twice.
2. **Orbit collects its own history.** The sampler already has per-process CPU and
   per-interface network every tick. Roll those up per exe name into a small local
   store (e.g. a JSON or SQLite file in Orbit's app-data folder), aggregated by day,
   pruned at 30 days. No admin, no fragile parsing, no AV concern, and it matches what
   Orbit actually measured. Downside: history only starts the day the user installs
   Orbit and only covers time Orbit was running. CPU *time* and network bytes work well
   this way; **metered network** and **tile updates** are not things Orbit samples today
   and would need extra work (metered state via the WinRT/connection-cost APIs; tile
   updates are not practically observable outside SRUM).
3. **Hybrid.** Orbit collects its own from day one, and *optionally*, when run as admin,
   back-fills the first view from SRUM so the user isn't staring at an empty table.

**Recommended:** start with **option 2** (Orbit's own history). It is the honest,
low-risk version and fits the sampler we already have. Treat SRUM back-fill as a later
"nice to have" behind the admin badge, and don't promise metered/tile columns in the
first cut — label the columns we can actually fill (CPU time, network, metered if we add
the cost API).

**Module / UI.** New `history.rs` in core (owns the rollup store + a `get_app_history`
command), a hook in `sampler.rs::run_loop` to feed it each tick, a new `06 / History`
tab (`views/history.js`). No new Cargo features for option 2; option 1/3 later would add
ESENT.

**Admin:** none for option 2; **admin required** for any SRUM reading.
**Risks:** empty-until-populated expectation (set it in the UI copy); store growth (cap
it); SRUM parsing fragility and AV attention if we go that route.
**Effort:** **M** for option 2 (sampler rollup + store + tab). **L** if SRUM reading is included.

---

## 2. Users tab (sessions)

**What it does for the user.** Lists the logged-in users / sessions on this PC with their
session state (active, disconnected), and per-session CPU/memory totals. Actions:
**Disconnect**, **Sign out** (log off), **Send message**. This is Task Manager's Users tab.

**How it works.** The Remote Desktop Services (WTS) APIs, all in `wtsapi32`:

- `WTSEnumerateSessions` — list sessions (id, station name, state).
- `WTSQuerySessionInformation` — per session: user name, domain, client name, etc.
- `WTSDisconnectSession` — disconnect.
- `WTSLogoffSession` — sign out.
- `WTSSendMessageW` — pop a message box on that session's desktop (title, body, style,
  timeout; use `bWait = FALSE` for a fire-and-forget notice).

Per-session CPU/memory isn't a WTS call — Orbit already has every process with a user and
CPU/memory in the snapshot. Map each process to its session with
`ProcessIdToSessionId` and sum per session. That reuses data we already sample.

**Admin / permissions (verified against Microsoft docs).**

- Enumerating and querying sessions: fine for the current user; reading *other* users'
  details reliably wants elevation.
- `WTSLogoffSession` on **another user's** session needs the **Reset** permission;
  `WTSSendMessage` to another session needs the **Message** permission; disconnect needs
  **Disconnect**. In practice, acting on another user means running as admin. A user
  already has full rights over **their own** session (Microsoft: "After a user logs on to
  a session, the user is granted all other Remote Desktop Services permissions for his or
  her session").

So: without admin, Orbit can show the list and let you act on your *own* session; the
Disconnect/Sign-out/Message buttons on *other* users get the same "Try Restart as
administrator" treatment the rest of Orbit uses.

**Module / UI.** New `users.rs` (system-lists family) with `list_users`,
`disconnect_session`, `logoff_session`, `send_session_message`. A `07 / Users` tab.
`WTS_CURRENT_SERVER_HANDLE` means no need to open a server handle. `Win32_System_RemoteDesktop`
is **already** enabled in `Cargo.toml`, so no Cargo change.

**Risks:** destructive actions — sign-out/disconnect can lose the other user's unsaved
work (Microsoft warns of this explicitly). Reuse Orbit's `confirmDialog({ danger: true })`.
Don't offer to sign out session 0 / the current session in a way that locks the user out.
**Effort:** **M**. The WTS calls are straightforward; the care is in the confirmations and
the per-session rollup.

---

## 3. Per-app network usage (live, via ETW)

**What it does for the user.** Fills the gap the proposal already admits: a real
bytes/sec sent and received **per process**, not just the machine total. Would light up a
"Network" column in Processes and a "Top network" tile in Performance.

**How it works.** There is no cheap per-process network counter in Windows; you have to
trace it. Options:

- **ETW, `Microsoft-Windows-Kernel-Network` provider.** A real-time trace session that
  emits a send/receive event per TCP/UDP datagram, each carrying the PID and the byte
  size. Orbit aggregates those into per-PID rates between sampler ticks. This is how
  per-process network is actually done on Windows. Verified: the provider's events carry
  PID and size and are commonly used exactly this way (e.g. Velociraptor's KernelNetwork
  artifact); the precise event-ID → direction mapping should be taken from the provider
  manifest rather than memorised **(unverified at the event-ID level)**.
- **`GetPerTcpConnectionEStats` / `SetPerTcpConnectionEStats`.** Per-TCP-connection byte
  counters. Verified against Microsoft docs: enabling eStats **requires Administrators**
  (`SetPerTcpConnectionEStats` returns `ERROR_ACCESS_DENIED` otherwise) and the app must
  be manifested `requireAdministrator` or run elevated. It is TCP-only (no UDP), per
  connection (you still map connection→PID via the TCP table), and extended stats are off
  by default so you must enable them per connection. Workable but partial.
- **Rust ETW crates.** `ferrisetw` (a Rust port of KrabsETW; latest 1.2.0, depends on
  `windows` ^0.57) or the lower-level `krabsetw` route. `ferrisetw` on `windows` 0.57 vs
  Orbit's 0.58 is a near-miss — would need checking for compatibility, else hand-roll the
  consumer against the `windows` ETW bindings Orbit already pulls in.

**Admin (verified).** Starting an ETW trace session needs **administrator, or membership
of Performance Log Users, or a LocalSystem/LocalService/NetworkService service**
(`StartTrace` returns `ERROR_ACCESS_DENIED` otherwise). So an ordinary user running Orbit
normally needs **Restart as administrator** for this to work at all.

**Cost.** A real-time kernel-network trace is a steady stream of events — one per packet-ish
— which is real CPU and allocation overhead on a busy machine, and the opposite of the
light once-a-second sampler Orbit runs today. It must run on its **own** thread/consumer,
be **opt-in** (off by default), and stop cleanly. Microsoft is blunt that ETW sessions are
a limited system resource (max ~64 on a system; ~8 system loggers) and that a session left
running after the app exits **persists** — so Orbit must name its session descriptively and
tear it down on exit (reuse the `RunEvent::Exit` hook that already resumes suspended
processes).

**Module / UI.** New `etwnet.rs` in core: owns the trace session + a `Mutex<HashMap<pid,
counters>>` the sampler reads each tick to fill `netRxBps`/`netTxBps` on `ProcessRow` /
`DynRow` (contract change — note it in the report). A setting "Per-app network (needs
admin)" gates it. Cargo adds the ETW features (`Win32_System_Diagnostics_Etw`) and maybe
a crate.

**Risks:** admin requirement; measurable overhead; orphaned trace sessions if teardown is
sloppy; AV/EDR sometimes flags processes that spin up kernel traces; event schema drift
between Windows builds.
**Effort:** **L**. This is the biggest and riskiest of the six.

---

## 4. Ctrl+Shift+Esc takeover (opt-out-able)

**What it does for the user.** Makes Ctrl+Shift+Esc (and "Task Manager" from the taskbar)
open **Orbit** instead of the built-in Task Manager — the trick Process Explorer's
"Replace Task Manager" uses. One toggle in Settings, reversible.

**How it works.** Windows has an **Image File Execution Options (IFEO)** "Debugger"
hook: set a `Debugger` string value under
`HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\taskmgr.exe`
pointing at Orbit's exe. Whenever something launches `taskmgr.exe`, Windows launches the
"debugger" (Orbit) with the taskmgr path appended instead. To undo, delete that value /
key. Verified: this is the documented mechanism Process Explorer uses, it lives under HKLM
(so **admin required** to write), and Windows provides no *official* permanent setting —
a feature update can reset it.

**Safe toggle design.**
- Settings toggle "Open Orbit with Ctrl+Shift+Esc". Off by default.
- Turning on: needs admin (writing HKLM). If not elevated, prompt Restart as admin first.
  Write the `Debugger` value = quoted full path to `orbit.exe`.
- Orbit's startup must **ignore the appended `taskmgr.exe` argument** so it doesn't try to
  treat it as a file — just launch normally. Without this the takeover breaks.
- Turning off: delete the value. Also offer this on first run after an update if we detect
  our value is gone, and never leave a dangling hook pointing at a moved/uninstalled exe.
- An **uninstall / "remove takeover"** path is mandatory — a stale Debugger value pointing
  at a deleted Orbit would break Task Manager entirely for the user.

**Admin:** yes (HKLM write). Reuse `startup.rs` registry helpers + `restart_as_admin`.
**Risks:** **antivirus / security tools flag IFEO Debugger hijacks** (it is a known malware
persistence technique) — the single biggest reason to make this explicit, opt-in, and
clearly labelled. Windows feature updates silently remove it. A broken/stale value can
leave the user unable to open any task manager — so the toggle must be bullet-proof about
cleanup and about tolerating the appended argument.
**Effort:** **S–M**. The registry write is small; the safety (argument handling, cleanup,
uninstall) is where the care goes.

---

## 5. Auto-start elevated without a UAC prompt

**What it does for the user.** Orbit launches at logon **already elevated**, with no UAC
prompt each time — so all the admin-only features above just work without "Restart as
administrator".

**How it works.** You cannot auto-elevate a normal Run-key entry without a prompt (that's
the whole point of UAC). The supported trick is a **Scheduled Task** set to
**"Run with highest privileges"**, triggered **At log on**. The task runs the exe elevated
under the user's own admin token, and Task Scheduler launching it does **not** raise a UAC
prompt. Create it via the Task Scheduler COM API (`ITaskService` / register a task with
`Principal.RunLevel = TASK_RUNLEVEL_HIGHEST`, logon trigger) or via `schtasks /create /sc
onlogon /rl HIGHEST`. Verified against Microsoft docs: `TASK_RUNLEVEL_HIGHEST` = "run with
the highest privileges", and when the task's principal is the Administrators group you must
set `RunLevel` to HIGHEST for it to run elevated; `schtasks` `/rl HIGHEST` is the CLI
equivalent and `/sc ONLOGON` is the logon trigger.

**Admin:** **creating** the task needs admin (only administrators can register tasks).
Removing it (the "disable auto-start" toggle) likewise. After that, each logon launch is
silent.

**Security considerations (worth stating plainly to the owner).** This is a real privilege
decision, not just a convenience:
- It means Orbit runs with full admin rights every session — a bigger attack surface. The
  exe path baked into the task must be fixed and in a location a standard user can't
  overwrite (don't point it at a user-writable folder, or a non-admin could swap the
  binary and get silent elevation — a classic privilege-escalation hole).
- Keep it strictly opt-in, off by default, clearly described ("Start Orbit elevated at
  logon"), and easy to remove.
- Prefer the user's own account with highest privileges over `/ru SYSTEM`; running as
  SYSTEM is more power than a task manager needs and changes how processes are owned.

**Module / UI.** New `autostart.rs` (or extend `startup.rs`): `set_auto_elevate(enabled)`
creating/deleting the task via Task Scheduler COM (`Win32_System_TaskScheduler` — new Cargo
feature) or shelling `schtasks`. Settings toggle next to the admin badge.
**Risks:** the writable-path escalation hole above; users forgetting it's on; interaction
with feature #4 (if both on, the takeover should launch the already-elevated instance).
**Effort:** **M** (COM task registration is fiddly; `schtasks` shelling is **S** but less tidy).

---

## 6. Known gaps left from v1 (from PROPOSAL.md "Limits")

The owner already flagged these; listing so the roadmap is complete. None is a bug — they
are honest limits of what v1 measures:

- **Per-process network** is not in v1 (total network only). → This is feature **#3** above.
- **Protected system processes** can't be ended or inspected even as admin; Orbit shows
  "Access denied" rather than failing silently. (Windows limitation, not fixable.)
- **Memory per process** is the *working set*; Task Manager shows *private* working set, so
  numbers differ slightly. Could be tightened later if the owner wants parity, but it's a
  cosmetic mismatch, not a gap.
- **GPU** numbers come from Windows performance counters (same source as Task Manager) — fine.
- **Per-process I/O** counts all reads/writes (files, pipes, network), not just physical
  disk; hence labelled "I/O". The Disk tile is real disk. (Deliberate, documented.)
- **Unelevated owner lookup**: Windows hides the owner of some SYSTEM processes without
  admin, so their user shows blank. Restart-as-admin already fills these in.

No action needed on these beyond #3, unless the owner specifically wants private-working-set
parity (small) later.

---

## Recommended sequence

Ordered by value-for-effort and risk, lightest and safest first:

1. **Users tab (#2) — M.** High-value, verified API path, Cargo feature already present,
   reuses the snapshot for per-session totals. Best first win.
2. **App history, Orbit's own (#1, option 2) — M.** Builds straight on the existing
   sampler, no admin, no fragile parsing. Ships a real "History" tab honestly.
3. **Ctrl+Shift+Esc takeover (#4) — S–M.** Small registry change, big "feels like a real
   task manager" payoff. Do it after #1/#2 so the thing it opens is worth opening. Invest
   in the cleanup/uninstall safety.
4. **Auto-start elevated (#5) — M.** Natural partner to #4 and a quality-of-life win once
   people rely on Orbit. Needs the security care noted above.
5. **Per-app network via ETW (#3) — L.** Most powerful, most work, most risk (admin,
   overhead, AV, schema drift). Do it last, as an opt-in power feature, once the lighter
   wins are in.
6. **v1 gaps (#6)** — nothing to schedule beyond #3; revisit private-working-set parity
   only if asked.

A theme across all of them: keep the admin-only parts **opt-in and clearly labelled**, and
reuse Orbit's existing "show what we can read, offer Restart as administrator for the rest"
pattern rather than demanding elevation up front.

---

## 7. Optimize (planned for 0.5)

The owner asked for something that helps optimize and speed up PCs, in the spirit of IObit
Driver Booster. The plan keeps the useful part and skips the risky part. Orbit stays local
and honest: it never downloads drivers or software itself, every clean-up shows what it will
remove before it removes it, and it makes no speed claims it can't measure.

Planning only, not yet checked against Microsoft's docs. API names below are the likely
route and are **(unverified)** until each feature is built.

### 7a. PC health report — M

**What it does.** One page that answers "is anything wrong with this PC?", with a plain
"Fix this" button per item where Orbit can act:
- Disk health (SMART / reliability) and free space per drive.
- Uptime, and whether a restart is pending (after updates or installs).
- Windows Update status: last check, pending updates.
- Startup load: the 0.4 startup impact numbers.
- Anything the Security scan flagged.

**Likely route.** Disk health from the Storage WMI classes (`MSFT_PhysicalDisk`; the
reliability counters probably need admin). Pending restart from the well-known registry
flags. Windows Update from the Windows Update Agent COM API (search only, no installs).
Reuses `impact.rs` and `security.rs`.

**Admin:** mostly no; disk reliability counters likely yes.

### 7b. Startup suggestions — S

**What it does.** Builds on the startup impact Orbit already measures: "These 3 apps add
the most to your sign-in. Turn them off?", with one click to disable each (the existing
startup toggle). It never suggests Microsoft or security software, and never disables
anything on its own. Shown in Startup and in the health report.

### 7c. Disk cleanup and "what's filling my disk" — M–L

**What it does.**
- Shows the size of each junk category: user and Windows temp files, the Windows Update
  download cache, crash dumps, browser caches, and the Recycle Bin.
- Clears only the categories the user ticks, and reports how much space it freed.
- A "biggest folders" view per drive, to find what's filling the disk, with "Open folder"
  instead of delete.

**Rules.** Browser caches only when that browser is closed. Never touch user documents,
Downloads, or anything outside the known junk folders. Files in use are skipped, not
forced.

**Admin:** user temp and the Recycle Bin, no. Windows temp, the update cache and crash
dumps, yes (Restart as administrator pattern).
**Risks:** deleting the wrong thing. Keep the folder list fixed and reviewed, add unit tests
for the path rules, and show a preview before every clean.

### 7d. Game / focus boost — S–M

**What it does.** One click, or automatically when a chosen game starts:
- lowers the priority of heavy background apps, or pauses them (optional);
- switches to the High performance power plan;
- shows a small "Boost on" badge.

When the game closes or the user turns it off, everything goes back exactly as it was.

**Likely route.** Reuses `actions.rs` priority and suspend, along with its critical-process
guard. Power plans via the Power API (`PowerGetActiveScheme` / `PowerSetActiveScheme`).
Game start detected from a user-picked list of exes (simple and predictable) rather than
guessing from full-screen windows.

**Admin:** none expected for your own apps. System processes are left alone.
**Risks:** an app left paused or low priority if Orbit crashes. Save the "before" state to
disk and restore it on the next start, the way `resume_all` works on exit.

### 7e. Driver check — M

**What it does.**
- Lists every device driver with its version, date, maker, and whether it is signed.
- Flags devices that report a problem (the yellow "!" in Device Manager, with the reason
  in plain words), very old drivers, and unsigned drivers.
- For updates, it offers only two safe routes:
  - drivers Windows Update itself offers for this PC, installed through Windows;
  - a link to the maker's official driver page (NVIDIA, AMD, Intel, Realtek and so on).

**Likely route.** Device list, problem codes, driver version and date through SetupAPI /
CfgMgr32. Windows Update driver search through the Windows Update Agent API.

**Admin:** listing, no. Installing a Windows Update driver, yes.
**Not doing:** downloading drivers from Orbit's own servers or third-party mirrors, or
installing drivers silently. That is where tools like this cause boot crashes.

### Not building (and why)

These are common in "optimizer" apps but don't help, so Orbit won't ship them:
- **RAM cleaners.** They push apps out of memory, and the PC gets slower as it pages them
  back in.
- **Registry cleaners.** No measurable speed gain, and a real chance of breaking apps or
  Windows.
- **"Internet booster" tweaks.** TCP tweaks with no measurable effect on modern Windows.

### Order

1. **Health report (7a) plus startup suggestions (7b):** safe, read-mostly, and shows
   value straight away.
2. **Disk cleanup (7c):** visible result (space freed), needs the most care on path rules.
3. **Game boost (7d):** builds on existing actions.
4. **Driver check (7e):** read-only listing first, Windows Update installs second.

---

## 8. Next release (planned for 0.6)

The owner's list after 0.5.0. Planning only; API names are **(unverified)** until built.

### 8a. GPU name in Performance — S

- Show the card's model name (e.g. "NVIDIA GeForce RTX 4070"), its VRAM and driver version.
- One line per GPU when there are two (laptop iGPU + dGPU).
- Likely route: DXGI `EnumAdapters1` for name and VRAM, matched to the PDH adapter LUIDs
  `pdh.rs` already reads. No admin.

### 8b. Right-click menu in History — S

- The same menu as Processes, where it makes sense:
  - Search online, Open file location, Properties, Copy name / path.
  - Show in Processes and End task, only while the app is running.
  - Remove this app from history.
- Reuses the Processes menu code and `actions.rs`. No admin.

### 8c. My Rig tab — S–M

A one-page spec sheet, with a **Copy specs** button for sharing:
- CPU: model, cores / threads, base clock.
- Memory: total, speed, slots used.
- GPU: from 8a.
- Motherboard and BIOS version.
- Drives: model, size, SSD or HDD.
- Displays: resolution and refresh rate.
- Windows edition and build.

Likely route: WMI (`Win32_Processor`, `Win32_PhysicalMemory`, `Win32_BaseBoard`,
`MSFT_PhysicalDisk`) plus what `StaticInfo` already has. No admin.
**Decided:** My Rig is 10 on key 0; Settings becomes 11 with no number key (sidebar,
palette and Ctrl+,).

### 8d. Motion like the LegacyTech site — S–M

- Buttons lift with a light sweep; icons turn on hover.
- Cards lift and glow on hover.
- Views fade and rise in with a short stagger; numbers count up on first show.
- On by default. The existing **Reduce motion** setting (and the Windows setting) turns all
  of it off. **Done:** it now reads **Animations** (on/off) in Settings > Display.
- CSS first, matching the design system's easing and durations. Must not cost CPU when
  Orbit sits in the tray.

### 8e. Malware scan through Microsoft Defender — M

Orbit should not ship its own antivirus engine. One that works needs daily signature
updates and a research team, and a weak one gives false comfort and false alarms.
Instead Orbit drives what Windows already has:
- Shows protection status: real-time on or off, definition age, last scan.
- **Quick scan** and **Full scan** buttons, with progress and results in Orbit.
- **Scan with Defender** on a file, folder or process (right-click).
- If another antivirus is installed, Defender is off, so Orbit shows that product's name
  and status instead.

Likely route: the Defender WMI classes (`MSFT_MpComputerStatus`, `MSFT_MpScan`) or
`MpCmdRun.exe`, and `SecurityCenter2` for other products. Some scans may need admin.
**Not doing:** uploading files anywhere. A VirusTotal hash lookup could come later, opt-in only.
**Done (0.6):** a Malware scan panel at the top of Security (status from
`MSFT_MpComputerStatus` and `SecurityCenter2`; Quick, Full and file / folder scans through
`MpCmdRun.exe`, hidden, one at a time, cancellable) and **Scan with Defender** in the
Processes and History menus. Threats are shown by name with **Open Windows Security**;
Orbit never removes anything. See `docs/features/defender.md`.

### 8f. Guided debloat: startup apps, scheduled tasks, services, built-in apps — L

Most people can't tell what "MicrosoftEdgeUpdateTaskMachineUA" or "SysMain" is. Orbit
explains each item in plain words and gives a verdict:
- **Safe to turn off**: what you lose, in one line.
- **Keep**: needed by Windows or the hardware.
- **Unknown**: leave it, with a Search online button.

Scope:
- Startup apps and non-Microsoft scheduled tasks: turn off (never delete).
- Services: set to Manual, never Disabled; core Windows services are not offered.
- Built-in apps (Xbox, Clipchamp, News and so on): remove for this user; they can be
  reinstalled from the Store.

Rules:
- Verdicts come from a reviewed list shipped with Orbit, not guesses.
- Nothing changes without a preview. Every change is saved, with **Undo** per item and
  **Undo all**.
- Changes to services and tasks need admin (Restart as administrator pattern).

**Risks:** breaking a feature someone uses. Keep the list short, explain each item, and
make undo work before anything else.
**Done (0.6):** Optimize > **Debloat**. Analyze reads startup apps, scheduled tasks,
services and this user's built-in apps and explains each from a reviewed list of 82 entries
(`src-tauri/src/debloat/known.rs`): Safe to turn off (what you lose), Keep (why) or Unknown
(Search online). A never-offer guard is checked at analysis and before every change. Only
ticked Safe items change, after a preview: startup apps and tasks are turned off, services set
to Manual, apps removed for this user. Every change is logged with its previous state before
it is made; Changes has Undo per item and Undo all (removed apps: their Store page). Windows
tasks are offered only when on the reviewed list. See `docs/features/debloat.md`.

### Order

1. **GPU name (8a)** and **History right-click (8b):** small, quick wins.
2. **My Rig (8c):** reuses 8a.
3. **Motion (8d):** a UI pass across all views.
4. **Defender scan (8e).**
5. **Guided debloat (8f):** last, it needs the most care.

---

## 9. Next release (planned for 0.7)

The owner's list after 0.6.0. Planning only; API names are **(unverified)** until built.

### 9a. Animated logo in the sidebar — S

- The pixel L from the logo kit, in the empty space under Settings.
- It assembles block by block when Orbit opens, then the cyan signal pixel pulses slowly.
- Optional: the pulse speeds up with CPU load, so it doubles as a live signal.
- Follows the **Animations** switch and the Windows setting. Stops while Orbit is hidden in the tray.
- CSS and SVG only, no image files.

**Done (0.7):** the mark sits in a thin ring between the nav and the Live / System card and is
left out when the window is too short for it. Blocks assemble in about a second; the cyan
pixel pulses every 3.2 s at idle, up to about once a second at 100% CPU. Clicking it opens
Settings at About. Details in `docs/features/logo.md`.

### 9b. CPU and GPU temperatures in Performance — M–L

**What it does.** Live temperatures on the CPU and GPU cards and panes, with a 60-second
graph, the peak in the window, and a warning colour when hot. A compact sensor panel in the
spirit of Quick CPU: per-core temps where available, package temp, GPU hot spot, fan speed
and power draw where the hardware reports them.

**GPU (the easy part).**
- NVIDIA: NVML (`nvml.dll`, installed with the driver): temperature, fan, power, clocks. No admin.
- AMD: ADLX. Intel: the Intel graphics control library.
- No supported library: show no temperature rather than a guess.

**CPU (the hard part).** Windows has no official API for CPU temperature. Apps like Quick CPU,
HWiNFO and LibreHardwareMonitor read it from the processor through their own kernel driver.
The options, safest first:
1. **Read it from a tool that's already running.** LibreHardwareMonitor and HWiNFO can share
   their sensors (WMI or shared memory). Orbit reads them if present. No driver, no admin.
2. **The ACPI thermal zone** (`MSAcpi_ThermalZoneTemperature`): needs admin, and on many
   desktops it's the motherboard's zone, not the CPU, or never changes. Shown as
   "approximate" only if it actually moves.
3. **A signed sensor driver** (for example PawnIO, which newer LibreHardwareMonitor uses):
   only an existing, signed, maintained one, installed by the user, never bundled silently.

**Not doing:** shipping WinRing0 or any unsigned or old driver. Windows Defender blocks
WinRing0 as a vulnerable driver, and a bad driver can crash the PC.

**Admin:** GPU, no. CPU depends on the route (1: no, 2: yes, 3: the driver install only).
**Risks:** wrong numbers. Show where each reading comes from, and hide a sensor rather than
show a made-up value.

**Done (0.7):** the CPU and GPU tiles show the temperature beside the load (normal / warm /
hot: CPU 80 / 90 °C, GPU 80 / 87 °C), and the CPU and GPU panes have a Sensors block (60-second
graph, peak, per-core temperatures and clocks, package power, fan, power and limit, clocks, and
the source). GPU: NVIDIA through NVML loaded at run time, matched to its DXGI adapter by PCI
address; AMD and Intel say "Temperature not available on this GPU" (no vendor library yet).
CPU: routes 1 (LibreHardwareMonitor / OpenHardwareMonitor WMI, HWiNFO shared memory) and 2
(ACPI, elevated, only once it moves, marked approximate); without them a one-line hint links to
HWiNFO and LibreHardwareMonitor. Route 3 not taken: no driver of any kind. Settings > Display
has °C / °F. Details in `docs/features/sensors.md`.

### Order

1. **Animated logo (9a):** small and self-contained.
2. **GPU temperatures (9b, GPU part).**
3. **CPU temperatures (9b, CPU part):** route 1 first, then decide on 2 and 3.

---

## 10. Next release (planned for 0.8)

The owner's notes after 0.7.0.

### 10a. "Start in tray" opens the window anyway — S (bug)

**Seen:** with "Start minimized to tray" on, Orbit's window opens on start.

**What the code does today:** the setting only applies when Windows starts Orbit at sign-in
(the Run entry passes `--minimized`; checked on the owner's PC, it does). Orbit then stays in the
tray only if the tray icon exists at that moment (`shell.rs`, `tray_visible`). Opening Orbit
by hand always shows the window, by design.

**Likely causes, to confirm:**
- At sign-in Orbit can start before the taskbar is ready, so the tray icon isn't there yet and
  Orbit falls back to showing the window.
- An auto-update restarts Orbit without `--minimized`.

**Fix:** when started with `--minimized`, wait for the taskbar (retry the tray icon for up to
about 30 s, and re-add it when Explorer announces `TaskbarCreated`) instead of showing the
window. Restart after an update with the same flags it was started with.

### 10b. Dismiss the startup suggestions card — S

The "These N apps add the most to your sign-in" card in Startup has no way to close it.
- Add a close button ("Hide"). Remember it per set of apps: the card comes back only when
  the suggested apps change.
- Same for the compact card in Health.

### 10c. Startup list stays in sync with changes made elsewhere — S–M

**Question asked:** if an app's own setting (for example Claude's "open at login") is changed, does
Orbit see it?

**Today:**
- Orbit reads the same places Windows and Task Manager use: the Run keys, the Startup folders,
  and the StartupApproved on/off switches. Claude's setting writes its own Run entry, so Orbit
  does see it.
- But the Startup tab loads once and only re-reads on **Refresh**, so a change made while
  Orbit is open shows up late.
- Store apps that start through a packaged startup task (WhatsApp, Spotify from the Store,
  and so on) aren't listed at all.

**Fix:**
- Re-read the list each time the Startup tab opens, and watch the Run and StartupApproved keys
  (`RegNotifyChangeKeyValue`) so a change made elsewhere appears within a second.
- List packaged startup tasks (`Windows.ApplicationModel.StartupTask` per package) with their
  on/off state. Turning one on from Orbit isn't allowed by Windows for other apps, so Orbit can
  only show it and point to Settings > Apps > Startup.
