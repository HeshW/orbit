# Feature: file search in the palette (v1.4, section 16)

Type part of a file or folder name in the Ctrl+K palette and matching files show in a
"Files" group after views, actions and processes, with their folder. Enter opens the file,
Ctrl+Enter shows it in File Explorer. Text typed on the wrong keyboard layout still matches,
for the whole palette ("ؤاقخةث" on an Arabic layout finds "chrome").

Files: `src-tauri/src/search.rs` (commands, state, build and follow threads),
`search/index.rs` (the index, record parsing, journal apply, ranking, result ids, layout
maps; no Windows calls), `search/usn.rs` (volume calls), `search/wsearch.rs` (Windows
Search), `search/launch.rs` (opening as the user), `search/tests.rs`; `ui/js/palette.js`,
`ui/js/layouts.js`, `ui/js/mock/search.js`, one fact in `ui/js/views/settings.js`.

## Two routes

**As administrator: Orbit's own name index.** The first file query starts a background
thread that indexes every fixed NTFS drive letter: `\\.\C:` opened read only,
`FSCTL_QUERY_USN_JOURNAL` for the journal id and position, then `FSCTL_ENUM_USN_DATA` to list
every record (file reference, parent reference, attributes, name). The raw file-table reader
from 15a (`cleanup/mft.rs`) is not used. While it runs the palette shows "Indexing C: 42%".

Each record takes one 12-byte slot (parent record, offset and length into one shared block of
UTF-8 name bytes, folder flag), indexed by record number; no string per file. Renames leave
old name bytes behind; the block is rebuilt when more than half (and at least 1 MB) is dead.

After the build a second thread reads `FSCTL_READ_USN_JOURNAL` every 2 seconds and applies
creates, deletes, renames and moves. It reads the drive again when the journal id changed,
when Orbit's saved position is older than the journal's first or lowest valid USN, or when a
read says the entries were deleted. A drive with no active journal is not followed and is read
again on a query at most every 10 minutes. Orbit never creates, changes or deletes a journal.

**Without administrator: Windows Search.** One read-only OLE DB query against
`Search.CollatorDSO`: `SELECT TOP 400 System.ItemPathDisplay, System.FileAttributes FROM
SystemIndex WHERE SCOPE='file:' AND System.FileName LIKE '%text%'` (quotes and LIKE wildcards
are stripped from the text). It runs on its own thread; Orbit waits at most 2 seconds and stops
reading rows when a newer query starts. Rows are re-ranked with the same ranking as the index.
It covers only what Windows Search indexes (usually the user's folders and Start menu).
If the WSearch service isn't running the palette shows one line, "File search needs Windows
Search, or Restart as administrator", which restarts Orbit as administrator.

The shell's `search-ms:` folder (`SHCreateItemFromParsingName` + `IEnumShellItems`) was tried
first: enumerated from Rust it returned no items, so it is not used.

## Matching

Case-insensitive on names only. Tiers: exact, prefix, word start, contains, initials ("vsc"
for "Visual Studio Code", also camelCase), then in-order letters (3+ letters), shorter names
first within a tier. The best 50 are kept with a fixed-size heap and get their full path by
walking parents iteratively (at most 256 levels; a loop or a missing parent drops the result).

Every `file_search` call bumps a generation number; a scan checks it every 32k names and
returns `stale: true` when a newer query started. The palette waits 120 ms after typing,
ignores replies to older requests and polls while the index builds.

## Opening

Rust keeps the paths behind the latest reply; the UI sends only a result `id`. An id from an
older reply, or out of range, is refused ("That result is out of date. Search again.").

When Orbit is elevated it never starts the file itself (it would run as administrator). It
asks the desktop's Explorer: `IShellWindows::FindWindowSW(SWC_DESKTOP)` -> `IServiceProvider`
-> `IShellBrowser` -> active `IShellView` -> `IShellFolderViewDual::Application` ->
`IShellDispatch2::ShellExecute`. If that fails it shows the file with `explorer.exe /select`
through the same route; if Explorer can't be reached it reports an error and starts nothing.
Not elevated, it uses `ShellExecuteW` and `explorer.exe /select` directly.

## Wrong keyboard layout

`keyboard_layouts` reads `GetKeyboardLayoutList`, then for each layout every virtual key in
base and Shift state with `ToUnicodeEx` (flag 4, so dead-key state is untouched; numeric
keypad skipped). It returns, for each ordered pair of layouts, the printable characters that
differ: `[{ from, to, map: { "ؤ": "c" } }]`. Each key's character is looked up directly rather
than through `VkKeyScanExW`: walking the keys once gives the same table for every character.

`ui/js/layouts.js` has the pure parts: `convertLayout(text, map)` and
`layoutVariants(text, maps)` (only maps that know every non-space character, no duplicates).
When the typed text finds fewer than 3 palette items, the variants are matched too and merged;
file search tries the variants when the text as typed finds no files.

## Rules

- The index lives in memory only and is dropped when Orbit closes. Nothing is written to disk.
- Results never leave the PC. No service, no background process after Orbit exits.
- Volumes are opened read only; the journal is only queried and read.
- Every reply buffer is parsed with bounds-checked reads: a record length of 0, under 60
  bytes, or past the end stops parsing; unknown versions and names outside their record are
  skipped; record numbers past 2^28 are ignored. No `unwrap` on disk data, no recursion.

## Settings > About

"File index" shows the index's memory and file count, "Building", "Not built", or that file
search uses Windows Search. It comes from `file_search_status`.

## Tests

- Rust (`search/tests.rs`): V2/V3 record parsing, zero-length, truncated and random buffers,
  journal create/rename/move/delete, reindex triggers, paths with cycles, missing parents and
  depth, ranking tiers, top-k and cancel, result-id validation, layout maps. No live volume.
- `search/wsearch.rs` has one `#[ignore]` test that asks the real Windows Search index.
- UI smoke (`tests/ui-smoke.mjs`, "file search"): file results with folders, Enter and
  Ctrl+Enter call `file_search_open` with `reveal` false/true, Arabic text finds chrome.exe,
  `convertLayout` with a mocked map, the Windows Search line, the Indexing line, About.
