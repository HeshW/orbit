# Feature: Developer leftovers and the 7-day hold (v1.2, 14b + 14c), Optimize > Cleanup

A third Cleanup panel. The user adds projects folders (E:\Work, for example) with a folder
picker; Orbit lists `node_modules` next to a `package.json` and `target` next to a `Cargo.toml`
that nobody has touched for 30 days, each with its size. Nothing is ticked. **Move to hold**
moves the ticked ones into a hold folder; held items show with **Restore** and **Delete now**
and are deleted for good after 7 days. Shapes and commands: `docs/CONTRACT.md` ("v1.2").

Files: `src-tauri/src/cleanup/devleft.rs` (tests in `cleanup/devleft/tests.rs`),
`ui/js/views/optimize/devleft.js` (mounted by `cleanup.js`), `ui/js/mock/devleft.js`,
`cl-dev-` rules in `ui/css/views/optimize/cleanup.css`. `cleanup.rs`'s `Layout` gained `data`
(Orbit's data folder) and `nogo` (folders this feature never enters).

## Rules (each unit-tested)

| rule | how |
|---|---|
| Manifest pairing | `node_modules` only with a plain `package.json` beside it, `target` only with `Cargo.toml` (`manifest_pairing`). |
| Outermost only | The walk never enters any `node_modules` or a matched `target` (`outermost_only`). |
| Never in Program Files, Windows, AppData, ProgramData | Picked folders and every walked folder are checked against `Layout::nogo`; restore targets too (`protected_roots`). |
| Picked folders pass the root checks | Not a drive root, not a protected folder or its parent, not a link; checked again on the real path from the handle (`protected_roots`). |
| Untouched for 30 days | Newest created/modified time of the build folder's direct children, the project folder, the manifest and lock files (`package-lock.json`, `yarn.lock`, `pnpm-lock.yaml`, `bun.lock(b)`, `Cargo.lock`). Access times are not used; an unreadable time counts as touched (`age_rule`). |
| Ids, never paths | `devleft_scan` keeps what it found; `devleft_hold` maps ids back, and `recheck` repeats every rule before moving (`id_resolution`). |
| No reparse points | Not walked, not listed, not moved (`links_are_never_moved`). |
| Hold, never delete directly | The folder is opened with `FILE_FLAG_OPEN_REPARSE_POINT`, its real path must be inside the picked folder's real path, and it is renamed through that handle (`FileRenameInfo`, no replace) into the hold folder after a volume-serial check (`hold_restore_and_expiry`). |
| No usable hold folder: keep | The item stays where it is and is reported as kept (`unusable_hold_keeps_in_place`). |
| Restore / Delete now only inside a hold | The record only names a hold folder (by drive) and a plain name; the item must be a direct child of that hold folder's real path, checked by handle; links are refused (`refuses_paths_outside_the_hold`). |
| Restore only inside a picked folder, never over a path | Refused when the original is outside every picked folder or anything exists there; both stay (`restore_conflict_and_place`). |
| 7 days | `expire` at start (`devleft::init` in setup) and every 24 h after, using the cleanup's checked delete with the hold folder as root (`hold_restore_and_expiry`). |

Hold folders: `%LOCALAPPDATA%\me.legacytech.orbit\hold` on the system drive, `X:\.orbit-hold`
(hidden) at the root of other drives (`hold_folder_per_drive`). Network shares have none.
Whether a standard user can create `X:\.orbit-hold` depends on the drive's permissions; when it
can't, the item is kept.

Testing: debug builds with `ORBIT_CLEANUP_ROOT` put the data folder at `<root>\OrbitData`; tests
use `Layout::under` and folders under the cargo target dir only.
