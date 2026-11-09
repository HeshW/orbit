# Feature: users (v0.4)

A Users view (05) like Task Manager's: one row per signed-in Windows session with its CPU,
memory, I/O and process count, expandable to the session's busiest processes, plus
Send message, Disconnect and Sign out. Commands and shapes as in `docs/V04-CONTRACT.md`;
nothing renamed.

Files: `src-tauri/src/users.rs`, `ui/js/views/users.js`, `ui/css/views/users.css`,
`ui/js/mock/users.js`.

## Backend (`users.rs`)

| item | how |
|---|---|
| `list_sessions` | `WTSEnumerateSessionsW`, then per session `WTSQuerySessionInformationW` for `WTSUserName`, `WTSDomainName`, `WTSConnectState`, `WTSClientName` and `WTSSessionInfo` (`LogonTime`, FILETIME to Unix seconds, read with `read_unaligned`). Sessions without a user name are skipped (services session 0, the RDP listener, a console nobody is signed in to). `isCurrent` = `ProcessIdToSessionId(GetCurrentProcessId())`. States: Active, Connected, Disconnected, Idle, Listen, everything else "other". Current session first, then by id. Runs in `spawn_blocking`. No admin needed. |
| `session_action` | `disconnect` = `WTSDisconnectSession`, `signout` = `WTSLogoffSession` (both `bWait = FALSE`), `message` = `WTSSendMessageW` with title "Message from Orbit", `MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND`, timeout 0 and `bWait = FALSE` (returns at once, nobody waits on a reply). Checked before any WTS call: unknown action ("Unknown session action: x."), session 0 ("Session 0 runs Windows services. Orbit won't change it."), empty message ("Type a message first."), over 1000 characters. Errors: 5 = "Access denied. Try Restart as administrator.", 2 / 7001 / 7022 = "That session has ended.", 7007 / 7041 = not connected / no screen for a message, others "Could not ... (error N).". The backend does not refuse the current session; the UI confirms first. |

## UI (`views/users.js`)

- Head stats: Signed in, Active, Disconnected, Processes. A note says the numbers add up
  every process in the session and that shared memory counts once per process (the sum of
  working sets can exceed installed RAM; on the dev PC 38.6 GB of 32 GB).
- Table: User (chevron, initial tile, name with `domain\user` tooltip, "You" chip), Session,
  Status chip, Client ("This PC" for the console), Signed in (time, "Signed in Nd Nh ago"
  tooltip), Processes, CPU, Memory, I/O (heat cells like Processes), actions menu (`...` and
  right click: Show/Hide processes, Send message, Disconnect, Sign out). Sortable headers;
  default order is you first, then by session. The global search filters by user, domain,
  client, status.
- Narrow windows: below 1400 px Session and Client hide (both are in the expanded row's
  label); below 1100 px Signed in hides too and the expanded row stacks. The expanded row's
  `colspan` follows the visible column count (a larger one adds phantom columns in a fixed
  table) and rows rebuild when that count changes on resize.
- Expanded row (your own session starts open): top 8 processes by CPU then memory, with
  icon, PID, CPU, memory, I/O; click opens Details. "And N more processes." Buttons Send
  message, Disconnect (disabled when already disconnected), Sign out. For other users while
  unelevated: "Windows lets only administrators change other users' sessions. Try Restart
  as administrator." (link runs `restartAsAdmin`).
- Live updates: sessions refresh every 5 s while visible; every snapshot updates numbers in
  place. The rows rebuild only when the list or an expanded row's process order changes,
  never while a pointer is down on the table (the rebuild waits for pointerup, so clicks are
  not lost); focus is restored after a rebuild.
- Confirms: Sign out is always `confirmDialog({ danger: true })` ("Sign out of Windows?" for
  your own session, "Sign out Sam?" for others). Disconnect of your own session is a danger
  confirm; of another user a normal confirm. Send message is a `formDialog` with a text area
  (1000 characters, counter); errors show inline, access denied explains that only
  administrators can message other users. Action errors toast with a "Restart as admin"
  button when the error asks for it.
- Palette (`registerCommands`): "Signed-in users" always; once 2+ characters are typed,
  "Send message to X", "Disconnect X" (not for disconnected sessions) and "Sign out X"
  (danger) per session ("Hesh (you)" for your own).

## Mock (`mock/users.js`)

Hesh (session 1, current, active) and Sam (session 2, disconnected). Unelevated, actions on
Sam fail with access denied; after Run as admin (mock) they work (disconnect changes the
state, sign out removes him). Actions on the current session succeed and do nothing.
Same validation as the backend. Flags: `?rdpuser` (a third, remote session "Studio" from
STUDIO-LAPTOP), `?usersfail` (`list_sessions` throws), `?nousers` (empty list).

## How to test

- `cargo test --lib users`: action parsing, state names, FILETIME conversion, error
  mapping, Win32 code extraction, message validation, requests refused before any WTS call
  (unknown action, session 0, empty message), and a read-only `list_sessions` that finds
  exactly one current session matching `ProcessIdToSessionId`.
- Mock: serve `ui/`, open Users, expand Sam, try every action unelevated, then the admin
  link, then again; try the flags and the palette ("sign out", "message").
- Live: debug exe with CDP; `await __TAURI__.core.invoke('list_sessions')`; open Users.

Checked: cargo build with no warnings, `cargo test` (75 passed), `node --check` on all
`ui/js` files. Mock: all actions, dialogs (danger confirms focus Cancel), menus, palette,
flags, all four themes, other eight views still open, no console errors; text contrast
measured >= 4.5:1 in all four themes. Live (debug exe, unelevated): one session (Hesh,
active, logon time correct), view renders and updates every tick, expand/collapse, Details
from a process, all four themes, no console errors; `session_action` called only with
requests the backend refuses before touching Windows.

## Not verified

- **The actions against real sessions were never run**, by design: disconnect, sign out
  and message (own or other session) are untested live. The error mapping is unit tested;
  the WTS calls compile against the documented signatures.
- A second real user, an RDP session (client name), and the elevated path: the dev PC has
  one local session and Orbit was never elevated.
- Whether `WTSSendMessageW` to a disconnected session queues, fails or is dropped (the mock
  accepts it).
