//! The Windows side of the name index: which volumes to index, and the three
//! read-only volume calls (FSCTL_QUERY_USN_JOURNAL, FSCTL_ENUM_USN_DATA,
//! FSCTL_READ_USN_JOURNAL). Orbit never creates, changes or deletes a journal.

use std::fs::{File, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;

use windows::core::PCWSTR;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{
    GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
};
use windows::Win32::System::Ioctl::{
    FSCTL_ENUM_USN_DATA, FSCTL_GET_NTFS_VOLUME_DATA, FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_USN_JOURNAL, MFT_ENUM_DATA_V0,
    NTFS_VOLUME_DATA_BUFFER, READ_USN_JOURNAL_DATA_V0, USN_JOURNAL_DATA_V0,
};
use windows::Win32::System::IO::DeviceIoControl;

use super::index::{
    parse_records, reply_next, JournalInfo, Record, REASON_FILE_CREATE, REASON_FILE_DELETE, REASON_RENAME_NEW_NAME,
    REASON_RENAME_OLD_NAME,
};

const BUF: usize = 1 << 20;
const ERROR_HANDLE_EOF: i32 = 38;
const ERROR_JOURNAL_DELETE_IN_PROGRESS: i32 = 1178;
const ERROR_JOURNAL_NOT_ACTIVE: i32 = 1179;
const ERROR_JOURNAL_ENTRY_DELETED: i32 = 1181;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn win32_code(e: &windows::core::Error) -> i32 {
    // HRESULT_FROM_WIN32: 0x8007xxxx.
    e.code().0 & 0xFFFF
}

/// Fixed (not removable or network) NTFS drive letters: ["C:", "D:"].
pub fn fixed_ntfs_volumes() -> Vec<String> {
    // SAFETY: no arguments.
    let mask = unsafe { GetLogicalDrives() };
    let mut out = Vec::new();
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let root = format!("{}:\\", (b'A' + i as u8) as char);
        let rw = wide(&root);
        // SAFETY: NUL-terminated root path.
        if unsafe { GetDriveTypeW(PCWSTR(rw.as_ptr())) } != 3 {
            continue; // DRIVE_FIXED only
        }
        let mut fs = [0u16; 32];
        // SAFETY: NUL-terminated root; writable buffer.
        if unsafe { GetVolumeInformationW(PCWSTR(rw.as_ptr()), None, None, None, None, Some(&mut fs)) }.is_err() {
            continue;
        }
        let end = fs.iter().position(|&c| c == 0).unwrap_or(fs.len());
        if String::from_utf16_lossy(&fs[..end]) == "NTFS" {
            out.push(root[..2].to_string());
        }
    }
    out
}

/// Opens the volume for reading only (needs administrator).
pub fn open_volume(drive: &str) -> Result<File, String> {
    OpenOptions::new()
        .read(true)
        .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0)
        .open(format!(r"\\.\{drive}"))
        .map_err(|e| e.to_string())
}

fn ioctl(f: &File, code: u32, input: &[u8], output: &mut [u8]) -> windows::core::Result<usize> {
    let mut got = 0u32;
    // SAFETY: valid handle; both buffers live for the call and their lengths are passed.
    unsafe {
        DeviceIoControl(
            HANDLE(f.as_raw_handle()),
            code,
            Some(input.as_ptr().cast()),
            input.len() as u32,
            Some(output.as_mut_ptr().cast()),
            output.len() as u32,
            Some(&mut got),
            None,
        )
    }?;
    Ok((got as usize).min(output.len()))
}

fn as_bytes<T: Copy>(v: &T) -> &[u8] {
    // SAFETY: T is a plain repr(C) Copy struct from the windows crate.
    unsafe { std::slice::from_raw_parts((v as *const T).cast::<u8>(), std::mem::size_of::<T>()) }
}

/// The volume's journal, or None when no journal is active.
pub fn query_journal(f: &File) -> Result<Option<JournalInfo>, String> {
    let mut out = vec![0u8; std::mem::size_of::<USN_JOURNAL_DATA_V0>() + 64];
    match ioctl(f, FSCTL_QUERY_USN_JOURNAL, &[], &mut out) {
        Ok(n) if n >= std::mem::size_of::<USN_JOURNAL_DATA_V0>() => {
            // SAFETY: n bytes were written, enough for the struct; read unaligned.
            let j: USN_JOURNAL_DATA_V0 = unsafe { std::ptr::read_unaligned(out.as_ptr().cast()) };
            Ok(Some(JournalInfo { id: j.UsnJournalID, first_usn: j.FirstUsn, next_usn: j.NextUsn, lowest_valid_usn: j.LowestValidUsn }))
        }
        Ok(_) => Err("Short journal reply.".into()),
        Err(e) if matches!(win32_code(&e), ERROR_JOURNAL_NOT_ACTIVE | ERROR_JOURNAL_DELETE_IN_PROGRESS) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// About how many file-table records the volume has (for progress).
pub fn record_estimate(f: &File) -> u64 {
    let mut out = vec![0u8; std::mem::size_of::<NTFS_VOLUME_DATA_BUFFER>()];
    match ioctl(f, FSCTL_GET_NTFS_VOLUME_DATA, &[], &mut out) {
        Ok(n) if n >= out.len() => {
            // SAFETY: the whole struct was written; read unaligned.
            let vd: NTFS_VOLUME_DATA_BUFFER = unsafe { std::ptr::read_unaligned(out.as_ptr().cast()) };
            let rec = u64::from(vd.BytesPerFileRecordSegment).max(1);
            (vd.MftValidDataLength.max(0) as u64) / rec
        }
        _ => 0,
    }
}

/// Every file and folder on the volume, in batches. `batch` returns false to stop.
pub fn enumerate(f: &File, high_usn: i64, mut batch: impl FnMut(&[Record]) -> bool) -> Result<(), String> {
    let mut buf = vec![0u8; BUF];
    let mut start = 0u64;
    loop {
        let input = MFT_ENUM_DATA_V0 { StartFileReferenceNumber: start, LowUsn: 0, HighUsn: high_usn };
        let n = match ioctl(f, FSCTL_ENUM_USN_DATA, as_bytes(&input), &mut buf) {
            Ok(n) => n,
            Err(e) if win32_code(&e) == ERROR_HANDLE_EOF => return Ok(()),
            Err(e) => return Err(e.to_string()),
        };
        let Some(next) = buf.get(..n).and_then(reply_next) else { return Ok(()) };
        let records = parse_records(&buf[..n]);
        if !batch(&records) {
            return Ok(());
        }
        if next <= start {
            return Ok(()); // no progress: stop rather than loop
        }
        start = next;
    }
}

pub enum ReadError {
    /// The journal moved past `start` or was replaced: read the volume again.
    Gone,
    Other(String),
}

/// Journal records from `start` to now, and the USN to continue from.
pub fn read_journal(f: &File, journal_id: u64, start: i64) -> Result<(Vec<Record>, i64), ReadError> {
    let mut buf = vec![0u8; BUF];
    let mut out = Vec::new();
    let mut usn = start;
    for _ in 0..64 {
        let input = READ_USN_JOURNAL_DATA_V0 {
            StartUsn: usn,
            ReasonMask: REASON_FILE_CREATE | REASON_FILE_DELETE | REASON_RENAME_NEW_NAME | REASON_RENAME_OLD_NAME,
            ReturnOnlyOnClose: 0,
            Timeout: 0,
            BytesToWaitFor: 0,
            UsnJournalID: journal_id,
        };
        let n = match ioctl(f, FSCTL_READ_USN_JOURNAL, as_bytes(&input), &mut buf) {
            Ok(n) => n,
            Err(e) => {
                return Err(match win32_code(&e) {
                    ERROR_JOURNAL_ENTRY_DELETED | ERROR_JOURNAL_NOT_ACTIVE | ERROR_JOURNAL_DELETE_IN_PROGRESS => ReadError::Gone,
                    _ => ReadError::Other(e.to_string()),
                })
            }
        };
        let Some(next) = buf.get(..n).and_then(reply_next) else { break };
        out.extend(parse_records(&buf[..n]));
        let next = next as i64;
        if next <= usn {
            break;
        }
        usn = next;
        if n <= 8 {
            break;
        }
    }
    Ok((out, usn))
}
