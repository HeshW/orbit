//! Process actions (end, restart, suspend, priority, affinity), process
//! details, shell helpers (file location, web search, URLs, Run new task),
//! elevation and report export.
//!
//! Every process action takes the `startTime` the UI saw with the pid. The
//! action opens the process once, checks the creation time on that same
//! handle (so a pid that was reused while a confirm dialog was open is not
//! hit), and then acts on that handle.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::c_void;
use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::mem::size_of;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind, Users};
use tauri::{AppHandle, Manager, State};
use windows::core::{w, HRESULT, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, BOOL, ERROR_ACCESS_DENIED, ERROR_BAD_NETPATH, ERROR_CANCELLED,
    ERROR_FILE_NOT_FOUND, ERROR_INVALID_NAME, ERROR_INVALID_PARAMETER, ERROR_NO_ASSOCIATION,
    ERROR_PATH_NOT_FOUND, FILETIME, HANDLE, HWND, WIN32_ERROR,
};
use windows::Win32::Security::{
    GetTokenInformation, LookupAccountSidW, TokenUser, SID_NAME_USE, TOKEN_QUERY, TOKEN_USER,
};
use windows::Win32::System::Com::{
    CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT, COINIT_APARTMENTTHREADED,
    COINIT_DISABLE_OLE1DDE,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
    TH32CS_SNAPPROCESS,
};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::{
    CreateProcessW, GetExitCodeProcess, GetPriorityClass, GetProcessAffinityMask,
    GetProcessHandleCount, GetProcessTimes, OpenProcess, OpenProcessToken,
    QueryFullProcessImageNameW, SetPriorityClass, SetProcessAffinityMask, TerminateProcess,
    ABOVE_NORMAL_PRIORITY_CLASS, BELOW_NORMAL_PRIORITY_CLASS, CREATE_NEW_CONSOLE,
    HIGH_PRIORITY_CLASS, IDLE_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS, PROCESS_ACCESS_RIGHTS,
    PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION, PROCESS_SUSPEND_RESUME,
    PROCESS_TERMINATE, REALTIME_PRIORITY_CLASS, STARTUPINFOW,
};
use windows::Win32::UI::Shell::{
    FOLDERID_Downloads, SHGetKnownFolderPath, ShellExecuteExW, ShellExecuteW, KF_FLAG_DEFAULT,
    SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowThreadProcessId, SW_SHOWNORMAL};

use crate::model::{same_start, AffinityInfo, ChildRef, ProcessDetails, ProcessRow, SuspendedSet};
use crate::sampler::{join_command_line, AppState};
use crate::winenum::ImagePaths;

const ACCESS_DENIED: &str = "Access denied. Try Restart as administrator.";
const EXITED: &str = "That process has exited.";
const PART_OF_ORBIT: &str = "That process is part of Orbit.";
const UNVERIFIED: &str = "Could not confirm which process that is. Refresh and try again.";

// NTSTATUS values returned by the native suspend/resume calls.
const STATUS_ACCESS_DENIED: i32 = 0xC000_0022_u32 as i32;
const STATUS_PROCESS_IS_TERMINATING: i32 = 0xC000_010A_u32 as i32;

/// `PROCESSINFOCLASS::ProcessBreakOnTermination`: nonzero for critical
/// processes (ending or freezing them takes Windows down).
const PROCESS_BREAK_ON_TERMINATION: u32 = 29;

/// Processes that draw or run the desktop. Suspending one freezes the
/// session, so Orbit could never be clicked again to resume it. Lowercase.
const DESKTOP_CRITICAL: &[&str] = &[
    "dwm.exe",
    "winlogon.exe",
    "csrss.exe",
    "wininit.exe",
    "smss.exe",
    "services.exe",
    "lsass.exe",
    "fontdrvhost.exe",
];

const WINDOWS_NEEDS_IT: &str = "Windows needs this process. Ending it would crash the PC.";

/// Processes Windows cannot run without: ending one bugchecks or logs off
/// the PC, even when Orbit is elevated. The last four are kernel-made
/// processes with no image file. Lowercase, as Toolhelp names them.
const WINDOWS_CRITICAL: &[&str] = &[
    "csrss.exe",
    "wininit.exe",
    "smss.exe",
    "lsass.exe",
    "services.exe",
    "winlogon.exe",
    "system",
    "registry",
    "secure system",
    "memory compression",
];

// NtSuspendProcess / NtResumeProcess / NtQueryInformationProcess are exported
// by ntdll but are not part of the enabled `windows` crate features, so they
// are declared here.
#[link(name = "ntdll")]
extern "system" {
    fn NtSuspendProcess(process_handle: HANDLE) -> i32;
    fn NtResumeProcess(process_handle: HANDLE) -> i32;
    fn NtQueryInformationProcess(
        process_handle: HANDLE,
        class: u32,
        information: *mut c_void,
        length: u32,
        return_length: *mut u32,
    ) -> i32;
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// Owns a kernel handle and closes it on drop.
pub(crate) struct OwnedHandle(pub(crate) HANDLE);

// SAFETY: a kernel handle is a process-wide value; any thread may use or close it.
unsafe impl Send for OwnedHandle {}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

fn win32_hresult(code: u32) -> HRESULT {
    HRESULT::from_win32(code)
}

/// Turns a Windows error into a short, human message.
fn describe(err: &windows::core::Error, action: &str) -> String {
    let code = err.code();
    if code == win32_hresult(ERROR_ACCESS_DENIED.0) {
        return ACCESS_DENIED.to_string();
    }
    if code == win32_hresult(ERROR_INVALID_PARAMETER.0) {
        return EXITED.to_string();
    }
    let message = err.message().to_string();
    let message = message.trim().trim_end_matches('.').trim();
    if message.is_empty() {
        format!("Could not {action}.")
    } else {
        format!("Could not {action}: {message}.")
    }
}

fn describe_ntstatus(status: i32, action: &str) -> String {
    match status {
        STATUS_ACCESS_DENIED => ACCESS_DENIED.to_string(),
        STATUS_PROCESS_IS_TERMINATING => EXITED.to_string(),
        other => format!("Could not {action} (status 0x{:08X}).", other as u32),
    }
}

fn is_protected(pid: u32) -> bool {
    pid == 0 || pid == 4 || pid == std::process::id()
}

/// Refuses actions on the idle process, the System process, Orbit itself
/// and Orbit's own helpers (its WebView2 processes). Resuming stays allowed
/// for Orbit's helpers, so one that got suspended somehow can be woken.
fn guard_pid(pid: u32, verb: &str) -> Result<(), String> {
    if pid == 0 {
        return Err(format!(
            "The System Idle Process only counts unused CPU time, so it can't be {verb}."
        ));
    }
    if pid == 4 {
        // The Windows kernel: ending or suspending it gets the same answer as
        // csrss.exe and the other processes in `WINDOWS_CRITICAL`.
        return Err(match verb {
            "ended" | "suspended" => WINDOWS_NEEDS_IT.to_string(),
            _ => format!("The System process can't be {verb}."),
        });
    }
    if pid == std::process::id() {
        return Err(format!("Orbit can't be {verb} from inside itself."));
    }
    if verb != "resumed" && orbit_subtree().contains(&pid) {
        return Err(PART_OF_ORBIT.to_string());
    }
    Ok(())
}

pub(crate) fn open_process(pid: u32, access: PROCESS_ACCESS_RIGHTS) -> Result<OwnedHandle, String> {
    unsafe { OpenProcess(access, BOOL(0), pid) }
        .map(OwnedHandle)
        .map_err(|e| describe(&e, "open that process"))
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn wide_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

fn lock_suspended(set: &SuspendedSet) -> std::sync::MutexGuard<'_, HashMap<u32, u64>> {
    set.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ---------------------------------------------------------------------------
// Toolhelp snapshot
// ---------------------------------------------------------------------------

pub(crate) struct ProcEntry {
    pub(crate) pid: u32,
    pub(crate) parent: u32,
    pub(crate) threads: u32,
    pub(crate) name: String,
}

pub(crate) fn snapshot_processes() -> Result<Vec<ProcEntry>, String> {
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
            .map_err(|e| describe(&e, "list processes"))?;
        let snap = OwnedHandle(snap);
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut out = Vec::with_capacity(256);
        if Process32FirstW(snap.0, &mut entry).is_ok() {
            loop {
                out.push(ProcEntry {
                    pid: entry.th32ProcessID,
                    parent: entry.th32ParentProcessID,
                    threads: entry.cntThreads,
                    name: wide_to_string(&entry.szExeFile),
                });
                if Process32NextW(snap.0, &mut entry).is_err() {
                    break;
                }
            }
        }
        Ok(out)
    }
}

/// parent pid -> child pids, from a Toolhelp snapshot.
fn children_map(entries: &[ProcEntry]) -> HashMap<u32, Vec<u32>> {
    let mut kids: HashMap<u32, Vec<u32>> = HashMap::new();
    for e in entries {
        if e.pid != 0 && e.pid != e.parent {
            kids.entry(e.parent).or_default().push(e.pid);
        }
    }
    kids
}

/// Creation time of the process behind `handle`, as a FILETIME tick count.
pub(crate) fn handle_creation_filetime(handle: HANDLE) -> Option<u64> {
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) }.ok()?;
    Some(((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64)
}

/// FILETIME ticks to Unix seconds, the same conversion sysinfo uses for
/// `ProcessRow::start_time`.
fn filetime_to_unix(ticks: u64) -> u64 {
    (ticks / 10_000_000).saturating_sub(11_644_473_600)
}

/// Process creation time as a FILETIME tick count, used to reject stale parent links
/// left behind by pid reuse.
fn creation_time(pid: u32) -> Option<u64> {
    if pid == 0 {
        return None;
    }
    let handle = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION).ok()?;
    handle_creation_filetime(handle.0)
}

/// True when `child` can really be a child of `parent` (it started after the parent).
/// Unknown times are given the benefit of the doubt.
fn plausible_child(parent_time: Option<u64>, child_time: Option<u64>) -> bool {
    match (parent_time, child_time) {
        (Some(p), Some(c)) => c >= p,
        _ => true,
    }
}

/// True for a direct child of Orbit that is one of its own helpers. Programs
/// Orbit starts for the user (Run new task, Restart) are children too, but
/// they are not part of Orbit and stay manageable.
fn is_orbit_helper(name: Option<&str>) -> bool {
    name.is_some_and(|n| n.eq_ignore_ascii_case("msedgewebview2.exe"))
}

/// Pids of Orbit's descendants (its WebView2 browser, renderer, GPU and
/// utility processes), not including Orbit itself. Children are linked only
/// when they started after their parent, which guards against pid reuse.
/// An enumeration failure gives an empty set, so no action is blocked by it.
fn orbit_subtree() -> HashSet<u32> {
    let Ok(entries) = snapshot_processes() else {
        return HashSet::new();
    };
    let kids = children_map(&entries);
    let names: HashMap<u32, &str> = entries.iter().map(|e| (e.pid, e.name.as_str())).collect();
    let me = std::process::id();
    let mut out = HashSet::new();
    let mut queue = VecDeque::from([(me, creation_time(me))]);
    while let Some((parent, parent_time)) = queue.pop_front() {
        let Some(children) = kids.get(&parent) else {
            continue;
        };
        for &child in children {
            if child == me || out.contains(&child) {
                continue;
            }
            if parent == me && !is_orbit_helper(names.get(&child).copied()) {
                continue;
            }
            let child_time = creation_time(child);
            if plausible_child(parent_time, child_time) {
                out.insert(child);
                queue.push_back((child, child_time));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Identity checks
// ---------------------------------------------------------------------------

/// Checks that the process behind `handle` started at `expected` (Unix
/// seconds, as the UI saw it in the snapshot). `None` or 0 (unknown) skips
/// the check. A different start time means the pid now belongs to another
/// process, so the original one has exited.
fn verify_identity(handle: HANDLE, expected: Option<u64>) -> Result<(), String> {
    let Some(expected) = expected.filter(|&t| t != 0) else {
        return Ok(());
    };
    let actual = handle_creation_filetime(handle)
        .map(filetime_to_unix)
        .ok_or_else(|| UNVERIFIED.to_string())?;
    if actual.abs_diff(expected) > 1 {
        return Err(EXITED.to_string());
    }
    Ok(())
}

/// Opens `pid` with `access` plus PROCESS_QUERY_LIMITED_INFORMATION and
/// verifies its start time on that same handle, so the check and the action
/// cannot race. Callers without a start time fall back to `access` alone if
/// the query right is refused (the behaviour before start times existed).
fn open_checked(
    pid: u32,
    access: PROCESS_ACCESS_RIGHTS,
    expected: Option<u64>,
) -> Result<OwnedHandle, String> {
    match open_process(pid, access | PROCESS_QUERY_LIMITED_INFORMATION) {
        Ok(handle) => {
            verify_identity(handle.0, expected)?;
            Ok(handle)
        }
        Err(e) => {
            if expected.is_some_and(|t| t != 0) {
                Err(e)
            } else {
                open_process(pid, access)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Windows system images
// ---------------------------------------------------------------------------

/// "%SystemRoot%" as Windows spells it, without a trailing backslash.
fn windows_root() -> &'static str {
    static DIR: OnceLock<String> = OnceLock::new();
    DIR.get_or_init(|| {
        let root = std::env::var("SystemRoot")
            .or_else(|_| std::env::var("windir"))
            .unwrap_or_else(|_| r"C:\Windows".to_string());
        root.trim_end_matches(['\\', '/']).to_string()
    })
}

/// Lowercase "%SystemRoot%\System32", without a trailing backslash.
fn system32_dir() -> &'static str {
    static DIR: OnceLock<String> = OnceLock::new();
    DIR.get_or_init(|| format!(r"{}\System32", windows_root()).to_lowercase())
}

/// Lowercase path with backslashes and without the `\\?\` prefix, for
/// comparing paths (also the key of the startup-impact file).
pub(crate) fn normalize_path(path: &str) -> String {
    let p = path.trim().trim_matches('"').replace('/', "\\").to_lowercase();
    match p.strip_prefix(r"\\?\") {
        Some(rest) => rest.to_string(),
        None => p,
    }
}

/// Current time as a FILETIME tick count (100 ns since 1601), comparable with
/// process creation times.
pub(crate) fn filetime_now() -> u64 {
    let since_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    (since_unix.as_secs() + 11_644_473_600) * 10_000_000 + u64::from(since_unix.subsec_nanos() / 100)
}

/// True when `name` is in `names` and the image is Windows' own copy: its
/// folder is `system32` (lowercase), or the path is unknown or has no folder
/// (as for the kernel-made System, Registry and Memory Compression). A
/// user's exe that only shares the name, somewhere else, does not match.
fn is_system_image(names: &[&str], name: &str, path: Option<&str>, system32: &str) -> bool {
    if !names.contains(&name.trim().to_lowercase().as_str()) {
        return false;
    }
    let Some(path) = path.map(str::trim).filter(|p| !p.is_empty()) else {
        return true;
    };
    let path = path.replace('/', "\\").to_lowercase();
    let path = path.strip_prefix(r"\\?\").unwrap_or(&path);
    match path.rsplit_once('\\') {
        Some((dir, _)) => dir.trim_end_matches('\\') == system32,
        None => true,
    }
}

/// Toolhelp name of `pid` ("csrss.exe", "Registry").
fn toolhelp_name(pid: u32) -> Option<String> {
    snapshot_processes()
        .ok()?
        .into_iter()
        .find(|e| e.pid == pid)
        .map(|e| e.name)
}

/// Full image path of `pid`: from `handle` if it has the query right, else
/// from a fresh query handle, else looked up without opening the process.
fn lookup_image_path(pid: u32, handle: Option<HANDLE>) -> Option<String> {
    handle
        .and_then(image_path)
        .or_else(|| {
            let h = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION).ok()?;
            image_path(h.0)
        })
        .or_else(|| ImagePaths::new().lookup(pid))
}

/// Checks `pid` (named `name`, else looked up) against `names`. The path is
/// only read when the name matches. Returns the matched name.
fn system_image_name(
    names: &[&str],
    pid: u32,
    handle: Option<HANDLE>,
    name: Option<String>,
) -> Option<String> {
    let name = name
        .or_else(|| toolhelp_name(pid))
        .or_else(|| handle.and_then(image_file_name))?;
    if !names.contains(&name.trim().to_lowercase().as_str()) {
        return None;
    }
    let path = lookup_image_path(pid, handle);
    is_system_image(names, &name, path.as_deref(), system32_dir()).then_some(name)
}

/// Refuses to end a process Windows needs (see `WINDOWS_CRITICAL`).
fn refuse_windows_critical(pid: u32, handle: Option<HANDLE>) -> Result<(), String> {
    match system_image_name(WINDOWS_CRITICAL, pid, handle, None) {
        Some(_) => Err(WINDOWS_NEEDS_IT.to_string()),
        None => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// End task / end tree
// ---------------------------------------------------------------------------

const STILL_ACTIVE: u32 = 259;

/// True when the process object still exists but the process has already exited
/// (TerminateProcess then fails with "access denied").
fn has_exited(pid: u32) -> bool {
    let Ok(handle) = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION) else {
        return false;
    };
    handle_has_exited(handle.0) == Some(true)
}

/// Exit state read from the handle itself; `None` if the handle lacks the
/// query right.
fn handle_has_exited(handle: HANDLE) -> Option<bool> {
    let mut code = 0u32;
    unsafe { GetExitCodeProcess(handle, &mut code) }.ok()?;
    Some(code != STILL_ACTIVE)
}

/// TerminateProcess on an open handle, with friendly errors.
fn terminate_handle(handle: HANDLE, pid: u32) -> Result<(), String> {
    unsafe { TerminateProcess(handle, 1) }.map_err(|e| {
        let message = describe(&e, "end that process");
        if message == ACCESS_DENIED
            && handle_has_exited(handle).unwrap_or_else(|| has_exited(pid))
        {
            EXITED.to_string()
        } else {
            message
        }
    })
}

fn terminate(pid: u32, expected: Option<u64>) -> Result<(), String> {
    let handle = open_checked(pid, PROCESS_TERMINATE, expected)?;
    terminate_handle(handle.0, pid)
}

/// Opens `pid` to end it and refuses a process Windows needs. When the open
/// is refused, the check still runs, so an unelevated user is told the real
/// reason instead of "Try Restart as administrator".
fn open_for_end(pid: u32, expected: Option<u64>) -> Result<OwnedHandle, String> {
    match open_checked(pid, PROCESS_TERMINATE, expected) {
        Ok(handle) => {
            refuse_windows_critical(pid, Some(handle.0))?;
            Ok(handle)
        }
        Err(e) => {
            if e == ACCESS_DENIED {
                refuse_windows_critical(pid, None)?;
            }
            Err(e)
        }
    }
}

#[tauri::command]
pub fn end_task(
    pid: u32,
    start_time: Option<u64>,
    suspended: State<'_, SuspendedSet>,
) -> Result<(), String> {
    guard_pid(pid, "ended")?;
    let handle = open_for_end(pid, start_time)?;
    // Ending the Windows shell: make sure a taskbar comes back (see ensure_shell).
    let shell = lookup_image_path(pid, Some(handle.0)).and_then(|path| shell_session(pid, &path));
    let since = filetime_now();
    terminate_handle(handle.0, pid)?;
    lock_suspended(&suspended).remove(&pid);
    if let Some(session) = shell {
        watch_shell(handle, pid, session, since);
    }
    Ok(())
}

/// Collects the descendants of `pid`, deepest first. Protected processes and
/// processes Windows needs (and everything below them, such as Orbit's own
/// WebView2 helpers or services.exe's svchosts) are left out.
fn collect_descendants(
    pid: u32,
    pid_time: Option<u64>,
    kids: &HashMap<u32, Vec<u32>>,
    names: &HashMap<u32, String>,
    visited: &mut HashSet<u32>,
    order: &mut Vec<u32>,
) {
    let Some(children) = kids.get(&pid) else {
        return;
    };
    for &child in children {
        if is_protected(child) || !visited.insert(child) {
            continue;
        }
        let child_time = creation_time(child);
        if !plausible_child(pid_time, child_time) {
            continue;
        }
        let name = names.get(&child).cloned();
        if system_image_name(WINDOWS_CRITICAL, child, None, name).is_some() {
            continue;
        }
        collect_descendants(child, child_time, kids, names, visited, order);
        order.push(child);
    }
}

/// Ends `pid` and its descendants. Returns the pids that were ended and the
/// overall result. The root is opened (and its identity checked) before any
/// child is touched, so a root that cannot be ended leaves its tree alone.
fn end_tree_blocking(pid: u32, start_time: Option<u64>) -> (Vec<u32>, Result<(), String>) {
    if let Err(e) = guard_pid(pid, "ended") {
        return (Vec::new(), Err(e));
    }
    let root = match open_for_end(pid, start_time) {
        Ok(handle) => handle,
        Err(e) => return (Vec::new(), Err(e)),
    };
    let root_time = handle_creation_filetime(root.0).or_else(|| creation_time(pid));

    let entries = match snapshot_processes() {
        Ok(entries) => entries,
        Err(e) => return (Vec::new(), Err(e)),
    };
    let kids = children_map(&entries);
    let names: HashMap<u32, String> = entries.into_iter().map(|e| (e.pid, e.name)).collect();

    let mut visited = HashSet::new();
    visited.insert(pid);
    let mut order = Vec::new();
    collect_descendants(pid, root_time, &kids, &names, &mut visited, &mut order);

    let mut ended = Vec::with_capacity(order.len() + 1);
    for child in order {
        if terminate(child, None).is_ok() {
            ended.push(child);
        }
    }

    let result = match terminate_handle(root.0, pid) {
        Ok(()) => {
            ended.push(pid);
            Ok(())
        }
        // The root exited on its own while its tree was being ended.
        Err(e) if e == EXITED => Ok(()),
        Err(e) if ended.is_empty() => Err(e),
        Err(e) => {
            let n = ended.len();
            Err(format!(
                "Ended {n} child process{}, but {}",
                if n == 1 { "" } else { "es" },
                lower_first(&e)
            ))
        }
    };
    (ended, result)
}

fn lower_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[tauri::command]
pub async fn end_tree(
    pid: u32,
    start_time: Option<u64>,
    suspended: State<'_, SuspendedSet>,
) -> Result<u32, String> {
    let (ended, result) =
        tauri::async_runtime::spawn_blocking(move || end_tree_blocking(pid, start_time))
            .await
            .map_err(|e| format!("Could not end the process tree: {e}."))?;
    {
        let mut set = lock_suspended(&suspended);
        for p in &ended {
            set.remove(p);
        }
    }
    result.map(|()| ended.len() as u32)
}

// ---------------------------------------------------------------------------
// Suspend / resume
// ---------------------------------------------------------------------------

/// Full path of the process image, read from the handle.
pub(crate) fn image_path(handle: HANDLE) -> Option<String> {
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    unsafe { QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) }
        .ok()?;
    let path = String::from_utf16_lossy(&buf[..(len as usize).min(buf.len())]);
    if path.is_empty() {
        None
    } else {
        Some(path)
    }
}

/// File name of the process image ("dwm.exe"), read from the handle.
fn image_file_name(handle: HANDLE) -> Option<String> {
    let path = image_path(handle)?;
    let name = path.rsplit('\\').next().unwrap_or("").to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// True when Windows marks the process critical (it bugchecks if the process
/// ends). A failed query counts as "not critical".
fn is_critical_process(handle: HANDLE) -> bool {
    let mut value: u32 = 0;
    let status = unsafe {
        NtQueryInformationProcess(
            handle,
            PROCESS_BREAK_ON_TERMINATION,
            (&mut value as *mut u32).cast(),
            size_of::<u32>() as u32,
            std::ptr::null_mut(),
        )
    };
    status >= 0 && value != 0
}

/// Refuses to suspend processes the desktop needs: suspending one leaves no
/// way to click Resume. Names only match Windows' own copies in System32.
/// Without a handle only the names are checked.
fn refuse_critical_suspend(pid: u32, handle: Option<HANDLE>) -> Result<(), String> {
    let name = toolhelp_name(pid).or_else(|| handle.and_then(image_file_name));
    if let Some(name) = system_image_name(DESKTOP_CRITICAL, pid, handle, name.clone()) {
        return Err(format!(
            "{name} is needed to draw or run the desktop; suspending it would freeze Windows with no way to resume it."
        ));
    }
    let needed = system_image_name(WINDOWS_CRITICAL, pid, handle, name.clone()).is_some();
    if needed || handle.is_some_and(is_critical_process) {
        let name = name.unwrap_or_else(|| "That process".to_string());
        return Err(format!(
            "{name} is a critical Windows process; suspending it would freeze Windows with no way to resume it."
        ));
    }
    Ok(())
}

#[tauri::command]
pub fn suspend_process(
    pid: u32,
    start_time: Option<u64>,
    suspended: State<'_, SuspendedSet>,
) -> Result<(), String> {
    guard_pid(pid, "suspended")?;
    let handle = match open_process(pid, PROCESS_SUSPEND_RESUME | PROCESS_QUERY_LIMITED_INFORMATION) {
        Ok(h) => h,
        Err(e) => {
            // Give the real reason, not "Try Restart as administrator".
            if e == ACCESS_DENIED {
                refuse_critical_suspend(pid, None)?;
            }
            return Err(e);
        }
    };
    verify_identity(handle.0, start_time)?;
    let actual = handle_creation_filetime(handle.0)
        .map(filetime_to_unix)
        .ok_or_else(|| UNVERIFIED.to_string())?;
    refuse_critical_suspend(pid, Some(handle.0))?;
    {
        let mut set = lock_suspended(&suspended);
        if let Some(&recorded) = set.get(&pid) {
            if same_start(recorded, actual) {
                // Already suspended by Orbit; suspending again would only stack the count.
                return Ok(());
            }
            // A stale entry for an earlier process with the same pid.
            set.remove(&pid);
        }
    }
    let status = unsafe { NtSuspendProcess(handle.0) };
    if status < 0 {
        return Err(describe_ntstatus(status, "suspend that process"));
    }
    lock_suspended(&suspended).insert(pid, actual);
    Ok(())
}

#[tauri::command]
pub fn resume_process(
    pid: u32,
    start_time: Option<u64>,
    suspended: State<'_, SuspendedSet>,
) -> Result<(), String> {
    guard_pid(pid, "resumed")?;
    let recorded = lock_suspended(&suspended).get(&pid).copied();
    let handle = match open_process(pid, PROCESS_SUSPEND_RESUME | PROCESS_QUERY_LIMITED_INFORMATION) {
        Ok(h) => h,
        Err(e) => {
            if e == EXITED {
                lock_suspended(&suspended).remove(&pid);
            }
            return Err(e);
        }
    };
    let actual = handle_creation_filetime(handle.0).map(filetime_to_unix);
    if let (Some(recorded), Some(actual)) = (recorded, actual) {
        if !same_start(recorded, actual) {
            // The process Orbit suspended is gone and its pid was reused.
            lock_suspended(&suspended).remove(&pid);
        }
    }
    // Without a start time from the UI, check against the one recorded at
    // suspend time.
    verify_identity(handle.0, start_time.filter(|&t| t != 0).or(recorded))?;
    let status = unsafe { NtResumeProcess(handle.0) };
    if status < 0 {
        return Err(describe_ntstatus(status, "resume that process"));
    }
    lock_suspended(&suspended).remove(&pid);
    Ok(())
}

/// Resumes every process Orbit suspended. Runs on `RunEvent::Exit` (closing
/// the window, `app.exit`, Restart as administrator), so nothing is left
/// frozen once Orbit is gone. A pid whose start time no longer matches
/// belongs to another process and is left alone. Errors are ignored.
pub fn resume_all(app: &AppHandle) {
    let Some(set) = app.try_state::<SuspendedSet>() else {
        return;
    };
    let entries: Vec<(u32, u64)> = lock_suspended(&set).drain().collect();
    for (pid, start) in entries {
        let Ok(handle) =
            open_process(pid, PROCESS_SUSPEND_RESUME | PROCESS_QUERY_LIMITED_INFORMATION)
        else {
            continue;
        };
        if verify_identity(handle.0, Some(start)).is_ok() {
            unsafe {
                let _ = NtResumeProcess(handle.0);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Priority
// ---------------------------------------------------------------------------

fn priority_name(class: u32) -> Option<&'static str> {
    match class {
        c if c == IDLE_PRIORITY_CLASS.0 => Some("idle"),
        c if c == BELOW_NORMAL_PRIORITY_CLASS.0 => Some("belowNormal"),
        c if c == NORMAL_PRIORITY_CLASS.0 => Some("normal"),
        c if c == ABOVE_NORMAL_PRIORITY_CLASS.0 => Some("aboveNormal"),
        c if c == HIGH_PRIORITY_CLASS.0 => Some("high"),
        c if c == REALTIME_PRIORITY_CLASS.0 => Some("realtime"),
        _ => None,
    }
}

fn priority_class(name: &str) -> Option<PROCESS_CREATION_FLAGS> {
    match name {
        "idle" => Some(IDLE_PRIORITY_CLASS),
        "belowNormal" => Some(BELOW_NORMAL_PRIORITY_CLASS),
        "normal" => Some(NORMAL_PRIORITY_CLASS),
        "aboveNormal" => Some(ABOVE_NORMAL_PRIORITY_CLASS),
        "high" => Some(HIGH_PRIORITY_CLASS),
        "realtime" => Some(REALTIME_PRIORITY_CLASS),
        _ => None,
    }
}

fn read_priority(pid: u32) -> Result<String, String> {
    let handle = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let class = unsafe { GetPriorityClass(handle.0) };
    if class == 0 {
        let err = windows::core::Error::from_win32();
        return Err(describe(&err, "read the priority"));
    }
    priority_name(class)
        .map(str::to_string)
        .ok_or_else(|| "That process uses an unknown priority class.".to_string())
}

#[tauri::command]
pub fn get_priority(pid: u32) -> Result<String, String> {
    read_priority(pid)
}

#[tauri::command]
pub fn set_priority(pid: u32, priority: String, start_time: Option<u64>) -> Result<(), String> {
    let class = priority_class(&priority)
        .ok_or_else(|| format!("Unknown priority \"{priority}\"."))?;
    guard_pid(pid, "changed")?;
    let handle = open_process(pid, PROCESS_SET_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION)?;
    verify_identity(handle.0, start_time)?;
    unsafe { SetPriorityClass(handle.0, class) }
        .map_err(|e| describe(&e, "change the priority"))?;
    if class == REALTIME_PRIORITY_CLASS {
        // Without the increase-base-priority privilege Windows quietly uses High.
        let now = unsafe { GetPriorityClass(handle.0) };
        if now != REALTIME_PRIORITY_CLASS.0 {
            return Err(
                "Realtime needs administrator rights. Windows set High instead.".to_string(),
            );
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Details
// ---------------------------------------------------------------------------

fn handle_count(pid: u32) -> Option<u32> {
    let handle = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION).ok()?;
    let mut count = 0u32;
    unsafe { GetProcessHandleCount(handle.0, &mut count) }.ok()?;
    Some(count)
}

/// Account name of the process token owner ("SYSTEM", "LOCAL SERVICE", "Hesh").
fn token_user_name(pid: u32) -> Option<String> {
    let process = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION).ok()?;
    unsafe {
        let mut raw_token = HANDLE::default();
        OpenProcessToken(process.0, TOKEN_QUERY, &mut raw_token).ok()?;
        let token = OwnedHandle(raw_token);

        let mut needed = 0u32;
        let _ = GetTokenInformation(token.0, TokenUser, None, 0, &mut needed);
        if needed == 0 {
            return None;
        }
        // u64 storage keeps the TOKEN_USER (which holds a pointer) aligned.
        let mut buf = vec![0u64; (needed as usize + 7) / 8];
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(buf.as_mut_ptr() as *mut c_void),
            (buf.len() * 8) as u32,
            &mut needed,
        )
        .ok()?;
        let token_user = &*(buf.as_ptr() as *const TOKEN_USER);

        let mut name = [0u16; 256];
        let mut name_len = name.len() as u32;
        let mut domain = [0u16; 256];
        let mut domain_len = domain.len() as u32;
        let mut sid_use = SID_NAME_USE::default();
        LookupAccountSidW(
            PCWSTR::null(),
            token_user.User.Sid,
            PWSTR(name.as_mut_ptr()),
            &mut name_len,
            PWSTR(domain.as_mut_ptr()),
            &mut domain_len,
            &mut sid_use,
        )
        .ok()?;
        let len = (name_len as usize).min(name.len());
        let value = String::from_utf16_lossy(&name[..len]);
        if value.is_empty() {
            None
        } else {
            Some(value)
        }
    }
}

/// Account name for a sysinfo user id. The user list is kept between calls
/// and rebuilt only when a lookup misses.
fn user_name_for(uid: &sysinfo::Uid) -> Option<String> {
    static USERS: OnceLock<Mutex<Users>> = OnceLock::new();
    let users = USERS.get_or_init(|| Mutex::new(Users::new_with_refreshed_list()));
    let mut users = users.lock().unwrap_or_else(|p| p.into_inner());
    let find = |users: &Users| {
        users
            .get_user_by_id(uid)
            .map(|u| u.name().to_string())
            .filter(|s| !s.is_empty())
    };
    find(&users).or_else(|| {
        users.refresh();
        find(&users)
    })
}

/// What the sampler's latest snapshot already knows about a process.
struct Known {
    row: ProcessRow,
    parent_name: Option<String>,
    children: Vec<ChildRef>,
}

/// Static facts for `pid` from the latest snapshot, or `None` if the pid is
/// not in it. Parent and child links are kept only when the start times
/// make sense (pid reuse leaves stale parent pids behind).
fn known_from_snapshot(state: &AppState, pid: u32) -> Option<Known> {
    let latest = state.latest.lock().unwrap_or_else(|p| p.into_inner());
    let snapshot = latest.as_ref()?;
    let row = snapshot.processes.iter().find(|r| r.pid == pid)?.clone();
    let started_after = |child: u64, parent: u64| child == 0 || parent == 0 || child >= parent;

    let parent_name = row
        .parent_pid
        .filter(|&pp| pp != pid && !(pp == 0 && pid != 0))
        .and_then(|pp| snapshot.processes.iter().find(|r| r.pid == pp))
        .filter(|p| started_after(row.start_time, p.start_time))
        .map(|p| p.name.clone());

    let mut children: Vec<ChildRef> = snapshot
        .processes
        .iter()
        .filter(|r| r.parent_pid == Some(pid) && r.pid != pid && r.pid != 0)
        .filter(|r| started_after(r.start_time, row.start_time))
        .map(|r| ChildRef {
            pid: r.pid,
            name: r.name.clone(),
        })
        .collect();
    children.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    Some(Known {
        row,
        parent_name,
        children,
    })
}

fn collect_details(pid: u32, known: Option<Known>) -> Result<ProcessDetails, String> {
    let spid = Pid::from_u32(pid);
    let mut kind = ProcessRefreshKind::nothing()
        .with_memory()
        .with_disk_usage()
        .with_cwd(UpdateKind::Always);
    if known.is_none() {
        kind = kind
            .with_exe(UpdateKind::Always)
            .with_cmd(UpdateKind::Always)
            .with_user(UpdateKind::Always);
    }
    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[spid]), true, kind);
    let process = sys.process(spid).ok_or_else(|| EXITED.to_string())?;

    // A snapshot row older than a pid reuse describes another process.
    let known = known.filter(|k| {
        k.row.start_time == 0
            || process.start_time() == 0
            || same_start(k.row.start_time, process.start_time())
    });

    // Thread counts come from Toolhelp.
    let entries = snapshot_processes().unwrap_or_default();
    let own_entry = entries.iter().find(|e| e.pid == pid);

    let cwd = process
        .cwd()
        .map(|p| p.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty());
    let disk = process.disk_usage();

    let (name, exe, cmd, user, parent_pid, parent_name, start_time, children) = match known {
        Some(k) => {
            let user = k
                .row
                .user
                .clone()
                .or_else(|| token_user_name(pid))
                .or_else(|| (pid == 0 || pid == 4).then(|| "SYSTEM".to_string()));
            let parent_pid = k
                .row
                .parent_pid
                .filter(|&p| p != pid && !(p == 0 && pid != 0));
            (
                k.row.name,
                k.row.exe,
                k.row.cmd,
                user,
                parent_pid,
                k.parent_name,
                k.row.start_time,
                k.children,
            )
        }
        None => slow_static_facts(pid, process, &entries, own_entry),
    };

    Ok(ProcessDetails {
        pid: pid as _,
        parent_pid: parent_pid.map(|p| p as _),
        parent_name,
        name,
        exe,
        cmd,
        cwd,
        user,
        start_time: start_time as _,
        priority: read_priority(pid).ok(),
        threads: own_entry.map(|e| e.threads as _),
        handles: handle_count(pid).map(|h| h as _),
        memory: process.memory() as _,
        virtual_memory: process.virtual_memory() as _,
        children,
        disk_total_read: disk.total_read_bytes as _,
        disk_total_written: disk.total_written_bytes as _,
    })
}

type StaticFacts = (
    String,
    Option<String>,
    String,
    Option<String>,
    Option<u32>,
    Option<String>,
    u64,
    Vec<ChildRef>,
);

/// Static facts read directly, for a pid the sampler has not seen yet.
fn slow_static_facts(
    pid: u32,
    process: &sysinfo::Process,
    entries: &[ProcEntry],
    own_entry: Option<&ProcEntry>,
) -> StaticFacts {
    let own_time = creation_time(pid);

    let name = {
        let n = process.name().to_string_lossy().into_owned();
        if n.is_empty() {
            own_entry.map(|e| e.name.clone()).unwrap_or_default()
        } else {
            n
        }
    };

    let exe = process
        .exe()
        .map(|p| p.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .or_else(|| ImagePaths::new().lookup(pid));

    let cmd = join_command_line(process.cmd());

    let user = process
        .user_id()
        .and_then(user_name_for)
        .or_else(|| token_user_name(pid))
        .or_else(|| (pid == 0 || pid == 4).then(|| "SYSTEM".to_string()));

    let parent_pid = process
        .parent()
        .map(|p| p.as_u32())
        .or_else(|| own_entry.map(|e| e.parent))
        .filter(|&p| p != pid && !(p == 0 && pid != 0));

    let parent_name = parent_pid.and_then(|ppid| {
        let entry = entries.iter().find(|e| e.pid == ppid)?;
        // A parent that started after this process is a reused pid, not the parent.
        if plausible_child(creation_time(ppid), own_time) {
            Some(entry.name.clone())
        } else {
            None
        }
    });

    let mut children: Vec<ChildRef> = entries
        .iter()
        .filter(|e| e.parent == pid && e.pid != pid && e.pid != 0)
        .filter(|e| plausible_child(own_time, creation_time(e.pid)))
        .map(|e| ChildRef {
            pid: e.pid as _,
            name: e.name.clone(),
        })
        .collect();
    children.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    (
        name,
        exe,
        cmd,
        user,
        parent_pid,
        parent_name,
        process.start_time(),
        children,
    )
}

#[tauri::command]
pub async fn get_process_details(
    pid: u32,
    state: State<'_, AppState>,
) -> Result<ProcessDetails, String> {
    let known = known_from_snapshot(&state, pid);
    tauri::async_runtime::spawn_blocking(move || collect_details(pid, known))
        .await
        .map_err(|e| format!("Could not read process details: {e}."))?
}

// ---------------------------------------------------------------------------
// Shell helpers
// ---------------------------------------------------------------------------

fn open_location_blocking(path: String) -> Result<(), String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("No file path is known for that process.".to_string());
    }
    if trimmed.contains('"') {
        return Err("That path is not valid.".to_string());
    }
    let normalized = trimmed.replace('/', "\\");
    if !Path::new(&normalized).exists() {
        return Err("That file no longer exists.".to_string());
    }
    Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{normalized}\""))
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Could not open File Explorer: {e}."))
}

/// Off the main thread: `exists()` on an unreachable network path can block
/// for the SMB timeout.
#[tauri::command]
pub async fn open_location(path: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || open_location_blocking(path))
        .await
        .map_err(|e| e.to_string())?
}

/// Percent-encodes a query string value (UTF-8, unreserved characters kept, spaces as '+').
fn url_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len() * 3);
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            other => {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                out.push('%');
                out.push(HEX[(other >> 4) as usize] as char);
                out.push(HEX[(other & 0x0F) as usize] as char);
            }
        }
    }
    out
}

fn shell_open(target: &str) -> Result<(), String> {
    let file = to_wide(target);
    let result = unsafe {
        ShellExecuteW(
            HWND::default(),
            w!("open"),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if (result.0 as isize) <= 32 {
        Err("Could not open the default browser.".to_string())
    } else {
        Ok(())
    }
}

#[tauri::command]
pub fn search_online(query: String) -> Result<(), String> {
    let query: String = query
        .chars()
        .filter(|c| !c.is_control())
        .take(500)
        .collect();
    let query = query.trim();
    if query.is_empty() {
        return Err("Nothing to search for.".to_string());
    }
    let url = format!("https://www.bing.com/search?q={}", url_encode(query));
    shell_open(&url)
}

#[tauri::command]
pub fn open_url(url: String) -> Result<(), String> {
    let url = url.trim();
    let lower = url.to_ascii_lowercase();
    if !lower.starts_with("https://") || url.len() <= "https://".len() {
        return Err("Only https:// links can be opened.".to_string());
    }
    if url.len() > 2048 || url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("That link is not valid.".to_string());
    }
    shell_open(url)
}

#[tauri::command]
pub fn restart_as_admin(app: AppHandle) -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("Could not find Orbit's executable: {e}."))?;
    let exe = to_wide(&exe.to_string_lossy());
    // The elevated Orbit waits for this one to exit before it takes the
    // single-instance lock (shell::configure), so it is not turned away.
    let params = to_wide(&format!("{} {}", crate::shell::AWAIT_PID_FLAG, std::process::id()));
    let result = unsafe {
        ShellExecuteW(
            HWND::default(),
            w!("runas"),
            PCWSTR(exe.as_ptr()),
            PCWSTR(params.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if (result.0 as isize) <= 32 {
        return Err("Elevation was cancelled.".to_string());
    }
    // app.exit raises RunEvent::Exit, whose handler resumes suspended processes.
    app.exit(0);
    Ok(())
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

fn downloads_dir() -> Result<PathBuf, String> {
    let known = unsafe {
        match SHGetKnownFolderPath(&FOLDERID_Downloads, KF_FLAG_DEFAULT, HANDLE::default()) {
            Ok(raw) => {
                let path = raw.to_string().ok();
                CoTaskMemFree(Some(raw.0 as *const c_void));
                path
            }
            Err(_) => None,
        }
    };
    let dir = match known.filter(|p| !p.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => {
            let profile = std::env::var_os("USERPROFILE")
                .ok_or_else(|| "Could not find your Downloads folder.".to_string())?;
            PathBuf::from(profile).join("Downloads")
        }
    };
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Could not open your Downloads folder: {e}."))?;
    Ok(dir)
}

const RESERVED_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Returns (stem, extension) for a safe file name inside the Downloads folder.
/// Extensions other than csv/txt become part of the stem and ".txt" is added.
fn sanitize_file_name(raw: &str) -> (String, String) {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'))
        .collect();
    let cleaned = cleaned.trim().trim_matches(['.', ' ']).trim();

    let (mut stem, ext) = match cleaned.rfind('.') {
        Some(dot) => {
            let ext = cleaned[dot + 1..].to_ascii_lowercase();
            if ext == "csv" || ext == "txt" {
                (cleaned[..dot].to_string(), ext)
            } else {
                (cleaned.to_string(), "txt".to_string())
            }
        }
        None => (cleaned.to_string(), "txt".to_string()),
    };

    stem = stem.trim().trim_end_matches(['.', ' ']).trim().to_string();
    if stem.chars().count() > 120 {
        stem = stem.chars().take(120).collect::<String>().trim_end().to_string();
    }
    if stem.is_empty() {
        stem = "orbit-report".to_string();
    }
    let base = stem.split('.').next().unwrap_or("").trim().to_ascii_uppercase();
    if RESERVED_NAMES.contains(&base.as_str()) {
        stem = format!("orbit-{stem}");
    }
    (stem, ext)
}

fn export_report_blocking(file_name: String, contents: String) -> Result<String, String> {
    let dir = downloads_dir()?;
    let (stem, ext) = sanitize_file_name(&file_name);

    for n in 1..=999u32 {
        let candidate = if n == 1 {
            format!("{stem}.{ext}")
        } else {
            format!("{stem} ({n}).{ext}")
        };
        let path = dir.join(&candidate);
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("Could not save the report: {e}.")),
        };
        let write = (|| -> std::io::Result<()> {
            if ext == "csv" {
                file.write_all(&[0xEF, 0xBB, 0xBF])?;
            }
            file.write_all(contents.as_bytes())?;
            file.flush()
        })();
        if let Err(e) = write {
            drop(file);
            let _ = std::fs::remove_file(&path);
            return Err(format!("Could not save the report: {e}."));
        }
        return Ok(path.to_string_lossy().into_owned());
    }
    Err("Could not pick a free file name in Downloads.".to_string())
}


/// Off the main thread: Downloads may be redirected to a network or OneDrive
/// folder, where the write can block.
#[tauri::command]
pub async fn export_report(file_name: String, contents: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || export_report_blocking(file_name, contents))
        .await
        .map_err(|e| e.to_string())?
}

// ---------------------------------------------------------------------------
// Restart
// ---------------------------------------------------------------------------

const NO_PATH: &str = "Orbit can't read this process's path. Try Restart as administrator.";
const STORE_APP: &str = "Store apps can't be restarted this way. Open it again from Start.";
const NO_RESTART_WINDOWS: &str = "Windows needs this process. Orbit can't restart it.";
const NO_RESTART_SERVICE: &str = "Services can't be restarted this way. Use the Services view.";
const RESTART_WAIT_MS: u64 = 5_000;
const SHELL_WAIT_MS: u64 = 3_000;

/// `PROCESSINFOCLASS::ProcessCommandLineInformation` (Windows 8.1+): the
/// command line exactly as the process got it.
const PROCESS_COMMAND_LINE_INFORMATION: u32 = 60;

#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *const u16,
}

/// The raw command line of the process behind `handle` (needs only the
/// limited query right).
fn raw_command_line(handle: HANDLE) -> Option<String> {
    let mut needed = 0u32;
    unsafe {
        NtQueryInformationProcess(
            handle,
            PROCESS_COMMAND_LINE_INFORMATION,
            std::ptr::null_mut(),
            0,
            &mut needed,
        );
    }
    if (needed as usize) < size_of::<UnicodeString>() || needed > 1 << 20 {
        return None;
    }
    // u64 storage keeps the UNICODE_STRING (which holds a pointer) aligned.
    let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
    let status = unsafe {
        NtQueryInformationProcess(
            handle,
            PROCESS_COMMAND_LINE_INFORMATION,
            buf.as_mut_ptr().cast(),
            (buf.len() * 8) as u32,
            &mut needed,
        )
    };
    if status < 0 {
        return None;
    }
    let us = unsafe { &*(buf.as_ptr() as *const UnicodeString) };
    let start = buf.as_ptr() as usize;
    let end = start + buf.len() * 8;
    let text = us.buffer as usize;
    let bytes = us.length as usize;
    // The string follows the header inside `buf`; anything else is not trusted.
    if us.buffer.is_null() || bytes == 0 || text < start || text + bytes > end {
        return None;
    }
    let units = unsafe { std::slice::from_raw_parts(us.buffer, bytes / 2) };
    let line = String::from_utf16_lossy(units).trim().to_string();
    (!line.is_empty()).then_some(line)
}

/// Command line and working directory, the way the details drawer reads them
/// (sysinfo). `expected` (Unix seconds) rejects a reused pid.
fn sysinfo_cmd_cwd(pid: u32, expected: Option<u64>) -> (Option<String>, Option<String>) {
    let spid = Pid::from_u32(pid);
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[spid]),
        true,
        ProcessRefreshKind::nothing()
            .with_cmd(UpdateKind::Always)
            .with_cwd(UpdateKind::Always),
    );
    let Some(p) = sys.process(spid) else {
        return (None, None);
    };
    if let Some(t) = expected {
        if p.start_time() != 0 && !same_start(t, p.start_time()) {
            return (None, None);
        }
    }
    let cmd = Some(join_command_line(p.cmd())).filter(|s| !s.trim().is_empty());
    let cwd = p
        .cwd()
        .map(|c| c.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty());
    (cmd, cwd)
}

/// Quotes an argument for a Windows command line when it holds spaces.
fn quote_arg(s: &str) -> String {
    if s.is_empty() || s.chars().any(char::is_whitespace) {
        format!("\"{s}\"")
    } else {
        s.to_string()
    }
}

/// Starts `exe` with `cmd` (the whole command line, argv[0] included) in
/// `cwd`. Without a command line the exe path alone is used; a working
/// directory that no longer exists falls back to the exe's folder.
fn spawn_process(exe: &str, cmd: Option<&str>, cwd: Option<&str>) -> Result<u32, String> {
    let app = to_wide(exe);
    let line = cmd.map(str::to_string).unwrap_or_else(|| quote_arg(exe));
    let mut line = to_wide(&line);
    let dir = cwd
        .map(str::to_string)
        .filter(|d| Path::new(d).is_dir())
        .or_else(|| Path::new(exe).parent().map(|p| p.to_string_lossy().into_owned()));
    let dir = dir.as_deref().map(to_wide);
    let startup = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut info = PROCESS_INFORMATION::default();
    unsafe {
        CreateProcessW(
            PCWSTR(app.as_ptr()),
            PWSTR(line.as_mut_ptr()),
            None,
            None,
            BOOL(0),
            // Console programs get a console of their own (ignored for GUI programs).
            CREATE_NEW_CONSOLE,
            None,
            dir.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
            &startup,
            &mut info,
        )
    }
    .map_err(|e| describe(&e, "start it again"))?;
    unsafe {
        let _ = CloseHandle(info.hThread);
        let _ = CloseHandle(info.hProcess);
    }
    Ok(info.dwProcessId)
}

/// Waits up to `ms` for the process behind `handle` to exit.
fn wait_exit(handle: HANDLE, pid: u32, ms: u64) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    loop {
        let done = match handle_has_exited(handle) {
            Some(done) => done,
            // No query right on this handle: ask a fresh one.
            None => open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION)
                .map_or(true, |h| handle_has_exited(h.0) == Some(true)),
        };
        if done {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Store and system apps (packaged) live under WindowsApps or SystemApps and
/// only start properly through their package.
fn is_packaged_path(path: &str) -> bool {
    let p = normalize_path(path);
    p.contains(r"\windowsapps\") || p.contains(r"\systemapps\")
}

/// True for Windows' own `%SystemRoot%\explorer.exe` (`windir` lowercase).
fn is_shell_explorer(path: &str, windir: &str) -> bool {
    normalize_path(path) == format!(r"{}\explorer.exe", windir.trim_end_matches('\\'))
}

fn file_name_of(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or(path).to_string()
}

fn session_of(pid: u32) -> Option<u32> {
    let mut session = 0u32;
    unsafe { ProcessIdToSessionId(pid, &mut session) }.ok()?;
    Some(session)
}

/// The session of `pid` when its image is the Windows shell (explorer.exe).
fn shell_session(pid: u32, path: &str) -> Option<u32> {
    if is_shell_explorer(path, &windows_root().to_lowercase()) {
        session_of(pid)
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug)]
struct ShellCandidate {
    pid: u32,
    session: Option<u32>,
    /// FILETIME ticks.
    created: Option<u64>,
}

/// An explorer.exe that started in `session` at or after `since` (FILETIME),
/// other than the one that was ended.
fn pick_new_shell(candidates: &[ShellCandidate], old_pid: u32, session: u32, since: u64) -> Option<u32> {
    candidates
        .iter()
        .find(|c| c.pid != old_pid && c.session == Some(session) && c.created.is_some_and(|t| t >= since))
        .map(|c| c.pid)
}

fn shell_candidates() -> Vec<ShellCandidate> {
    snapshot_processes()
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.name.eq_ignore_ascii_case("explorer.exe"))
        .map(|e| ShellCandidate {
            pid: e.pid,
            session: session_of(e.pid),
            created: creation_time(e.pid),
        })
        .collect()
}

/// Pid of the process that owns the taskbar, if there is a taskbar.
fn tray_owner() -> Option<u32> {
    let hwnd = unsafe { FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) }.ok()?;
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    (pid != 0).then_some(pid)
}

/// After explorer.exe in `session` was ended at `since`: waits up to 3 s for
/// Windows to start a new one (AutoRestartShell), else starts
/// `%SystemRoot%\explorer.exe` itself. Returns the new shell's pid. When the
/// ended explorer.exe was only a folder window, the taskbar is still there
/// and nothing is started.
fn ensure_shell(old_pid: u32, session: u32, since: u64) -> Result<u32, String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(SHELL_WAIT_MS);
    loop {
        if let Some(pid) = pick_new_shell(&shell_candidates(), old_pid, session, since) {
            return Ok(pid);
        }
        if let Some(pid) = tray_owner().filter(|&p| p != old_pid) {
            return Ok(pid);
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
    let root = windows_root();
    spawn_process(&format!(r"{root}\explorer.exe"), None, Some(root))
        .map_err(|e| e.replace("start it again", "start Explorer again"))
}

/// End task on the shell: bring a taskbar back in the background. Only one
/// watcher runs at a time (ending every explorer.exe ends several shells).
fn watch_shell(handle: OwnedHandle, pid: u32, session: u32, since: u64) {
    static WATCHING: AtomicBool = AtomicBool::new(false);
    if WATCHING.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        wait_exit(handle.0, pid, RESTART_WAIT_MS);
        drop(handle);
        let _ = ensure_shell(pid, session, since);
        WATCHING.store(false, Ordering::SeqCst);
    });
}

/// Processes Orbit must not relaunch itself, with the reason.
fn restart_refusal(pid: u32, handle: HANDLE, path: &str) -> Result<(), String> {
    if is_packaged_path(path) {
        return Err(STORE_APP.to_string());
    }
    if is_critical_process(handle)
        || system_image_name(DESKTOP_CRITICAL, pid, Some(handle), None).is_some()
    {
        return Err(NO_RESTART_WINDOWS.to_string());
    }
    // Session 0 is services: started again by Orbit they would run as the user.
    if session_of(pid) == Some(0) {
        return Err(NO_RESTART_SERVICE.to_string());
    }
    Ok(())
}

/// Returns whether the process was ended, and the new pid.
fn restart_blocking(pid: u32, start_time: Option<u64>) -> (bool, Result<u32, String>) {
    if let Err(e) = guard_pid(pid, "restarted") {
        return (false, Err(e));
    }
    let handle = match open_for_end(pid, start_time) {
        Ok(h) => h,
        Err(e) => return (false, Err(e)),
    };
    let Some(path) = lookup_image_path(pid, Some(handle.0)) else {
        return (false, Err(NO_PATH.to_string()));
    };
    let name = file_name_of(&path);

    if let Some(session) = shell_session(pid, &path) {
        let since = filetime_now();
        if let Err(e) = terminate_handle(handle.0, pid) {
            return (false, Err(e));
        }
        wait_exit(handle.0, pid, RESTART_WAIT_MS);
        let result = ensure_shell(pid, session, since)
            .map_err(|e| format!("Ended Explorer, but {}", lower_first(&e)));
        return (true, result);
    }

    if let Err(e) = restart_refusal(pid, handle.0, &path) {
        return (false, Err(e));
    }
    let expected = handle_creation_filetime(handle.0).map(filetime_to_unix);
    let (sys_cmd, cwd) = sysinfo_cmd_cwd(pid, expected);
    let cmd = raw_command_line(handle.0).or(sys_cmd);

    if let Err(e) = terminate_handle(handle.0, pid) {
        return (false, Err(e));
    }
    if !wait_exit(handle.0, pid, RESTART_WAIT_MS) {
        return (
            true,
            Err(format!("{name} did not close in time, so Orbit did not start it again.")),
        );
    }
    let result = spawn_process(&path, cmd.as_deref(), cwd.as_deref())
        .map_err(|e| format!("Ended {name}, but {}", lower_first(&e)));
    (true, result)
}

/// Ends a process and starts it again with the same command line and working
/// directory. Explorer is handed back to Windows (or started again if Windows
/// does not). Returns the new pid.
#[tauri::command]
pub async fn restart_process(
    pid: u32,
    start_time: Option<u64>,
    suspended: State<'_, SuspendedSet>,
) -> Result<u32, String> {
    let (ended, result) =
        tauri::async_runtime::spawn_blocking(move || restart_blocking(pid, start_time))
            .await
            .map_err(|e| format!("Could not restart that process: {e}."))?;
    if ended {
        lock_suspended(&suspended).remove(&pid);
    }
    result
}

// ---------------------------------------------------------------------------
// Run new task
// ---------------------------------------------------------------------------

const RUN_EMPTY: &str = "Type the name of a program, folder, document or website.";

/// True for "https://x", "ms-settings:display", "shell:startup": a scheme of
/// two or more characters before the first colon (so "C:\x" is not one).
fn looks_like_url(s: &str) -> bool {
    let Some((scheme, rest)) = s.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    scheme.len() >= 2
        && !rest.is_empty()
        && chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// Splits a Run box command into (file, parameters): a quoted path and its
/// arguments, a URL, an unquoted path that may contain spaces (the longest
/// prefix that `exists`), or the first word ("notepad", "cmd /k echo hi").
fn parse_run_command(input: &str, exists: impl Fn(&str) -> bool) -> Option<(String, String)> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(rest) = s.strip_prefix('"') {
        let (file, args) = match rest.find('"') {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => (rest, ""),
        };
        let file = file.trim();
        return (!file.is_empty()).then(|| (file.to_string(), args.trim().to_string()));
    }
    if looks_like_url(s) && !s.contains(char::is_whitespace) {
        return Some((s.to_string(), String::new()));
    }
    // A backslash or a drive ("C:..."); "/k" switches are not paths.
    let path_like = |p: &str| p.contains('\\') || p.as_bytes().get(1) == Some(&b':');
    if path_like(s) && exists(s) {
        return Some((s.to_string(), String::new()));
    }
    let splits: Vec<usize> = s
        .char_indices()
        .filter(|(_, c)| c.is_whitespace())
        .map(|(i, _)| i)
        .collect();
    for &i in splits.iter().rev() {
        let head = s[..i].trim_end();
        if path_like(head) && exists(head) {
            return Some((head.to_string(), s[i..].trim().to_string()));
        }
    }
    match splits.first() {
        Some(&i) => Some((s[..i].to_string(), s[i..].trim().to_string())),
        None => Some((s.to_string(), String::new())),
    }
}

fn run_error(err: &windows::core::Error, file: &str) -> String {
    let code = err.code();
    let is = |e: WIN32_ERROR| code == win32_hresult(e.0);
    if is(ERROR_CANCELLED) {
        return "Elevation was cancelled.".to_string();
    }
    if is(ERROR_FILE_NOT_FOUND)
        || is(ERROR_PATH_NOT_FOUND)
        || is(ERROR_NO_ASSOCIATION)
        || is(ERROR_INVALID_NAME)
        || is(ERROR_BAD_NETPATH)
    {
        return format!("Windows can't find '{file}'. Check the name and try again.");
    }
    describe(err, "run that")
}

/// ShellExecuteEx in a fresh STA thread (what the shell expects), in the
/// user's profile folder like the Windows Run box.
fn shell_run(file: &str, params: &str, as_admin: bool) -> Result<(), String> {
    let file_w = to_wide(file);
    let params_w = to_wide(params);
    let dir = std::env::var("USERPROFILE").ok().filter(|d| Path::new(d).is_dir());
    let dir_w = dir.as_deref().map(to_wide);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: if as_admin { w!("runas") } else { w!("open") },
        lpFile: PCWSTR(file_w.as_ptr()),
        lpParameters: if params.is_empty() { PCWSTR::null() } else { PCWSTR(params_w.as_ptr()) },
        lpDirectory: dir_w.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut info) }.map_err(|e| run_error(&e, file))
}

fn run_task_blocking(command: &str, as_admin: bool) -> Result<(), String> {
    let expanded = crate::startup::expand_env(command.trim());
    let exists = |p: &str| Path::new(p).exists() || Path::new(&format!("{p}.exe")).exists();
    let (file, params) = parse_run_command(&expanded, exists).ok_or_else(|| RUN_EMPTY.to_string())?;
    let file = if file.to_ascii_lowercase().starts_with("www.") {
        format!("https://{file}")
    } else {
        file
    };
    std::thread::spawn(move || {
        let init = unsafe {
            CoInitializeEx(None, COINIT(COINIT_APARTMENTTHREADED.0 | COINIT_DISABLE_OLE1DDE.0))
        };
        let result = shell_run(&file, &params, as_admin);
        if init.is_ok() {
            unsafe { CoUninitialize() };
        }
        result
    })
    .join()
    .unwrap_or_else(|_| Err("Could not run that.".to_string()))
}

/// Runs a program, folder, document or URL like the Windows Run box.
#[tauri::command]
pub async fn run_task(command: String, as_admin: bool) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || run_task_blocking(&command, as_admin))
        .await
        .map_err(|e| format!("Could not run that: {e}."))?
}

// ---------------------------------------------------------------------------
// CPU affinity
// ---------------------------------------------------------------------------

const CRITICAL_UNCHANGED: &str = "Windows needs this process. Orbit won't change it.";

/// Parses a decimal mask from the UI and checks it against the system mask.
fn validate_mask(mask: &str, system: u64) -> Result<u64, String> {
    let m: u64 = mask
        .trim()
        .parse()
        .map_err(|_| "That processor selection is not valid.".to_string())?;
    if m == 0 {
        return Err("Pick at least one processor.".to_string());
    }
    if m & !system != 0 {
        return Err("That selection includes processors this PC does not have.".to_string());
    }
    Ok(m)
}

fn read_affinity(handle: HANDLE) -> Result<(u64, u64), String> {
    let mut process = 0usize;
    let mut system = 0usize;
    unsafe { GetProcessAffinityMask(handle, &mut process, &mut system) }
        .map_err(|e| describe(&e, "read the affinity"))?;
    Ok((process as u64, system as u64))
}

#[tauri::command]
pub fn get_affinity(pid: u32) -> Result<AffinityInfo, String> {
    let handle = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let (process, system) = read_affinity(handle.0)?;
    Ok(AffinityInfo {
        process_mask: process.to_string(),
        system_mask: system.to_string(),
        logical_cores: system.count_ones(),
    })
}

#[tauri::command]
pub fn set_affinity(pid: u32, mask: String, start_time: Option<u64>) -> Result<(), String> {
    guard_pid(pid, "changed")?;
    let handle = match open_process(pid, PROCESS_SET_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION) {
        Ok(h) => h,
        Err(e) => {
            if e == ACCESS_DENIED {
                refuse_windows_critical(pid, None).map_err(|_| CRITICAL_UNCHANGED.to_string())?;
            }
            return Err(e);
        }
    };
    verify_identity(handle.0, start_time)?;
    refuse_windows_critical(pid, Some(handle.0)).map_err(|_| CRITICAL_UNCHANGED.to_string())?;
    if is_critical_process(handle.0) {
        return Err(CRITICAL_UNCHANGED.to_string());
    }
    let (_, system) = read_affinity(handle.0)?;
    let m = validate_mask(&mask, system)?;
    unsafe { SetProcessAffinityMask(handle.0, m as usize) }
        .map_err(|e| describe(&e, "change the affinity"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_queries() {
        assert_eq!(url_encode("chrome.exe high cpu"), "chrome.exe+high+cpu");
        assert_eq!(url_encode("a&b=c"), "a%26b%3Dc");
        assert_eq!(url_encode("é"), "%C3%A9");
    }

    #[test]
    fn sanitizes_names() {
        assert_eq!(
            sanitize_file_name("..\\orbit/processes.csv"),
            ("orbitprocesses".to_string(), "csv".to_string())
        );
        assert_eq!(sanitize_file_name("report.TXT"), ("report".to_string(), "txt".to_string()));
        assert_eq!(sanitize_file_name("data.json"), ("data.json".to_string(), "txt".to_string()));
        assert_eq!(sanitize_file_name("CON.csv"), ("orbit-CON".to_string(), "csv".to_string()));
        assert_eq!(sanitize_file_name("  "), ("orbit-report".to_string(), "txt".to_string()));
    }

    #[test]
    fn maps_priorities() {
        for name in ["idle", "belowNormal", "normal", "aboveNormal", "high", "realtime"] {
            let class = priority_class(name).unwrap();
            assert_eq!(priority_name(class.0), Some(name));
        }
        assert!(priority_class("turbo").is_none());
    }

    #[test]
    fn matches_windows_critical_images() {
        let s32 = r"c:\windows\system32";
        let m = |name: &str, path: Option<&str>| is_system_image(WINDOWS_CRITICAL, name, path, s32);
        // Windows' own copies, any case, with or without the \\?\ prefix.
        assert!(m("csrss.exe", Some(r"C:\Windows\System32\csrss.exe")));
        assert!(m("LSASS.EXE", Some(r"C:\WINDOWS\system32\lsass.exe")));
        assert!(m("winlogon.exe", Some(r"\\?\C:\Windows\System32\winlogon.exe")));
        assert!(m("smss.exe", Some("C:/Windows/System32/smss.exe")));
        for name in ["wininit.exe", "services.exe"] {
            assert!(m(name, Some(&format!(r"C:\Windows\System32\{name}"))));
        }
        // Path unknown (unelevated, or kernel-made processes): the name decides.
        assert!(m("csrss.exe", None));
        assert!(m("services.exe", Some("")));
        for name in ["System", "Registry", "Secure System", "Memory Compression"] {
            assert!(m(name, None));
            assert!(m(name, Some(name)));
        }
        // A user's exe that only shares the name is not protected.
        assert!(!m("csrss.exe", Some(r"C:\Users\Hesh\Downloads\csrss.exe")));
        assert!(!m("lsass.exe", Some(r"C:\Windows\System32\drivers\lsass.exe")));
        assert!(!m("lsass.exe", Some(r"C:\Windows\lsass.exe")));
        assert!(!m("registry", Some(r"D:\tools\Registry")));
        // Other names never match.
        assert!(!m("explorer.exe", Some(r"C:\Windows\explorer.exe")));
        assert!(!m("svchost.exe", Some(r"C:\Windows\System32\svchost.exe")));
        assert!(!m("csrss", None));
    }

    #[test]
    fn refuses_system_and_idle() {
        // pid 4 (System) gets the same answer as csrss.exe.
        assert_eq!(guard_pid(4, "ended"), Err(WINDOWS_NEEDS_IT.to_string()));
        assert_eq!(guard_pid(4, "suspended"), Err(WINDOWS_NEEDS_IT.to_string()));
        assert_eq!(end_tree_blocking(4, None), (Vec::new(), Err(WINDOWS_NEEDS_IT.to_string())));
        assert_eq!(guard_pid(4, "resumed"), Err("The System process can't be resumed.".to_string()));
        assert!(guard_pid(0, "ended").unwrap_err().starts_with("The System Idle Process"));
        assert!(guard_pid(0, "suspended").unwrap_err().ends_with("can't be suspended."));
    }

    #[test]
    fn desktop_list_is_path_aware() {
        let s32 = r"c:\windows\system32";
        assert!(is_system_image(DESKTOP_CRITICAL, "dwm.exe", Some(r"C:\Windows\System32\dwm.exe"), s32));
        assert!(!is_system_image(DESKTOP_CRITICAL, "dwm.exe", Some(r"E:\Games\dwm.exe"), s32));
    }

    #[test]
    fn system32_dir_is_lowercase_without_slash() {
        let dir = system32_dir();
        assert!(dir.ends_with(r"\system32"));
        assert_eq!(dir, dir.to_lowercase());
    }

    /// The live csrss.exe / smss.exe / Registry processes are recognised, even
    /// unelevated (their paths come from the lookup that needs no handle).
    #[test]
    fn recognises_live_critical_processes() {
        let entries = snapshot_processes().unwrap();
        let mut checked = 0;
        for e in &entries {
            let lower = e.name.to_lowercase();
            if WINDOWS_CRITICAL.contains(&lower.as_str()) && e.pid != 0 {
                assert!(
                    system_image_name(WINDOWS_CRITICAL, e.pid, None, Some(e.name.clone())).is_some(),
                    "{} ({}) was not recognised",
                    e.name,
                    e.pid
                );
                assert_eq!(refuse_windows_critical(e.pid, None), Err(WINDOWS_NEEDS_IT.to_string()));
                checked += 1;
            }
        }
        assert!(checked >= 4, "only {checked} critical processes found");
        // Orbit's test binary is not one of them.
        assert_eq!(refuse_windows_critical(std::process::id(), None), Ok(()));
    }

    #[test]
    fn normalizes_paths() {
        assert_eq!(normalize_path(r"\\?\C:\Apps\X.EXE"), r"c:\apps\x.exe");
        assert_eq!(normalize_path(" \"C:/Apps/x.exe\" "), r"c:\apps\x.exe");
    }

    #[test]
    fn recognises_packaged_apps_and_the_shell() {
        assert!(is_packaged_path(
            r"C:\Program Files\WindowsApps\Microsoft.WindowsTerminal_1.21_x64__8wekyb3d8bbwe\WindowsTerminal.exe"
        ));
        assert!(is_packaged_path(r"C:\Windows\SystemApps\ShellExperienceHost_cw5n1h2txyewy\ShellExperienceHost.exe"));
        assert!(!is_packaged_path(r"C:\Program Files\Notepad++\notepad++.exe"));

        let windir = r"c:\windows";
        assert!(is_shell_explorer(r"C:\Windows\explorer.exe", windir));
        assert!(is_shell_explorer(r"\\?\C:\WINDOWS\Explorer.EXE", windir));
        assert!(!is_shell_explorer(r"C:\Windows\SysWOW64\explorer.exe", windir));
        assert!(!is_shell_explorer(r"D:\Tools\explorer.exe", windir));
    }

    #[test]
    fn picks_the_new_shell() {
        let c = |pid, session, created| ShellCandidate { pid, session: Some(session), created: Some(created) };
        let since = 1_000;
        // The ended shell, a folder window from before, another session's shell: none count.
        let old = [c(10, 1, 500), c(11, 1, 900), c(12, 2, 2_000)];
        assert_eq!(pick_new_shell(&old, 10, 1, since), None);
        let with_new = [c(10, 1, 500), c(11, 1, 900), c(12, 2, 2_000), c(13, 1, 1_500)];
        assert_eq!(pick_new_shell(&with_new, 10, 1, since), Some(13));
        // Unknown session or start time is not trusted.
        let unknown = [ShellCandidate { pid: 14, session: None, created: Some(2_000) }, ShellCandidate { pid: 15, session: Some(1), created: None }];
        assert_eq!(pick_new_shell(&unknown, 10, 1, since), None);
    }

    #[test]
    fn only_webview2_children_are_part_of_orbit() {
        assert!(is_orbit_helper(Some("msedgewebview2.exe")));
        assert!(is_orbit_helper(Some("MSEDGEWEBVIEW2.EXE")));
        assert!(!is_orbit_helper(Some("notepad.exe")));
        assert!(!is_orbit_helper(None));
    }

    #[test]
    fn parses_run_box_commands() {
        let none = |_: &str| false;
        let p = |s: &str| parse_run_command(s, none).map(|(f, a)| (f, a));
        let pair = |f: &str, a: &str| Some((f.to_string(), a.to_string()));
        assert_eq!(p("notepad"), pair("notepad", ""));
        assert_eq!(p("  cmd /k echo hi "), pair("cmd", "/k echo hi"));
        assert_eq!(p(r#""C:\Program Files\App\app.exe" --min -x"#), pair(r"C:\Program Files\App\app.exe", "--min -x"));
        assert_eq!(p(r#""C:\No End"#), pair(r"C:\No End", ""));
        assert_eq!(p("https://legacytech.me/x?y=1"), pair("https://legacytech.me/x?y=1", ""));
        assert_eq!(p("ms-settings:display"), pair("ms-settings:display", ""));
        assert_eq!(p(r"C:\Users"), pair(r"C:\Users", ""));
        assert_eq!(p(""), None);
        assert_eq!(p("   "), None);
        assert_eq!(p("\"\""), None);

        // Unquoted paths with spaces: the longest prefix that exists.
        let exists = |s: &str| s == r"C:\Program Files\App\app.exe" || s == r"C:\Program Files";
        assert_eq!(
            parse_run_command(r"C:\Program Files\App\app.exe --min", exists),
            pair(r"C:\Program Files\App\app.exe", "--min")
        );
        assert_eq!(parse_run_command(r"C:\Program Files", exists), pair(r"C:\Program Files", ""));
        // Bare words are never looked up on disk (they go to the shell's search).
        let all = |_: &str| true;
        assert_eq!(parse_run_command("cmd /k echo hi", all), pair("cmd", "/k echo hi"));
    }

    #[test]
    fn spots_urls() {
        assert!(looks_like_url("https://x.y"));
        assert!(looks_like_url("shell:startup"));
        assert!(!looks_like_url(r"C:\Windows"));
        assert!(!looks_like_url("notepad"));
        assert!(!looks_like_url("cmd /k a:b"));
        assert!(!looks_like_url("x:"));
    }

    #[test]
    fn validates_affinity_masks() {
        let system = 0b1111u64;
        assert_eq!(validate_mask("5", system), Ok(5));
        assert_eq!(validate_mask(" 15 ", system), Ok(15));
        assert!(validate_mask("0", system).is_err());
        assert!(validate_mask("16", system).is_err());
        assert!(validate_mask("-1", system).is_err());
        assert!(validate_mask("abc", system).is_err());
        assert!(validate_mask("", system).is_err());
        // The full 64-bit range survives the string round trip.
        assert_eq!(validate_mask(&u64::MAX.to_string(), u64::MAX), Ok(u64::MAX));
    }

    #[test]
    fn quotes_arguments() {
        assert_eq!(quote_arg(r"C:\a\b.exe"), r"C:\a\b.exe");
        assert_eq!(quote_arg(r"C:\Program Files\b.exe"), r#""C:\Program Files\b.exe""#);
    }

    /// Reads this test process's own command line and affinity (no side effects).
    #[test]
    fn reads_own_command_line_and_affinity() {
        let me = open_process(std::process::id(), PROCESS_QUERY_LIMITED_INFORMATION).unwrap();
        let line = raw_command_line(me.0).unwrap();
        let exe = std::env::current_exe().unwrap();
        let stem = exe.file_stem().unwrap().to_string_lossy().to_lowercase();
        assert!(line.to_lowercase().contains(&stem), "{line}");
        let info = get_affinity(std::process::id()).unwrap();
        let system: u64 = info.system_mask.parse().unwrap();
        let process: u64 = info.process_mask.parse().unwrap();
        assert!(system != 0 && process & !system == 0);
        assert_eq!(info.logical_cores, system.count_ones());
    }
}
