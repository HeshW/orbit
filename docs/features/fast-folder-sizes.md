# Feature: fast folder sizes from the NTFS file table (v1.3, 15a)

Biggest folders (Optimize > Cleanup) reads the drive's master file table (MFT) in one pass
instead of opening every folder, when Orbit runs as administrator and the folder is on a local
NTFS drive letter. Otherwise, or on any error, it uses the folder walk as before. The result has
the same shape either way; `method` says which ran (see `docs/CONTRACT.md` "v1.3").

Files: `src-tauri/src/cleanup/mft.rs` (reader and parser), `cleanup/mft_tests.rs` (byte
fixtures), the `cleanup_folder_sizes` command in `cleanup.rs`, two lines in
`ui/js/views/optimize/cleanup.js`, and `ui/js/mock/cleanup.js`.

## How it works

1. `GetVolumePathNameW` must give a drive root (`C:\`), `GetDriveTypeW` fixed or removable,
   and `GetVolumeInformationW` "NTFS". ReFS, FAT, exFAT, network drives and folders on a
   volume mounted in a folder use the walk.
2. `\.\C:` is opened with read access only (shares read, write, delete). Nothing in the
   module can write. `FSCTL_GET_NTFS_VOLUME_DATA` gives the MFT start, record size, cluster
   size and the MFT's valid length.
3. Record 0 (`$MFT` itself) gives the data runs, so a fragmented MFT is read piece by piece,
   in 8 MB reads. If `$MFT`'s own runs spill into extension records, Orbit uses the walk.
4. Each record is fixed up (update sequence), parsed with bounds-checked reads, and merged:
   extension records add their names and data to their base record.
5. Children are grouped by parent, then the requested folder is measured exactly like the
   walk (top 40 folders, other folders summed, files in this folder, two levels kept for
   drill-down).

## Rules

- Hand-parsed, not the `mft` crate: that crate pulls in chrono, serde_json, encoding, lru and
  more for attributes Orbit doesn't need, and its parser assumes well-formed input. Orbit
  needs five attributes; each read uses `get`, so a bad record is skipped, never a panic.
- Sizes are space used on disk: the allocated size of the unnamed `$DATA` (for compressed or
  sparse files, the total actually allocated). Small files stored inside their record count 0.
  Cloud placeholders and offline files count 0, like the walk. Alternate data streams are not
  counted.
- Names: Win32 (or Win32 and DOS) names first, then POSIX; DOS-only short names are ignored.
- Hard links count once, under the folder of the record's first Win32 name. (The walk counts
  a hard-linked file in every folder that holds it.)
- Folders that are reparse points (junctions, symlinks, cloud folders) are counted as links
  and not followed, like the walk.
- Skipped: records not in use, torn records, the root's self-reference, metafiles and reserved
  records (0-23 except the root), everything under `$Extend`, and records whose parent
  reference is stale (sequence number mismatch).
- Unlike the walk, nothing is unreadable, so `unreadable` is 0 and folders such as
  `System Volume Information` are included.
- Cancel works during the read and the measure; a cancelled scan does not fall back.
- Nothing is sized from an unchecked disk value: record numbers past the MFT's valid length
  are skipped, and every multiply or add on disk values is checked (overflow means the walk).
- Extension records are merged only when their base reference's sequence number matches the
  base record, so a reused record doesn't pick up an old file's pieces.
- The measure uses an explicit stack, so a very deep folder chain can't overflow the thread.
- Totals guard: for a drive root, the total must be within 10% (plus the MFT's own size) of the
  drive's used space (`GetDiskFreeSpaceExW`); for a folder, of a cached walk of the same
  folder when there is one. Otherwise the walk's result is used.
- The drill-down cache holds one method at a time: storing a result drops cached answers of
  the other method.

## Tests

`cargo test cleanup::mft` builds record bytes in memory: fix-ups and torn records, a
fragmented run list with a negative offset, a sparse file, a file whose name and data live in
an extension record (and a later segment that must not change its size), a hard link, a
junction, a DOS-only name, a stale parent, garbage and out-of-bounds attributes. One test
builds the same tree on disk and as records and checks that the walk and the file table give
identical results. The administrator path on a real drive is not unit-tested (it needs
elevation); measuring speed on the owner's PC is still **(unverified)**.
