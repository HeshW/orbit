# Orbit

A Windows task manager by LegacyTech. Rust (Tauri 2) backend, plain HTML/CSS/JS UI.
See `PROPOSAL.md` for features and `docs/CONTRACT.md` for the internals.

## Download

Get **[Orbit-Setup.exe](https://github.com/HeshW/orbit/releases/latest/download/Orbit-Setup.exe)** (Windows 10/11, 64-bit)
or visit [orbit.legacytech.me](https://orbit.legacytech.me). Orbit isn't code-signed yet, so Windows
SmartScreen may warn you: click **More info**, then **Run anyway**.

Installed copies (0.3.0 and later) check for updates when they start and update themselves
(Settings > About has the switch and a "Check for updates" button). Anyone on 0.2.x installs
0.3.0 by hand once.

## What's new

Each version's changes are on [GitHub Releases](https://github.com/HeshW/orbit/releases), and
the latest five are in the What's new section of [orbit.legacytech.me](https://orbit.legacytech.me).
The release notes are the commit subjects since the previous tag, so write them for people.

## Releasing

1. Bump the version in `src-tauri/tauri.conf.json` and `src-tauri/Cargo.toml` (and the
   version text in `site/index.html`).
2. Commit, push, then tag and push the tag: `git tag v0.3.1 && git push origin v0.3.1`.
3. GitHub Actions (`.github/workflows/release.yml`) builds the installer in the cloud, signs
   the update and publishes the release with `latest.json` and `Orbit-Setup.exe`. Installed
   copies pick it up on their next start. Pushing to `main` also redeploys the landing page.

Update signing works like RiftLens: the public key goes in `plugins.updater.pubkey` in
`tauri.conf.json`, and the private key and its password live only in the repo secrets
`TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. Losing the private key
means existing installs can't verify future updates (users would reinstall by hand once).

Because `createUpdaterArtifacts` is on, a local `cargo tauri build` needs the signing key in
the environment; a plain `cargo build --release` (below) doesn't.

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

## UI smoke test

`tests/ui-smoke.mjs` serves `ui/` with the mock backend, opens it in Chromium, visits every
view in all four themes, opens the palette and the details drawer, and fails on any console
error, page error or empty view. CI (`.github/workflows/ci.yml`) runs it and `cargo test` on
pushes and pull requests to `main` and `v0.4`, and a release only builds when both pass.

```
npm install
npx playwright-core install chromium
npm run test:ui
```

To use the installed Microsoft Edge instead of downloading Chromium:
`$env:ORBIT_BROWSER = "msedge"; npm run test:ui`.

## Preview the UI in a browser

`ui/js/api.js` has a mock backend, so `ui/` runs in any browser when served over HTTP
with `.js` served as `text/javascript`.

## License

MIT. See [LICENSE](LICENSE). Copyright (c) 2026 LegacyTech.
