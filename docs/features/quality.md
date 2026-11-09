# Feature: quality (v0.4)

Release quality: a UI smoke test, CI, release notes from commits, a What's new section on
the landing page, and the MIT license. No app code changed.

Files: `LICENSE`, `README.md`, `package.json`, `tests/ui-smoke.mjs`,
`.github/workflows/release.yml`, `.github/workflows/ci.yml`, `site/index.html`,
`site/styles.css`, `site/whatsnew.js`.

## What it does

| piece | how |
|---|---|
| `LICENSE` | MIT, "Copyright (c) 2026 LegacyTech". README has a License section and a What's new pointer to Releases. |
| `package.json` | Private, `type: module`, devDependency `playwright-core` pinned to `1.64.0`, script `test:ui`. No lockfile is committed (see contractChanges). |
| `tests/ui-smoke.mjs` | Built-in Node static server on a random port serves `ui/` (`.js` as `text/javascript`). Launches Chromium (`ORBIT_BROWSER=msedge` uses the installed Edge through Playwright's `channel`). Fresh context, 1440x900. For each theme (night, void, signal, daylight, picked by clicking the Settings theme card and checked on `html[data-theme]`): visits all nine views by clicking the sidebar and fails if a view is not active, has no size or no text within 8 s; opens the palette with Ctrl+K, types "perf", expects items, closes with Esc; double-clicks the first process row, expects `#details.is-open` with text, closes with Esc. Any `console.error` or page error fails the run. Prints each step and a summary; exit code 1 on any failure. |
| `ci.yml` | On push and pull_request to `main` and `v0.4`: `ui` job (ubuntu, Node 22, `npm install`, `npx playwright-core install --with-deps chromium`, `npm run test:ui`) and `rust` job (windows, stable, rust-cache, `cargo test` in `src-tauri`). `permissions: contents: read`. |
| `release.yml` | New `test` job (as CI's `ui` job); `release` `needs: test`, checks out with `fetch-depth: 0`, runs `cargo test` before building, then a pwsh step builds the notes: `git describe --tags --abbrev=0 --match 'v*' HEAD^` gives the previous tag, `git log --no-merges --pretty='- %s' prev..HEAD`, drops `- vX.Y scaffold...` lines, keeps at most 50, falls back to "- Maintenance release.". They go to `$GITHUB_OUTPUT` (random heredoc delimiter) and lead `releaseBody`, followed by the existing download and SmartScreen lines, so `latest.json` notes show the changes. |
| `site/` | Nav link and section "04 / What's new" ("Updates itself. Says what changed."). `whatsnew.js` (plain script, `defer`) fetches `api.github.com/repos/HeshW/orbit/releases?per_page=5` and renders a card per release: version (links to its GitHub page), date ("12 Nov 2026"), Pre-release label when set, notes. Everything is HTML-escaped; `- ` / `* ` lines become list items, other lines paragraphs, `**bold**` stays bold. Non-OK response (rate limit 403), network error or bad shape: "Release notes couldn't load right now. See every version on GitHub Releases."; an empty list: "No releases yet." Without JS the initial text links to Releases. Copy: hero meta adds "Updates itself", Privacy says Orbit goes online to check for updates (can be turned off), Install mentions automatic updates and Settings > About, footer links "MIT license". Design: existing `.card`, `.micro`, `.display`, `.lede` classes and tokens; no new colours. |

Note on the browser install: the task said `npx playwright install chromium`. With only
`playwright-core` installed, `npx playwright` fetches the latest `playwright` package, whose
Chromium build can differ from the one `playwright-core 1.64.0` expects. The workflows run the
same installer through `npx playwright-core install --with-deps chromium`, which always
matches the installed version.

## How to test

```
npm install
npx playwright-core install chromium          # or skip it and use Edge:
$env:ORBIT_BROWSER = "msedge"; npm run test:ui
```

Site: serve `site/` over HTTP and open `/#whats-new`.

## Verified

- `npm run test:ui` passes locally with `ORBIT_BROWSER=msedge` and with Playwright's own
  Chromium (already in the local Playwright cache): 4 themes x 9 views, palette, drawer, 0 console errors.
- The test fails when it should: a copy of `ui/` with an injected `console.error` and a
  History view whose `mount` returns early gave 5 failures and exit code 1.
- Both workflow files parse with `js-yaml` 4.1.0 (jobs, `needs: test`, `releaseBody` checked).
  The release-notes PowerShell ran locally against this repo: previous tag v0.3.1, ten
  subjects, the scaffold commit dropped.
- Site in the built-in browser: the live GitHub API returned the four existing releases and
  they render; stubbed responses for a markdown list with HTML in it (escaped, no elements
  created), a 403, a network error and an empty list all render as intended; 375 px wide
  has no horizontal scroll; 0 console errors.
- `node --check` on every `ui/js` file, `site/whatsnew.js` and `tests/ui-smoke.mjs`;
  `cargo build` (no warnings) and `cargo test` (96 passed, 4 ignored).

## Not verified

- The workflows have not run on GitHub (no push allowed): the ubuntu `--with-deps` install,
  the Windows `cargo test` on a clean runner, and the release-notes output reaching
  tauri-action and `latest.json`.
- The site was not deployed.
- No live exe check: this feature changes no app code.
