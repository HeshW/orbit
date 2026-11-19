# Feature: services on svchost rows (v1.2, 14a)

A process row that hosts Windows services says which ones, and the details drawer lists them.
Before this, Services could jump to a process but not the other way round. Example: an
`svchost.exe` row shows "Windows Audio, Windows Audio Endpoint Builder" under its name.

Files: `src-tauri/src/services.rs` (`process_services`, `group_hosted` with its tests),
`src-tauri/src/model.rs` (`HostedService`), `ui/js/hosted.js`, `ui/js/mock/services.js`.
Shared files touched: `lib.rs` (one command), `views/processes.js` (row text and tooltip),
`details.js` (Services section), `views/services.js` (`showService`), `css/views/processes.css`,
`css/views/lists.css`, `mock/index.js`, `tests/ui-smoke.mjs` ("svchost services").

## How it works

- `process_services` enumerates services once (`EnumServicesStatusExW`, the PID is
  `ServiceStatusProcess.dwProcessId`) and groups them by PID. It skips the per-service start
  type and description lookups that `list_services` does, so it is the lighter call.
  Read-only, no admin.
- It is a separate command, not a `ProcessRow` field: the per-second snapshot stays as it is and
  services are never enumerated on a tick. The UI caches the answer in `hosted.js` and asks
  again every 10 s while Processes is shown (and when the drawer opens).
- Any process that hosts services gets the text, not only svchost. App groups do not (a group
  has no single PID).
- Row: the names go in the row's secondary text when the row has no window title, and always in
  the tooltip. Long lists are cut with an ellipsis.
- Drawer: "Services (n)" lists each one with **Go to service**. That opens Services, clears the
  search, scrolls to the service and marks the row until you leave the view.

## Tests

- Rust: `services::tests` check the grouping (sorted by display name, PID 0 skipped, no
  duplicates). No OS calls.
- UI smoke: a mock svchost row shows its services, the drawer lists the same number, and
  Go to service marks the row in Services.
