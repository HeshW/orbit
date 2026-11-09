# Feature: security (v0.4)

A Security view (08) that scans running programs, signature info in the details drawer,
and the five security commands from `docs/V04-CONTRACT.md`. No shapes or names changed.

Files: `src-tauri/src/security.rs`, `ui/js/views/security.js`, `ui/js/details.js`,
`ui/css/views/security.css`, `ui/js/mock/security.js`.

## Backend (`security.rs`)

| command | how |
|---|---|
| `get_signatures(paths)` | `WinVerifyTrust` (WINTRUST_ACTION_GENERIC_VERIFY_V2) on the embedded signature first. On `TRUST_E_NOSIGNATURE` / subject form unknown / provider unknown the file is looked up in the Windows catalogs (`CryptCATAdminAcquireContext2` SHA256, then SHA1; `CalcHashFromFileHandle2`, `EnumCatalogFromHash`, `CatalogInfoFromContext`) and verified with `WTD_CHOICE_CATALOG`. Revocation off and `WTD_CACHE_ONLY_URL_RETRIEVAL`, so no network wait. Publisher = signer cert display name (`WTHelperProvDataFromStateData` + `WTHelperGetProvSignerFromChain` + `CertGetNameStringW`). Status: success = signed, no signature and no catalog = unsigned, any other trust error = invalid, file can't be opened = unknown. Cached per normalized path + size + mtime ("unknown" not cached), up to 8 worker threads, in `spawn_blocking`. |
| `security_scan` | Reads the latest snapshot, one finding per program file (the oldest process running it), sorted high > medium > info, then name. |
| `file_sha256(path)` | sha2 streaming, 64 KB buffer, lowercase hex. |
| `get_process_modules(pid, startTime?)` | `OpenProcess(QUERY_INFORMATION \| VM_READ)`, start time checked on that handle, `EnumProcessModulesEx(LIST_MODULES_ALL)` (retries a partial copy), `GetModuleFileNameExW`. Size = file size on disk (falls back to `SizeOfImage`). Main exe first, as Windows lists them. |
| `get_process_environment(pid)` | Access checked with `OpenProcess` first (so a denied read says "Access denied. Try Restart as administrator." instead of an empty list), then sysinfo `environ()`. Sorted by name; hidden `=C:` entries kept. |

Scan rules (reasons are plain sentences, in this order):

| check | severity |
|---|---|
| Signed by Microsoft and in System32 / SysWOW64 | never flagged |
| Name copies a Windows process outside `%SystemRoot%` (svchost.exe, csrss.exe, lsass.exe, ...), a one-edit lookalike (svch0st, scvhost; not for "services"/"explorer", which are ordinary words), or a name starting with the word "Windows" outside `%SystemRoot%`. Microsoft-signed files are exempt. | high |
| Hidden or system attribute on the exe | high |
| Larger than 200 MB and the last 1 MB is at least 99% zeros | high |
| Exe missing on disk | medium |
| Unsigned or invalid signature | medium; high with an odd folder or any high sign above |
| Temp, a Downloads folder, a browser profile (Chrome/Edge/Brave/Vivaldi/Chromium/Yandex User Data, Firefox Profiles, Opera), straight in ProgramData | medium; high if unsigned |
| Other AppData | info |

Assumption (differs from the task text): plain AppData is **info**, not medium, because
many normal apps install per user there (Discord, VS Code user setup, Spotify, OneDrive).
This matches the scaffold mock. Unsigned + AppData is therefore medium, not high. Temp,
Downloads and browser profiles (all inside AppData too) stay medium/high as specified.

Cargo note: the `windows` crate gates `WTHelperProvDataFromStateData` and
`WTHelperGetProvSignerFromChain` behind `Win32_Security_Cryptography_Sip`, which Orbit's
Cargo.toml does not enable, so `security.rs` declares those two in a local
`#[link(name = "wintrust")] extern "system"` block.

## UI

- **Security view** (`views/security.js`): stats (High, Medium, Info, Checked processes),
  filter chips with counts, "Scanned hh:mm", "Scan now / Scan again" (spinner while
  scanning), table with icon, name + path, PID (or "Exited"), publisher or "Unsigned" /
  "Invalid signature", reasons as chips, severity chip, and per-row Details
  (`selectProcess`), Open file location, Check on VirusTotal, End task (`endTask` from
  processes.js, with the row's start time). The global search filters findings. The scan
  runs the first time the view opens. Empty state: "Nothing suspicious found." plus what
  was checked and how many processes / files. When only info rows exist a note says so.
- **VirusTotal**: `checkOnVirusTotal(exe, name)` (exported) hashes with `file_sha256` and
  opens `https://www.virustotal.com/gui/file/<hash>` via `open_url`. Tooltip: "Orbit sends
  only the file's SHA-256 hash, never the file."
- **Palette** (`registerCommands`): "Security scan" (goes to the view and scans), and
  "Check <name> on VirusTotal" for running processes whose name the typed text contains
  (or, for "virustotal" alone, the last scan's findings). Items are only offered for a
  matching query so they do not flood the Actions group.
- **Details drawer** (`details.js`): Signature fact under Path (chip Signed / Unsigned /
  Invalid / Unknown + publisher, loaded when the drawer opens, cached), a "Verify
  signature" button with a plain verdict and time, and two expandable lists loaded on
  first open: Modules (file name, folder, size) and Environment (name, value, copy
  button per variable). Errors show with "Try again".
- **Mock**: mirrors the Rust rules on the mock processes (orbit.exe medium, AppData apps
  info) and adds two high findings: `chromeupdate.exe` (hidden, unsigned, browser profile)
  and `svch0st.exe` (lookalike, unsigned, Temp).

## How to test

- `cargo test` in `src-tauri`: 12 tests in `security::tests` (rules per severity, edit
  distance, environment parsing, hashing, real catalog signatures on notepad.exe /
  kernel32.dll, unsigned Cargo.toml, missing file = unknown, own modules and environment,
  start-time mismatch = "That process has exited.").
- Live (debug exe, CDP): `invoke('security_scan')`, `get_signatures` on every exe in the
  snapshot, `file_sha256`, `get_process_modules`, `get_process_environment`.
- Mock: serve `ui/`, open Security, open Details on a row, expand Modules / Environment.

Measured on the dev PC (debug build, unelevated, 538 processes, 176 program files):
first scan 1.9 s, repeat scan 66 ms (cache); `get_signatures` on all 176 files from cache
19 ms. 161 signed, 15 unsigned, 0 unknown. 41 findings (15 medium, 26 info, 0 high).
Modules and environment of SYSTEM processes (lsass, services) return "Access denied.
Try Restart as administrator."

## Not verified

- Elevated run (modules / environment of SYSTEM processes should then load).
- A real file with an invalid signature, a hidden exe, a zero-padded exe and a missing
  exe on a live system (covered by unit tests on the rules only).
- The browser opening VirusTotal from the live app (the mock call was checked; the live
  `open_url` command is the existing one, not exercised to avoid opening the owner's
  browser).
- In the mock, the two high findings have pids that are not in the mock process list,
  so they show "Exited" (Details and End task disabled). See contract changes.
