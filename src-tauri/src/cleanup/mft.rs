//! Biggest folders from the NTFS master file table (roadmap 15a).
//!
//! Only used when Orbit runs as administrator and the folder is on a local
//! NTFS drive; any error makes the caller fall back to the folder walk. The
//! volume is opened for reading only, and nothing here can write.
//!
//! Records are hand-parsed with bounds-checked reads (`get`), so a malformed
//! record is skipped and never panics. Sizes are space used on disk (the
//! allocated size of each file's unnamed `$DATA`; resident data takes none).
//! Each file record counts once, under the parent of its first Win32 (or
//! POSIX) name, so hard links are not counted twice.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use windows::core::PCWSTR;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{
    GetDriveTypeW, GetVolumeInformationW, GetVolumePathNameW, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE,
};
use windows::Win32::System::Ioctl::{FSCTL_GET_NTFS_VOLUME_DATA, NTFS_VOLUME_DATA_BUFFER};
use windows::Win32::System::IO::DeviceIoControl;

use super::{now_unix, wide, Ctl, FolderEntry, FolderSizes, CACHE_DEPTH, NOT_LOCAL, TOP_FOLDERS};

const ROOT: usize = 5;
/// Records below this are metafiles ($MFT, $Bitmap, ...) or reserved; the walk never lists them.
const FIRST_USER: usize = 24;
const CHUNK: usize = 8 << 20;
const MAX_DEPTH: u32 = 4096;

const IN_USE: u8 = 1;
const DIR: u8 = 2;
const REPARSE: u8 = 4;
const OFFLINE: u8 = 8;

const FILE_ATTRIBUTE_REPARSE: u32 = 0x400;

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o.checked_add(2)?)?.try_into().ok()?))
}
fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o.checked_add(4)?)?.try_into().ok()?))
}
fn u64_at(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(o..o.checked_add(8)?)?.try_into().ok()?))
}

/// Applies the update-sequence fix-ups (one per 512-byte stride). False when
/// the record is torn or the array is out of bounds.
pub(crate) fn fixup(rec: &mut [u8]) -> bool {
    let (Some(off), Some(count)) = (u16_at(rec, 4), u16_at(rec, 6)) else { return false };
    let (off, count) = (off as usize, count as usize);
    if count < 2 || (count - 1) * 512 > rec.len() {
        return false;
    }
    let Some(usn) = u16_at(rec, off) else { return false };
    for i in 1..count {
        let Some(fix) = u16_at(rec, off + i * 2) else { return false };
        let pos = i * 512 - 2;
        if u16_at(rec, pos) != Some(usn) {
            return false;
        }
        let Some(slot) = rec.get_mut(pos..pos + 2) else { return false };
        slot.copy_from_slice(&fix.to_le_bytes());
    }
    true
}

/// A data run: start cluster (None = sparse) and length in clusters.
pub(crate) fn data_runs(mut b: &[u8]) -> Option<Vec<(Option<u64>, u64)>> {
    let mut out = Vec::new();
    let mut lcn: i64 = 0;
    loop {
        let head = *b.first()?;
        if head == 0 {
            return Some(out);
        }
        let (ls, os) = ((head & 0xF) as usize, (head >> 4) as usize);
        if ls == 0 || ls > 8 || os > 8 {
            return None;
        }
        let len_b = b.get(1..1 + ls)?;
        let off_b = b.get(1 + ls..1 + ls + os)?;
        let mut len = [0u8; 8];
        len[..ls].copy_from_slice(len_b);
        let len = u64::from_le_bytes(len);
        if os == 0 {
            out.push((None, len));
        } else {
            let fill = if off_b[os - 1] & 0x80 != 0 { 0xFF } else { 0 };
            let mut off = [fill; 8];
            off[..os].copy_from_slice(off_b);
            lcn = lcn.checked_add(i64::from_le_bytes(off))?;
            out.push((Some(u64::try_from(lcn).ok()?), len));
        }
        b = b.get(1 + ls + os..)?;
    }
}

#[derive(Default, Debug)]
pub(crate) struct Rec {
    pub base: u64,
    pub seq: u16,
    pub flags: u8,
    /// (parent ref, namespace rank, name). Rank 2 = Win32 or Win32+DOS, 1 = POSIX.
    pub names: Vec<(u64, u8, String)>,
    /// Allocated bytes of the unnamed `$DATA`, from its first segment.
    pub alloc: Option<u64>,
    /// Runs of the unnamed `$DATA` (any segment), for reading `$MFT` itself.
    pub runs: Vec<(Option<u64>, u64)>,
    pub data_size: u64,
    pub has_list: bool,
}

/// Parses one fixed-up record. None when it is not a valid record.
pub(crate) fn parse(rec: &[u8]) -> Option<Rec> {
    if rec.get(0..4)? != b"FILE" {
        return None;
    }
    let hflags = u16_at(rec, 0x16)?;
    let mut r = Rec { seq: u16_at(rec, 0x10)?, base: u64_at(rec, 0x20)? & 0xFFFF_FFFF_FFFF, ..Default::default() };
    if hflags & 1 != 0 {
        r.flags |= IN_USE;
    }
    if hflags & 2 != 0 {
        r.flags |= DIR;
    }
    let used = (u32_at(rec, 0x18)? as usize).min(rec.len());
    let mut at = u16_at(rec, 0x14)? as usize;
    while at + 8 <= used {
        let kind = u32_at(rec, at)?;
        if kind == 0xFFFF_FFFF {
            break;
        }
        let len = u32_at(rec, at + 4)? as usize;
        if len < 16 || at + len > used {
            return None;
        }
        let a = rec.get(at..at + len)?;
        let non_res = *a.get(8)? != 0;
        let named = *a.get(9)? != 0;
        let aflags = u16_at(a, 0xC)?;
        let value = || -> Option<&[u8]> {
            let vl = u32_at(a, 0x10)? as usize;
            let vo = u16_at(a, 0x14)? as usize;
            a.get(vo..vo.checked_add(vl)?)
        };
        match kind {
            0x10 if !non_res => {
                let attrs = u32_at(value()?, 0x20)?;
                if attrs & FILE_ATTRIBUTE_REPARSE != 0 {
                    r.flags |= REPARSE;
                }
                if attrs & NOT_LOCAL != 0 {
                    r.flags |= OFFLINE;
                }
            }
            0x20 => r.has_list = true,
            0x30 if !non_res => {
                let v = value()?;
                let parent = u64_at(v, 0)?;
                let n = *v.get(0x40)? as usize;
                let rank = match *v.get(0x41)? {
                    1 | 3 => 2,
                    0 => 1,
                    _ => 0, // pure DOS 8.3 name
                };
                if rank > 0 {
                    let units: Vec<u16> =
                        v.get(0x42..0x42 + n * 2)?.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                    r.names.push((parent, rank, String::from_utf16_lossy(&units)));
                }
            }
            0x80 if !named => {
                if non_res {
                    let lowest = u64_at(a, 0x10)?;
                    if lowest == 0 {
                        // Compressed or sparse: the total actually allocated.
                        let total = aflags & 0x8001 != 0 && u16_at(a, 0x22)? != 0 && len >= 0x48;
                        r.alloc = Some(if total { u64_at(a, 0x40)? } else { u64_at(a, 0x28)? });
                        r.data_size = u64_at(a, 0x30)?;
                    }
                    let ro = u16_at(a, 0x20)? as usize;
                    r.runs.extend(data_runs(a.get(ro..)?)?);
                } else {
                    r.alloc = Some(0);
                }
            }
            0xC0 => r.flags |= REPARSE,
            _ => {}
        }
        at += len;
    }
    Some(r)
}

/// Every record of the table, merged (extension records into their base).
#[derive(Default)]
pub(crate) struct Table {
    seq: Vec<u16>,
    flags: Vec<u8>,
    parent: Vec<u64>,
    rank: Vec<u8>,
    /// Index into `names` (start, len).
    name: Vec<(u32, u32)>,
    names: String,
    bytes: Vec<u64>,
    kids: Vec<Vec<u32>>,
}

impl Table {
    fn grow(&mut self, i: usize) {
        if i >= self.seq.len() {
            let n = i + 1;
            self.seq.resize(n, 0);
            self.flags.resize(n, 0);
            self.parent.resize(n, u64::MAX);
            self.rank.resize(n, 0);
            self.name.resize(n, (0, 0));
            self.bytes.resize(n, 0);
        }
    }

    /// Adds record `no` (raw bytes, fix-ups not yet applied). Bad records are skipped.
    pub(crate) fn add(&mut self, no: usize, raw: &mut [u8]) {
        if !fixup(raw) {
            return;
        }
        let Some(r) = parse(raw) else { return };
        if r.flags & IN_USE == 0 {
            return;
        }
        let target = if r.base != 0 {
            match usize::try_from(r.base) {
                Ok(b) if b != no => b,
                _ => return,
            }
        } else {
            no
        };
        if target > u32::MAX as usize - 1 {
            return;
        }
        self.grow(target);
        if r.base == 0 {
            self.seq[target] = r.seq;
            self.flags[target] |= r.flags;
        }
        if let Some(a) = r.alloc {
            self.bytes[target] = a;
        }
        for (parent, rank, n) in r.names {
            if rank > self.rank[target] {
                self.rank[target] = rank;
                self.parent[target] = parent;
                let (Ok(s), Ok(l)) = (u32::try_from(self.names.len()), u32::try_from(n.len())) else { continue };
                self.names.push_str(&n);
                self.name[target] = (s, l);
            }
        }
    }

    fn name_of(&self, i: usize) -> &str {
        let (s, l) = self.name.get(i).copied().unwrap_or((0, 0));
        self.names.get(s as usize..(s as usize + l as usize)).unwrap_or("")
    }

    fn is(&self, i: usize, f: u8) -> bool {
        self.flags.get(i).is_some_and(|x| x & f != 0)
    }

    /// Builds parent -> children. Unreachable and orphaned records drop out.
    pub(crate) fn finish(&mut self) {
        let n = self.seq.len();
        let mut kids = vec![Vec::new(); n];
        for i in FIRST_USER..n {
            if !self.is(i, IN_USE) || self.rank[i] == 0 {
                continue;
            }
            let pref = self.parent[i];
            let p = (pref & 0xFFFF_FFFF_FFFF) as usize;
            let pseq = (pref >> 48) as u16;
            if p == i || p >= n || !self.is(p, IN_USE) || !self.is(p, DIR) || (p != ROOT && p < FIRST_USER) {
                continue;
            }
            if pseq != 0 && self.seq[p] != pseq {
                continue; // stale reference to a reused record
            }
            kids[p].push(i as u32);
        }
        self.kids = kids;
    }

    /// The record of `parts` below the root, following folders only.
    fn find(&self, parts: &[String]) -> Option<usize> {
        if !self.is(ROOT, IN_USE) {
            return None;
        }
        let mut at = ROOT;
        for part in parts {
            let want = part.to_lowercase();
            at = *self.kids.get(at)?.iter().find(|&&k| {
                let k = k as usize;
                self.is(k, DIR) && self.name_of(k).to_lowercase() == want
            })? as usize;
            if self.is(at, REPARSE) {
                return None;
            }
        }
        Some(at)
    }

    fn measure(&self, at: usize, dir: &Path, depth: u32, ctl: &mut Ctl<'_>, cache: &mut Vec<FolderSizes>) -> (u64, u64) {
        let (mut bytes, mut files) = (0u64, 0u64);
        let keep = depth <= CACHE_DEPTH;
        let mut here = FolderSizes { path: dir.to_string_lossy().into_owned(), ..Default::default() };
        let mut kids: Vec<FolderEntry> = Vec::new();
        for &k in self.kids.get(at).map(Vec::as_slice).unwrap_or(&[]) {
            if ctl.cancelled() {
                return (bytes, files);
            }
            let k = k as usize;
            if self.is(k, DIR) {
                if self.is(k, REPARSE) || depth >= MAX_DEPTH {
                    here.links += 1;
                    continue;
                }
                let name = self.name_of(k);
                let path = dir.join(name);
                if depth == 0 {
                    ctl.p.label = name.to_string();
                }
                let (b, f) = self.measure(k, &path, depth + 1, ctl, cache);
                bytes += b;
                files += f;
                if keep {
                    kids.push(FolderEntry {
                        name: name.to_string(),
                        path: path.to_string_lossy().into_owned(),
                        bytes: b,
                        files: f,
                        unreadable: 0,
                    });
                }
            } else {
                let size = if self.is(k, OFFLINE) { 0 } else { self.bytes[k] };
                bytes += size;
                files += 1;
                here.files_here += 1;
                here.files_here_bytes += size;
                ctl.add(1, size);
            }
        }
        if keep {
            kids.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
            if kids.len() > TOP_FOLDERS {
                for k in kids.drain(TOP_FOLDERS..) {
                    here.other_bytes += k.bytes;
                    here.other_count += 1;
                }
            }
            here.folders = kids;
            here.bytes = bytes;
            here.files = files;
            here.scanned_at = now_unix();
            cache.push(here);
        }
        (bytes, files)
    }

    /// Same shape as `cleanup::folder_sizes` for `dir`, whose path below the volume root is `parts`.
    pub(crate) fn sizes(&self, dir: &Path, parts: &[String], ctl: &mut Ctl<'_>) -> Result<(FolderSizes, Vec<FolderSizes>), String> {
        let at = self.find(parts).ok_or("Folder not found in the file table.")?;
        let mut cache = Vec::new();
        self.measure(at, dir, 0, ctl, &mut cache);
        let cancelled = ctl.cancelled();
        ctl.finish();
        if cancelled {
            let blank = FolderSizes {
                path: dir.to_string_lossy().into_owned(),
                cancelled: true,
                scanned_at: now_unix(),
                ..Default::default()
            };
            return Ok((blank, Vec::new()));
        }
        let top = cache.pop().ok_or("No result.")?;
        Ok((top, cache))
    }
}

/// Splits records out of the `$MFT` byte stream as it is read.
pub(crate) struct Feed {
    rec_size: usize,
    buf: Vec<u8>,
    next: usize,
    pub table: Table,
}

impl Feed {
    pub(crate) fn new(rec_size: usize) -> Self {
        Feed { rec_size, buf: Vec::new(), next: 0, table: Table::default() }
    }

    pub(crate) fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
        let whole = self.buf.len() / self.rec_size * self.rec_size;
        let mut taken = std::mem::take(&mut self.buf);
        for rec in taken[..whole].chunks_exact_mut(self.rec_size) {
            self.table.add(self.next, rec);
            self.next += 1;
        }
        self.buf = taken.split_off(whole);
    }
}

// ---------------------------------------------------------------------------
// The live volume (not unit-tested: needs administrator)
// ---------------------------------------------------------------------------

/// "C:" and the folder names below the root, when `dir` is on a local NTFS drive.
fn volume_of(dir: &Path) -> Result<(String, Vec<String>), String> {
    let s = dir.to_string_lossy().into_owned();
    let w = wide(&s);
    let mut vol = [0u16; 300];
    // SAFETY: NUL-terminated input; the output slice is writable.
    unsafe { GetVolumePathNameW(PCWSTR(w.as_ptr()), &mut vol) }.map_err(|e| e.to_string())?;
    let end = vol.iter().position(|&c| c == 0).unwrap_or(vol.len());
    let root = String::from_utf16_lossy(&vol[..end]);
    let b = root.as_bytes();
    if root.len() != 3 || !b[0].is_ascii_alphabetic() || &b[1..] != b":\\" {
        return Err("Not a drive letter.".into());
    }
    let rw = wide(&root);
    // SAFETY: NUL-terminated root path.
    let kind = unsafe { GetDriveTypeW(PCWSTR(rw.as_ptr())) };
    if kind != 2 && kind != 3 {
        return Err("Not a local drive.".into()); // DRIVE_REMOVABLE, DRIVE_FIXED
    }
    let mut fs_name = [0u16; 32];
    // SAFETY: NUL-terminated root path; writable buffer.
    unsafe { GetVolumeInformationW(PCWSTR(rw.as_ptr()), None, None, None, None, Some(&mut fs_name)) }
        .map_err(|e| e.to_string())?;
    let end = fs_name.iter().position(|&c| c == 0).unwrap_or(fs_name.len());
    if String::from_utf16_lossy(&fs_name[..end]) != "NTFS" {
        return Err("Not NTFS.".into());
    }
    let rest = s.get(3..).filter(|_| s.get(..3).is_some_and(|p| p.eq_ignore_ascii_case(&root))).ok_or("Path outside its volume.")?;
    let parts = rest.split('\\').filter(|p| !p.is_empty()).map(str::to_string).collect();
    Ok((root[..2].to_string(), parts))
}

fn read_at(f: &mut File, off: u64, buf: &mut [u8]) -> Result<(), String> {
    f.seek(SeekFrom::Start(off)).map_err(|e| e.to_string())?;
    f.read_exact(buf).map_err(|e| e.to_string())
}

fn read_table(letter: &str, ctl: &mut Ctl<'_>) -> Result<Option<Table>, String> {
    let mut f = OpenOptions::new()
        .read(true)
        .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0)
        .open(format!(r"\\.\{letter}"))
        .map_err(|e| e.to_string())?;
    let mut vd = NTFS_VOLUME_DATA_BUFFER::default();
    let mut got = 0u32;
    // SAFETY: valid handle; output buffer is a properly sized struct.
    unsafe {
        DeviceIoControl(
            HANDLE(f.as_raw_handle()),
            FSCTL_GET_NTFS_VOLUME_DATA,
            None,
            0,
            Some(&mut vd as *mut _ as *mut core::ffi::c_void),
            std::mem::size_of::<NTFS_VOLUME_DATA_BUFFER>() as u32,
            Some(&mut got),
            None,
        )
    }
    .map_err(|e| e.to_string())?;
    let rec = vd.BytesPerFileRecordSegment as usize;
    let cluster = vd.BytesPerCluster as u64;
    let valid = u64::try_from(vd.MftValidDataLength).map_err(|_| "Bad MFT length.")?;
    if !(512..=65536).contains(&rec) || rec % 512 != 0 || cluster == 0 || cluster % 512 != 0 {
        return Err("Unexpected volume geometry.".into());
    }
    let start = u64::try_from(vd.MftStartLcn).map_err(|_| "Bad MFT start.")?;
    let mut first = vec![0u8; rec.max(cluster as usize)];
    read_at(&mut f, start * cluster, &mut first)?;
    let mut r0 = first[..rec].to_vec();
    if !fixup(&mut r0) {
        return Err("Record 0 is torn.".into());
    }
    let r0 = parse(&r0).ok_or("Record 0 is unreadable.")?;
    let covered: u64 = r0.runs.iter().map(|r| r.1).sum::<u64>() * cluster;
    if r0.has_list && covered < valid {
        return Err("$MFT spills into extension records.".into());
    }
    let mut feed = Feed::new(rec);
    let mut left = valid;
    let mut buf = vec![0u8; CHUNK];
    for (lcn, len) in r0.runs {
        let lcn = lcn.ok_or("Sparse $MFT run.")?;
        let mut off = lcn * cluster;
        let mut run_left = (len * cluster).min(left);
        while run_left > 0 {
            if ctl.cancelled() {
                return Ok(None);
            }
            let n = (run_left as usize).min(CHUNK);
            // Whole sectors only: raw volume reads must be sector-sized.
            let n_read = n.div_ceil(512) * 512;
            let b = buf.get_mut(..n_read).ok_or("Chunk size.")?;
            read_at(&mut f, off, b)?;
            feed.push(&b[..n]);
            off += n as u64;
            run_left -= n as u64;
            left -= n as u64;
        }
        if left == 0 {
            break;
        }
    }
    let mut t = feed.table;
    t.finish();
    Ok(Some(t))
}

/// Biggest folders from the file table. Err means "use the folder walk".
pub fn folder_sizes(dir: &Path, ctl: &mut Ctl<'_>) -> Result<(FolderSizes, Vec<FolderSizes>), String> {
    let (letter, parts) = volume_of(dir)?;
    let label = std::mem::take(&mut ctl.p.label);
    ctl.p.label = "Reading the file table".to_string();
    ctl.emit(true);
    let Some(table) = read_table(&letter, ctl)? else {
        ctl.finish();
        let blank = FolderSizes { path: dir.to_string_lossy().into_owned(), cancelled: true, scanned_at: now_unix(), ..Default::default() };
        return Ok((blank, Vec::new()));
    };
    ctl.p.label = label;
    table.sizes(dir, &parts, ctl)
}

#[cfg(test)]
#[path = "mft_tests.rs"]
mod tests;
