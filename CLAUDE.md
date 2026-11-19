# Orbit

A Windows task manager: Tauri 2 Rust backend in `src-tauri/`, plain ES-module UI in `ui/` (no
framework, no build step). Public repo HeshW/orbit; the site in `site/` deploys to
orbit.legacytech.me when `main` is pushed. Read `docs/CONTRACT.md` before changing commands,
events or file layout, and `docs/ROADMAP.md` for what's shipped and planned.

## Commands

- Build (PowerShell): `$env:CARGO_TARGET_DIR="E:/Work/.orbit-target"; cargo build --release` in
  `src-tauri`, then copy the exe to `src-tauri/target/release/orbit.exe`. On the owner's PC use
  only that shared target folder (not the README's `%TEMP%` one), one build at a time; stop if
  C: has under 10 GB free or E: under 15 GB.
- Rust tests: `cargo test` in `src-tauri` (same `CARGO_TARGET_DIR`).
- UI smoke test: `$env:ORBIT_BROWSER="msedge"; npm run test:ui`. It serves `ui/` with the mock
  backend, visits every view in all four themes and fails on any console error.
- CI (`.github/workflows/ci.yml`) runs both on pushes and pull requests to `main`; `release.yml`
  runs them again on `v*` tags before building the installer.

## Layout

- `src-tauri/src/lib.rs` registers every command in `generate_handler!`. One module per feature
  (`rules.rs`, `boost.rs`, `debloat/`, `startup/`, ...); bigger modules keep tests in `<name>/tests.rs`.
  `sampler.rs` is the process-list tick that features like Boost and rules hook into.
- `ui/js/api.js` `call(cmd, args)` talks to Rust. `ui/js/mock/` is a mock backend so the UI runs in
  a browser; `ui/js/views/` has one file per view, `views/optimize/` one per Optimize tab.
- `ui/css/tokens.css` holds the themes; each feature's CSS uses its own class prefix (`ru-` for rules).
- `docs/features/<name>.md` explains each feature; `site/index.html` is the landing page.

## Adding a feature

1. Rust command in its module, registered in `lib.rs`.
2. A matching mock in `ui/js/mock/` (wired in `mock/index.js`; the registry is described in
   `docs/V04-CONTRACT.md`), so the smoke test can reach it.
3. Smoke test coverage in `tests/ui-smoke.mjs`.
4. A `docs/CONTRACT.md` entry and `docs/features/<name>.md`.
5. For a release: one card in `site/index.html`.

## Conventions

- UI copy: one sentence per card, sentence-case buttons, no emoji, no feature lists packed into
  a sentence. Errors say plainly what happened and what the user can do.
- Commit subjects become the public release notes (`release.yml` lists them since the last tag),
  so write them for users: "Rules has a switch to turn every rule off". Start internal commits
  with `WIP:` and they are left out.
- Each release is built on a `vX.Y` branch, live-tested on the owner's PC, then `main` is
  fast-forwarded and a `vX.Y.Z` tag is pushed. Version lives in `src-tauri/tauri.conf.json`,
  `src-tauri/Cargo.toml` (and `Cargo.lock`), and three spots in `site/index.html`.
- Anything that changes the PC is guarded in Rust, not just hidden in the UI: Windows, security
  and anti-cheat processes are refused there.

## Live testing the real exe

Orbit is single-instance, so the owner closes their installed copy first. Start the built exe
with throwaway folders: `WEBVIEW2_USER_DATA_FOLDER`, `ORBIT_HISTORY_DIR`, `ORBIT_SHELL_DIR`,
`ORBIT_BOOST_DIR`, plus `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9431` to
drive the page over CDP. `ORBIT_RULES_DIR`, `ORBIT_DEBLOAT_DIR`, `ORBIT_CLEANUP_ROOT` and
`ORBIT_FAKE_NO_TASKBAR_SECS` work only in debug builds, so a release build writes rules to the
real `%LOCALAPPDATA%\me.legacytech.orbit`; clean that up.
Close the test copy by its PID, never by name.

## Never do

- Touch the installed Orbit (`%LOCALAPPDATA%\Orbit`) or its process, or kill `orbit.exe` by name.
- Make real changes to the PC in tests: no cleanup, debloat Apply, disabling startup items or
  services, power plan switches, Defender scans or UAC prompts. Use mocks, unit tests with fake
  OS layers, and scratch processes (a copied exe outside `C:\Windows`), never the owner's apps.
- Write Image File Execution Options keys, install a driver, or set Realtime priority.
- Change `plugins.updater.pubkey` in `tauri.conf.json`: installed copies would stop updating.
- Tag a release or update `main` without the owner's "Yes, release X".
