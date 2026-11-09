//! Tools: find which processes lock a file or the files in a folder (Restart Manager).
//!
//! Restart Manager answers "who has this file open" for the current user without admin
//! rights: start a session, register the files, read the affected processes with the
//! two-call `RmGetList` pattern, end the session. A folder covers the files inside it
//! (recursively, at most `MAX_FOLDER_FILES`): each file is asked for its users directly
//! on several threads, then Restart Manager describes those processes (see
//! `find_lockers`).

use std::collections::HashMap;
use std::ffi::c_void;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS, FILETIME, WIN32_ERROR};
use windows::Win32::System::RestartManager::{
    RmConsole, RmCritical, RmEndSession, RmExplorer, RmGetList, RmMainWindow, RmOtherWindow,
    RmRegisterResources, RmService, RmStartSession, CCH_RM_SESSION_KEY, RM_APP_TYPE,
    RM_PROCESS_INFO, RM_UNIQUE_PROCESS,
};
use windows::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION;

use crate::actions::{handle_creation_filetime, image_path, open_process, snapshot_processes};

/// A process that has the file open. `app_type`: "app" | "service" |
/// "explorer" | "console" | "critical" | "unknown".
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Locker {
    pub pid: u32,
    pub name: String,
    pub exe: Option<String>,
    pub app_type: String,
}

/// Files checked at most in a folder (about 20 s for 2,000).
pub const MAX_FOLDER_FILES: usize = 2000;
/// Processes registered per Restart Manager session when describing a folder's users.
const APPS_PER_SESSION: usize = 16;
/// Threads that ask Windows about the files of a folder.
const PROBE_THREADS: usize = 32;
/// Files Orbit could not check itself that still go to Restart Manager (80 ms each).
const MAX_RM_FILES: usize = 25;
/// Files per `RmRegisterResources` call (calls add up within one session).
const REGISTER_BATCH: usize = 500;
/// How deep a folder walk goes.
const MAX_DEPTH: usize = 32;

const NOT_FOUND: &str = "Orbit can't find that file or folder. Check the path and try again.";
const EMPTY_PATH: &str = "Type the path of a file or folder.";
const NOT_ABSOLUTE: &str = "Type the full path, starting with a drive letter like C:\\.";

#[tauri::command]
pub async fn find_file_lockers(path: String) -> Result<Vec<Locker>, String> {
    tauri::async_runtime::spawn_blocking(move || find_lockers(&path))
        .await
        .map_err(|e| e.to_string())?
}

/// The blocking work behind `find_file_lockers`.
pub fn find_lockers(raw: &str) -> Result<Vec<Locker>, String> {
    let path = clean_path(raw)?;
    let meta = fs::metadata(&path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => NOT_FOUND.to_string(),
        std::io::ErrorKind::PermissionDenied => {
            "Access denied. Try Restart as administrator.".to_string()
        }
        _ => format!("Orbit can't open that path: {e}."),
    })?;
    if !meta.is_dir() {
        let infos = rm_query(&[path], &[])?;
        return Ok(to_lockers(infos, &[]));
    }

    // A folder. Windows answers "which processes have this file open" in about 80 ms
    // per file, and Restart Manager asks the same thing one file after another (a
    // minute for 700 files). So Orbit asks directly, on several threads at once, and
    // Restart Manager then describes the processes found (registered as applications),
    // plus the few files Orbit could not ask about.
    let mut files = Vec::new();
    collect_files(&path, 0, &mut files);
    let (pids, mut unchecked) = probe_files(&files);
    if pids.is_empty() && unchecked.is_empty() {
        return Ok(Vec::new());
    }
    unchecked.truncate(MAX_RM_FILES);
    let apps: Vec<RM_UNIQUE_PROCESS> = pids.iter().filter_map(|&pid| unique_process(pid)).collect();
    let mut infos = rm_query(&unchecked, &[]).unwrap_or_default();
    // Small sessions: one that fails (a process exited meanwhile) only loses the app
    // types of its own processes, which are still listed from `pids`.
    for chunk in apps.chunks(APPS_PER_SESSION) {
        if let Ok(more) = rm_query(&[], chunk) {
            infos.extend(more);
        }
    }
    Ok(to_lockers(infos, &pids))
}

/// The pids using any of `files`, and the files Orbit could not ask about. Runs on up
/// to `PROBE_THREADS` threads (the per-file query waits in the kernel, not on the CPU).
fn probe_files(files: &[PathBuf]) -> (Vec<u32>, Vec<PathBuf>) {
    let threads = files.len().clamp(1, PROBE_THREADS);
    let per = files.len().div_ceil(threads).max(1);
    let parts: Vec<(Vec<u32>, Vec<PathBuf>)> = std::thread::scope(|scope| {
        let handles: Vec<_> = files
            .chunks(per)
            .map(|chunk| {
                scope.spawn(move || {
                    let mut pids = Vec::new();
                    let mut unchecked = Vec::new();
                    for f in chunk {
                        match pids_using(f) {
                            Some(list) => pids.extend(list),
                            None => unchecked.push(f.clone()),
                        }
                    }
                    (pids, unchecked)
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    });
    let mut pids: Vec<u32> = Vec::new();
    let mut unchecked = Vec::new();
    for (p, u) in parts {
        for pid in p {
            if !pids.contains(&pid) {
                pids.push(pid);
            }
        }
        unchecked.extend(u);
    }
    (pids, unchecked)
}

/// Pid and start time, as Restart Manager identifies a process. None when Orbit can't
/// open it (System, protected processes).
fn unique_process(pid: u32) -> Option<RM_UNIQUE_PROCESS> {
    let h = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION).ok()?;
    let t = handle_creation_filetime(h.0)?;
    Some(RM_UNIQUE_PROCESS {
        dwProcessId: pid,
        ProcessStartTime: FILETIME { dwLowDateTime: t as u32, dwHighDateTime: (t >> 32) as u32 },
    })
}

/// Trims spaces and surrounding quotes, expands %VARS% and requires an absolute path.
fn clean_path(raw: &str) -> Result<PathBuf, String> {
    let mut s = raw.trim();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        s = s[1..s.len() - 1].trim();
    }
    if s.is_empty() {
        return Err(EMPTY_PATH.to_string());
    }
    let expanded = crate::startup::expand_env(s);
    let p = PathBuf::from(expanded.trim());
    if !p.is_absolute() {
        return Err(NOT_ABSOLUTE.to_string());
    }
    Ok(p)
}

/// Files inside `dir`, depth first, up to `MAX_FOLDER_FILES`. Links and folders Orbit
/// can't read are skipped.
fn collect_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > MAX_DEPTH || out.len() >= MAX_FOLDER_FILES {
        return;
    }
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut subdirs = Vec::new();
    for entry in rd.flatten() {
        if out.len() >= MAX_FOLDER_FILES {
            return;
        }
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            subdirs.push(entry.path());
        } else if ft.is_file() {
            out.push(entry.path());
        }
    }
    for sub in subdirs {
        if out.len() >= MAX_FOLDER_FILES {
            return;
        }
        collect_files(&sub, depth + 1, out);
    }
}

#[repr(C)]
struct IoStatusBlock {
    status: usize,
    information: usize,
}

#[link(name = "ntdll", kind = "raw-dylib")]
extern "system" {
    fn NtQueryInformationFile(
        file: *mut c_void,
        io: *mut IoStatusBlock,
        info: *mut c_void,
        len: u32,
        class: u32,
    ) -> i32;
}

/// FileProcessIdsUsingFileInformation.
const FILE_PROCESS_IDS_USING_FILE: u32 = 47;
const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;

/// The pids that have `file` open, straight from the file system
/// (FileProcessIdsUsingFileInformation, what Restart Manager reads per file). The file
/// is opened for attributes only, with full sharing, so this never blocks another
/// program, and Orbit's own handle is not listed. None when Orbit can't ask.
fn pids_using(file: &Path) -> Option<Vec<u32>> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    const SHARE_ALL: u32 = 0x7;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let Ok(f) = fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(SHARE_ALL)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(file)
    else {
        return None;
    };
    // { ULONG count; ULONG_PTR pids[] }: room for 64 pids, grown on demand.
    let mut buf: Vec<usize> = vec![0; 65];
    for _ in 0..4 {
        let mut io = IoStatusBlock { status: 0, information: 0 };
        let status = unsafe {
            NtQueryInformationFile(
                f.as_raw_handle(),
                &mut io,
                buf.as_mut_ptr().cast(),
                (buf.len() * std::mem::size_of::<usize>()) as u32,
                FILE_PROCESS_IDS_USING_FILE,
            )
        };
        if status == STATUS_INFO_LENGTH_MISMATCH {
            buf = vec![0; buf.len() * 4];
            continue;
        }
        if status < 0 {
            return None;
        }
        let count = (buf[0] as u32 as usize).min(buf.len() - 1);
        return Some(buf[1..=count].iter().map(|&pid| pid as u32).collect());
    }
    None
}

/// Ends the Restart Manager session when dropped.
struct RmSession(u32);

impl Drop for RmSession {
    fn drop(&mut self) {
        unsafe {
            let _ = RmEndSession(self.0);
        }
    }
}

fn rm_error(code: WIN32_ERROR, what: &str) -> String {
    match code.0 {
        5 => "Access denied. Try Restart as administrator.".to_string(),
        87 => "Windows could not read that path. Check it and try again.".to_string(),
        _ => format!("Windows could not {what} (error {}).", code.0),
    }
}

fn wide(p: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

/// Registers `files` and `apps` in a fresh Restart Manager session and returns the
/// processes that use any of the files, plus the registered apps.
fn rm_query(files: &[PathBuf], apps: &[RM_UNIQUE_PROCESS]) -> Result<Vec<RM_PROCESS_INFO>, String> {
    if files.is_empty() && apps.is_empty() {
        return Ok(Vec::new());
    }
    let mut handle = 0u32;
    let mut key = [0u16; CCH_RM_SESSION_KEY as usize + 1];
    let rc = unsafe { RmStartSession(&mut handle, 0, PWSTR(key.as_mut_ptr())) };
    if rc != ERROR_SUCCESS {
        return Err(rm_error(rc, "start a file check"));
    }
    let session = RmSession(handle);

    for chunk in files.chunks(REGISTER_BATCH) {
        let wides: Vec<Vec<u16>> = chunk.iter().map(|p| wide(p)).collect();
        let ptrs: Vec<PCWSTR> = wides.iter().map(|w| PCWSTR(w.as_ptr())).collect();
        let rc = unsafe { RmRegisterResources(session.0, Some(&ptrs), None, None) };
        if rc != ERROR_SUCCESS {
            return Err(rm_error(rc, "check that file"));
        }
    }
    if !apps.is_empty() {
        let rc = unsafe { RmRegisterResources(session.0, None, Some(apps), None) };
        if rc != ERROR_SUCCESS {
            return Err(rm_error(rc, "check the programs using that folder"));
        }
    }

    // Two-call pattern: ask how many, then read them. The list can grow in between,
    // so retry a few times on ERROR_MORE_DATA.
    let mut infos: Vec<RM_PROCESS_INFO> = Vec::new();
    for _ in 0..5 {
        let mut needed = 0u32;
        let mut count = infos.len() as u32;
        let mut reasons = 0u32;
        let buf = if infos.is_empty() { None } else { Some(infos.as_mut_ptr()) };
        let rc = unsafe { RmGetList(session.0, &mut needed, &mut count, buf, &mut reasons) };
        if rc == ERROR_SUCCESS {
            infos.truncate(count as usize);
            return Ok(infos);
        }
        if rc != ERROR_MORE_DATA {
            return Err(rm_error(rc, "list the processes using that file"));
        }
        infos = vec![RM_PROCESS_INFO::default(); needed as usize + 4];
    }
    Err("The list of processes kept changing. Try again.".to_string())
}

fn app_type(t: RM_APP_TYPE) -> &'static str {
    if t == RmMainWindow || t == RmOtherWindow {
        "app"
    } else if t == RmService {
        "service"
    } else if t == RmExplorer {
        "explorer"
    } else if t == RmConsole {
        "console"
    } else if t == RmCritical {
        "critical"
    } else {
        "unknown"
    }
}

fn wide_str(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end]).trim().to_string()
}

fn file_name(path: &str) -> Option<String> {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
}

/// One `Locker` per pid, with the exe path when Orbit can read it and the image name
/// ("Code.exe") as the name. `extra` pids Restart Manager did not describe (Orbit
/// could not open them, like System) are added with the type Orbit can tell.
/// Sorted: apps first, then by name.
fn to_lockers(infos: Vec<RM_PROCESS_INFO>, extra: &[u32]) -> Vec<Locker> {
    let names: HashMap<u32, String> = snapshot_processes()
        .map(|list| list.into_iter().map(|p| (p.pid, p.name)).collect())
        .unwrap_or_default();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for info in infos {
        let pid = info.Process.dwProcessId;
        if !seen.insert(pid) {
            continue;
        }
        let exe = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION)
            .ok()
            .and_then(|h| image_path(h.0));
        let name = exe
            .as_deref()
            .and_then(file_name)
            .or_else(|| names.get(&pid).cloned().filter(|n| !n.is_empty()))
            .unwrap_or_else(|| {
                let app = wide_str(&info.strAppName);
                if app.is_empty() {
                    format!("PID {pid}")
                } else {
                    app
                }
            });
        // Restart Manager calls the asking process (Orbit) critical: it won't close it.
        let kind = if pid == std::process::id() { "app" } else { app_type(info.ApplicationType) };
        out.push(Locker { pid, name, exe, app_type: kind.to_string() });
    }
    for &pid in extra {
        if !seen.insert(pid) {
            continue;
        }
        let exe = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION)
            .ok()
            .and_then(|h| image_path(h.0));
        let name = exe
            .as_deref()
            .and_then(file_name)
            .or_else(|| names.get(&pid).cloned().filter(|n| !n.is_empty()))
            .unwrap_or_else(|| if pid == 4 { "System".to_string() } else { format!("PID {pid}") });
        let app_type = if pid <= 4 { "critical" } else { "unknown" };
        out.push(Locker { pid, name, exe, app_type: app_type.to_string() });
    }
    out.sort_by(|a, b| {
        let rank = |l: &Locker| if l.app_type == "app" { 0 } else { 1 };
        rank(a).cmp(&rank(b)).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("orbit-tools-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn finds_this_process_on_an_open_file() {
        let dir = scratch("file");
        let path = dir.join("held.txt");
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(b"orbit").unwrap();
        let me = std::process::id();
        let found = find_lockers(path.to_str().unwrap()).unwrap();
        assert!(found.iter().any(|l| l.pid == me), "own pid missing: {found:?}");
        let mine = found.iter().find(|l| l.pid == me).unwrap();
        assert!(mine.exe.is_some());
        assert!(!mine.name.is_empty());
        drop(f);
        let after = find_lockers(path.to_str().unwrap()).unwrap();
        assert!(!after.iter().any(|l| l.pid == me), "still listed after close: {after:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn finds_a_locked_file_inside_a_folder() {
        let dir = scratch("folder");
        let sub = dir.join("a").join("b");
        fs::create_dir_all(&sub).unwrap();
        for i in 0..20 {
            fs::write(dir.join(format!("free-{i}.txt")), b"x").unwrap();
        }
        let path = sub.join("held.log");
        let f = fs::File::create(&path).unwrap();
        // Quoted, with a trailing space, as pasted from Explorer's "Copy as path".
        let quoted = format!("\"{}\" ", dir.display());
        let found = find_lockers(&quoted).unwrap();
        assert!(found.iter().any(|l| l.pid == std::process::id()), "{found:?}");
        drop(f);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pids_using_lists_only_real_users() {
        let dir = scratch("probe");
        let path = dir.join("probe.txt");
        fs::write(&path, b"x").unwrap();
        assert_eq!(pids_using(&path), Some(vec![]), "a closed file reads as in use");
        let f = fs::File::open(&path).unwrap();
        assert_eq!(pids_using(&path), Some(vec![std::process::id()]));
        drop(f);
        assert_eq!(pids_using(&path), Some(vec![]));
        let _ = fs::remove_dir_all(&dir);
    }

    /// Timing on a real folder: `cargo test --lib folder_timing -- --ignored --nocapture`
    /// (ORBIT_LOCK_DIR overrides the folder).
    #[test]
    #[ignore]
    fn folder_timing() {
        let dir = std::env::var("ORBIT_LOCK_DIR").unwrap_or_else(|_| r"C:\Windows\System32\drivers".into());
        let t = std::time::Instant::now();
        let mut files = Vec::new();
        collect_files(Path::new(&dir), 0, &mut files);
        eprintln!("walk: {} files in {:?}", files.len(), t.elapsed());
        let t = std::time::Instant::now();
        let (pids, unchecked) = probe_files(&files);
        eprintln!("probe: {} pids, unchecked {}, {:?}", pids.len(), unchecked.len(), t.elapsed());
        let t = std::time::Instant::now();
        let found = find_lockers(&dir).unwrap();
        eprintln!("find_lockers: {} in {:?}: {found:?}", found.len(), t.elapsed());
    }

    #[test]
    fn empty_folder_has_no_lockers() {
        let dir = scratch("empty");
        assert!(find_lockers(dir.to_str().unwrap()).unwrap().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn bad_paths_are_short_errors() {
        assert_eq!(find_lockers("   ").unwrap_err(), EMPTY_PATH);
        assert_eq!(find_lockers("relative\\file.txt").unwrap_err(), NOT_ABSOLUTE);
        let missing = std::env::temp_dir().join("orbit-tools-no-such-file-7f3a.txt");
        assert_eq!(find_lockers(missing.to_str().unwrap()).unwrap_err(), NOT_FOUND);
    }

    #[test]
    fn folder_walk_stops_at_the_cap() {
        let dir = scratch("cap");
        // Few real files are enough to check the walk counts and recurses.
        fs::create_dir_all(dir.join("x")).unwrap();
        fs::write(dir.join("one.txt"), b"1").unwrap();
        fs::write(dir.join("x").join("two.txt"), b"2").unwrap();
        let mut out = Vec::new();
        collect_files(&dir, 0, &mut out);
        assert_eq!(out.len(), 2);
        let mut full = vec![PathBuf::new(); MAX_FOLDER_FILES];
        collect_files(&dir, 0, &mut full);
        assert_eq!(full.len(), MAX_FOLDER_FILES);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn app_types_map_to_contract_names() {
        assert_eq!(app_type(RmMainWindow), "app");
        assert_eq!(app_type(RmOtherWindow), "app");
        assert_eq!(app_type(RmService), "service");
        assert_eq!(app_type(RmExplorer), "explorer");
        assert_eq!(app_type(RmConsole), "console");
        assert_eq!(app_type(RmCritical), "critical");
        assert_eq!(app_type(RM_APP_TYPE(0)), "unknown");
    }
}
