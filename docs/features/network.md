# Feature: network (v0.4)

A Network view (07) listing every connection and open port with the app behind it, a
"Find port" box ("What's using port 3000?"), and per-app download/upload rates when Orbit
runs as administrator. Commands and shapes as in `docs/V04-CONTRACT.md`; nothing renamed.

Files: `src-tauri/src/network.rs`, `ui/js/views/network.js`, `ui/css/views/network.css`,
`ui/js/mock/network.js`.

## Backend (`network.rs`)

| item | how |
|---|---|
| `list_connections` | `GetExtendedTcpTable(TCP_TABLE_OWNER_PID_ALL)` and `GetExtendedUdpTable(UDP_TABLE_OWNER_PID)` for `AF_INET` and `AF_INET6`, in `spawn_blocking`. The buffer grows until it fits (the table can grow between the size query and the read). Rows are read bounds-checked (`read_unaligned`, never past the buffer whatever the count says). Ports converted from network byte order; IPv4 from network order; IPv6 via `Ipv6Addr`, link-local keeps its zone (`fe80::1%11`). TCP states mapped to camelCase (`listen`, `established`, `timeWait`, `closeWait`, `synSent`, `synReceived`, `finWait1/2`, `closing`, `lastAck`, `closed`, `deleteTcb`). Listeners have `remoteAddress`/`remotePort` null; UDP has no remote and `state` null. Sorted by local port. One table failing is skipped; all four failing is "Could not read the connection table." No admin needed. Live: about 420 rows in 1.4 ms (unit test) / 38 ms round trip from the UI. |
| `per_app_status()` / `per_app_network_status` | Unelevated (`IsUserAnAdmin`, cached): `{ available: false, reason: "Needs administrator rights. Use Run as admin." }` and nothing is started. Elevated: the first call starts the trace thread; then "Starting per-app network." until it runs, `available: true` while it runs, or the failure reason. |
| `per_app_rates()` | `None` unless the trace runs. Otherwise one lock and a map swap: bytes per pid since the previous call divided by the seconds since then. Pids without traffic are not in the map. |
| ETW trace | Raw `windows` ETW APIs (no new crate). Real-time session "Orbit Network Trace" (`StartTraceW`, own session GUID, 64 KB buffers, 1 s flush) with **Microsoft-Windows-Kernel-Network** `{7DD42A49-5329-4832-8DFD-43D979153A88}` enabled (`EnableTraceEx2`, level 5, keywords IPv4 0x10 + IPv6 0x20). Consumer on thread `orbit-net-trace`: `OpenTraceW` (real-time + EVENT_RECORD) and `ProcessTrace`. Counted events: TCPv4 10/11, TCPv6 26/27, UDPv4 42/43, UDPv6 58/59 (sent/received); each payload starts with `PID: u32, size: u32`. Connect, disconnect, retransmit, TCP copy and fail events are ignored. A session with the same name left over from a crash (`ERROR_ALREADY_EXISTS`) is stopped and started again. Access denied and "too many sessions" (1450) get plain reasons. |
| `shutdown()` | Marks shutdown (nothing starts after it) and stops the session with `ControlTraceW(STOP)`, which ends `ProcessTrace`; the thread then closes its handle. If someone else stops the session, the status says so. |

Cargo note: the `windows` crate gates `OpenTraceW` and `EVENT_TRACE_LOGFILEW` behind
`Win32_System_Time`, which Orbit does not enable. `network.rs` declares `OpenTraceW` in a
local `#[link(name = "advapi32")]` block and lays out `EVENT_TRACE_LOGFILEW` itself, with
the two blocks it never reads (EVENT_TRACE, TRACE_LOGFILE_HEADER) as opaque 8-byte-aligned
arrays. Size (448) and every field offset were checked against the Windows SDK headers
with MSVC (`sizeof`/`offsetof`) and are compile-time asserted. ferrisetw was not used (no
crate added).

## UI (`views/network.js`)

- Head: Connections, Listening, Established, Apps (distinct pids).
- **Find port** (prominent, top left): accepts "3000", ":3000", "port 3000",
  "localhost:3000". Filters the table to that local port and highlights the matching rows
  and port numbers. The answer names each app holding it ("Listening on 0.0.0.0, [::]
  (TCP)", "Open on ... (UDP)", "N connections") with Details and End task, and counts
  connections from other local apps to it. "Port N is free." when nothing holds it;
  "Ports are numbers from 1 to 65535." for bad input. Esc or the x clears it.
- **Top apps / Download and upload** (top right): when `snapshot.network.perApp.available`,
  the 5 busiest processes by `netRxBps + netTxBps` (null counts as 0) with a bar and both
  rates, refreshed every snapshot; names open Details. Otherwise a muted dashed note with
  the reason, and a Run as admin button (`restartAsAdmin` from app.js) when unelevated.
- Toolbar: All / Listening (TCP listen + UDP) / Established, and TCP / UDP toggles (one of
  them always stays on), each with counts; "N of M"; refresh button.
- Table: process (icon + name; "System Idle Process" for pid 0, "PID n" dimmed if gone),
  PID, protocol (+ "v6"), local address, remote address, state chip (Listening cyan,
  Established green, others muted, UDP "-"). Sortable headers. Row buttons: Details
  (`selectProcess`), Copy address (remote if any, else local, `[v6]:port` form), Search
  online for the remote host (`search_online`; disabled for loopback, unspecified,
  link-local and private addresses), End task (`endTask` from processes.js with the row's
  start time; disabled for pid 0/4, Orbit itself and exited processes). The global search
  filters rows (name, pid, addresses, ports, state).
- Refreshes `list_connections` every 2 s only while the view is visible and the window is
  not hidden; the table DOM is only rebuilt when a visible row changed, and focus on a row
  button survives a refresh.
- Palette (`registerCommands`): "Find port", "Network connections", and "Find port N"
  when the typed text is a port ("3000", "port 3000", "what's using port 3000").
- `findPort(port?)` is exported (opens the view with the box focused/filled).

## Mock (`mock/network.js`)

Tied to mock snapshot pids: dev servers on 3000 (node vite, 0.0.0.0 and [::]) and 5173
(node server.js, 127.0.0.1 and [::1]) with Chrome connected to both, browser / Discord /
VS Code / Steam / OneDrive connections, svchost and System listeners and UDP (incl. a
link-local IPv6), 0-2 TIME_WAIT rows that come and go. Ending a dev server's process
frees its port. Flags: `?netfail` (command throws), `?nonet` (no connections).
`per_app_network_status` answers by `ctx.isAdmin()`.

## How to test

- `cargo test --lib network`: port/address conversion, state names, all four table
  layouts from synthetic buffers, buffer overrun safety, event parsing (all counted ids,
  ignored ids, short payloads), aggregation into per-second rates, unelevated status (no
  session started), and a live test that opens its own TCP listener, TCP connection, UDP
  socket and IPv6 listener and finds them with the right pid/state.
- `cargo test --lib dump_connections -- --ignored --nocapture` prints this PC's table.
- Live: debug exe with CDP, `await __TAURI__.core.invoke('list_connections')`, open
  Network, type a port (for example the CDP port, held by msedgewebview2.exe).
- Mock: serve `ui/`, open Network, type 3000 or 5173, try the flags above.

Checked: debug build, unelevated, 419 live connections; view refreshed every 2 s, Find
port, palette, all four themes, no console errors (live and mock). Text contrast checked
in all four themes (the highlighted port uses ink text on the cyan tint, 4.5:1+ in
Daylight).

## Not verified

- **The elevated path is untested**: starting the ETW session, receiving Kernel-Network
  events, real per-app rates, `shutdown()` stopping the session, and the leftover-session
  restart. It can't be run here (no elevation allowed). Event parsing and aggregation are
  unit tested with synthetic payloads; the logfile struct layout is checked against the SDK.
- Top apps rendering was checked in the mock by injecting rates into the snapshot (the
  mock snapshot always says per-app is unavailable; see contract changes).
- Copy address, Search online and End task were exercised in the mock only; the live app
  was not used to end processes or open the browser.
