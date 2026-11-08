# Orbit

A Windows task manager by LegacyTech. Rust (Tauri 2) backend, plain HTML/CSS/JS UI.
See `PROPOSAL.md` for features and `docs/CONTRACT.md` for the internals.

## Download

Get **[Orbit-Setup.exe](https://github.com/HeshW/orbit/releases/latest/download/Orbit-Setup.exe)** (Windows 10/11, 64-bit)
or visit [orbit.legacytech.me](https://orbit.legacytech.me). Orbit isn't code-signed yet, so Windows
SmartScreen may warn you: click **More info**, then **Run anyway**.

Installers are built in the cloud by GitHub Actions (`.github/workflows/release.yml`) when a
`v*` tag is pushed. Keep the tag equal to `version` in `src-tauri/tauri.conf.json`.

## Run from a local build

```
src-tauri\target\release\orbit.exe
```

The UI is built into the exe, so it runs on its own. Some actions (ending SYSTEM
processes, machine-wide startup entries, services) need "Run as admin".

## Build

Needs Rust (stable MSVC) and the Visual Studio C++ build tools.

```
cd src-tauri
cargo build --release
```

If cargo fails with "file in use" errors (os error 32 / LNK1104), build to a target
folder outside the project and copy the exe back:

```
$env:CARGO_TARGET_DIR = "$env:TEMP\orbit-target"
cargo build --release
```

`cargo test` runs the unit tests.

## Preview the UI in a browser

`ui/js/api.js` has a mock backend, so `ui/` runs in any browser when served over HTTP
with `.js` served as `text/javascript`.
