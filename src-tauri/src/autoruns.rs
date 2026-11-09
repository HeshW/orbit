//! Autoruns: scheduled tasks, services, logon keys and image hijacks, the
//! places programs use to start themselves beyond the Run keys.
//!
//! Read only. Every registry key here is opened with query/enumerate rights
//! only; nothing writes the Winlogon or Image File Execution Options keys,
//! the Task Scheduler or the service manager.
//!
//! - Tasks: Task Scheduler 2.0 COM (`ITaskService`), every folder, hidden
//!   tasks included, kept when an enabled trigger fires at log on or at
//!   system start. Unelevated, Windows only shows the tasks this user may read.
//! - Services: Win32 services and drivers that start automatically (auto,
//!   delayed, boot, system) whose binary is not signed by Microsoft.
//!   svchost-hosted services are judged by their ServiceDll.
//! - Logon: HKLM Winlogon `Shell` and `Userinit` (64-bit view).
//! - Image hijacks: Image File Execution Options subkeys with a `Debugger`
//!   value (64-bit and 32-bit views).
//!
//! Signatures come from `security::signatures_for` (shared cache, no
//! duplicate WinVerifyTrust code here).

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use windows::core::{Interface, BSTR, PCWSTR, PWSTR, VARIANT};
use windows::Win32::Foundation::{ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS, VARIANT_BOOL};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_LOCAL_MACHINE,
    KEY_ENUMERATE_SUB_KEYS, KEY_QUERY_VALUE, KEY_WOW64_32KEY, KEY_WOW64_64KEY, REG_EXPAND_SZ,
    REG_SAM_FLAGS, REG_SZ, REG_VALUE_TYPE,
};
use windows::Win32::System::Services::{
    CloseServiceHandle, EnumServicesStatusExW, OpenSCManagerW, OpenServiceW, QueryServiceConfig2W,
    QueryServiceConfigW, ENUM_SERVICE_STATUS_PROCESSW, QUERY_SERVICE_CONFIGW, SC_ENUM_PROCESS_INFO,
    SC_HANDLE, SC_MANAGER_CONNECT, SC_MANAGER_ENUMERATE_SERVICE, SERVICE_AUTO_START,
    SERVICE_BOOT_START, SERVICE_CONFIG_DELAYED_AUTO_START_INFO, SERVICE_DELAYED_AUTO_START_INFO,
    SERVICE_DRIVER, SERVICE_QUERY_CONFIG, SERVICE_STATE_ALL, SERVICE_SYSTEM_START, SERVICE_WIN32,
};
use windows::Win32::System::TaskScheduler::{
    IAction, IComHandlerAction, IExecAction, IRegisteredTask, ISessionStateChangeTrigger, ITaskFolder,
    ITaskService, ITrigger, IWeeklyTrigger, TaskScheduler, TASK_ACTION_COM_HANDLER, TASK_ACTION_EXEC,
    TASK_ACTION_SEND_EMAIL, TASK_ACTION_SHOW_MESSAGE, TASK_ACTION_TYPE, TASK_CONSOLE_CONNECT,
    TASK_CONSOLE_DISCONNECT, TASK_ENUM_HIDDEN, TASK_REMOTE_CONNECT, TASK_REMOTE_DISCONNECT,
    TASK_SESSION_LOCK, TASK_SESSION_STATE_CHANGE_TYPE, TASK_SESSION_UNLOCK, TASK_TRIGGER_BOOT,
    TASK_TRIGGER_CUSTOM_TRIGGER_01, TASK_TRIGGER_DAILY, TASK_TRIGGER_EVENT, TASK_TRIGGER_IDLE,
    TASK_TRIGGER_LOGON, TASK_TRIGGER_MONTHLY, TASK_TRIGGER_MONTHLYDOW, TASK_TRIGGER_REGISTRATION,
    TASK_TRIGGER_SESSION_STATE_CHANGE, TASK_TRIGGER_TIME, TASK_TRIGGER_TYPE2, TASK_TRIGGER_WEEKLY,
};

use crate::security::{signatures_for, SignatureInfo};
use crate::startup::expand_env;

const WINLOGON_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon";
const IFEO_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options";
const SERVICES_KEY: &str = r"SYSTEM\CurrentControlSet\Services";

/// Safety stops for broken or hostile data.
const MAX_TASK_FOLDERS: usize = 4096;
const MAX_SUBKEYS: u32 = 65_536;

/// The signer Microsoft uses for third-party drivers it has tested (WHQL).
/// Those are not Microsoft's own files, so they stay in the services list.
const WHQL_SIGNER: &str = "Microsoft Windows Hardware Compatibility Publisher";

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Autoruns {
    pub tasks: Vec<AutorunTask>,
    pub services: Vec<AutorunService>,
    pub logon: LogonKeys,
    pub image_hijacks: Vec<ImageHijack>,
}

/// One scheduled task.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AutorunTask {
    /// Full task path, e.g. "\\Microsoft\\Windows\\Defrag\\ScheduledDefrag".
    pub name: String,
    /// Resolved executable, "" if the action is not an exe.
    pub path: String,
    pub author: Option<String>,
    pub enabled: bool,
    /// Plain words, e.g. "At log on", "Daily at 03:00".
    pub triggers: Vec<String>,
    /// Executable plus arguments.
    pub command: String,
    pub publisher: Option<String>,
    /// "signed" | "unsigned" | "invalid" | "unknown", `None` if not checked.
    pub signature_status: Option<String>,
}

/// One automatic service or driver.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AutorunService {
    pub name: String,
    pub display_name: String,
    /// Image path from the service config.
    pub path: String,
    /// As in `ServiceEntry::start_type`.
    pub start_type: String,
    pub publisher: Option<String>,
    pub signature_status: Option<String>,
    /// Signed by Microsoft.
    pub microsoft: bool,
}

/// HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon values.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct LogonKeys {
    pub shell: String,
    pub userinit: String,
    /// True when `shell` is the Windows default (explorer.exe).
    pub shell_ok: bool,
    /// True when `userinit` is the Windows default (userinit.exe).
    pub userinit_ok: bool,
}

/// An Image File Execution Options "Debugger" value.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ImageHijack {
    /// Image name, e.g. "taskmgr.exe".
    pub image: String,
    pub debugger: String,
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn system_root() -> String {
    std::env::var("SystemRoot")
        .or_else(|_| std::env::var("windir"))
        .unwrap_or_else(|_| r"C:\Windows".to_string())
        .trim_end_matches('\\')
        .to_string()
}

fn eq_ignore_case(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// File name of a Windows path (after the last backslash or slash).
fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// Pulls the file out of a command line: the quoted part, or the first token
/// ending in `.exe`/`.sys`/`.dll` (unquoted paths may contain spaces), or the
/// first word.
fn file_from_command(command: &str) -> String {
    let s = command.trim();
    if let Some(rest) = s.strip_prefix('"') {
        return rest.split('"').next().unwrap_or("").trim().to_string();
    }
    let first = s.split_whitespace().next().unwrap_or("");
    if !first.contains(['\\', '/']) {
        // A bare name ("rundll32") never has spaces.
        return first.to_string();
    }
    // ASCII lowercasing keeps every byte where it is, so offsets found in
    // `lower` are valid in `s` (Unicode lowercasing can change the length).
    let lower = s.to_ascii_lowercase();
    let mut best: Option<usize> = None;
    for ext in [".exe", ".sys", ".dll"] {
        let mut from = 0usize;
        while let Some(idx) = lower[from..].find(ext) {
            let end = from + idx + ext.len();
            let next = lower[end..].chars().next();
            if next.map_or(true, |c| c.is_whitespace() || c == '"' || c == ',') {
                best = Some(best.map_or(end, |b: usize| b.min(end)));
                break;
            }
            from = end;
        }
    }
    match best {
        Some(end) => s[..end].trim().to_string(),
        None => s.split_whitespace().next().unwrap_or("").to_string(),
    }
}

/// Makes a file path absolute the way Windows reads service and task paths:
/// `\??\` and `\\?\` prefixes dropped, `\SystemRoot\` and a bare `System32\`
/// prefix mapped to the Windows folder, bare names found in System32 or the
/// Windows folder. Anything else is returned as it is.
fn absolute_path(raw: &str, root: &str) -> String {
    let mut p = raw.trim().trim_matches('"').replace('/', "\\");
    for prefix in [r"\??\", r"\\?\"] {
        if let Some(rest) = p.strip_prefix(prefix) {
            p = rest.to_string();
        }
    }
    let lower = p.to_lowercase();
    if lower.starts_with(r"\systemroot\") {
        return format!("{root}{}", &p[r"\SystemRoot".len()..]);
    }
    if lower.starts_with(r"system32\") || lower.starts_with(r"syswow64\") {
        return format!("{root}\\{p}");
    }
    if p.is_empty() || p.contains('\\') || Path::new(&p).is_absolute() {
        // Windows adds ".exe" to a program path that has no extension.
        if !p.is_empty() && Path::new(&p).extension().is_none() && !Path::new(&p).is_file() {
            let with_exe = format!("{p}.exe");
            if Path::new(&with_exe).is_file() {
                return with_exe;
            }
        }
        return p;
    }
    let name = if Path::new(&p).extension().is_none() { format!("{p}.exe") } else { p.clone() };
    for dir in [format!("{root}\\System32"), root.to_string()] {
        let candidate = format!("{dir}\\{name}");
        if Path::new(&candidate).is_file() {
            return candidate;
        }
    }
    p
}

/// The file a command line starts, as an absolute path ("" if none).
fn resolve_command_file(command: &str, root: &str) -> String {
    let expanded = expand_env(command);
    let file = file_from_command(&expanded);
    if file.is_empty() {
        String::new()
    } else {
        absolute_path(&file, root)
    }
}

fn is_microsoft(sig: &SignatureInfo) -> bool {
    sig.status == "signed"
        && sig.publisher.as_deref().map_or(false, |p| {
            let p = p.trim();
            p.starts_with("Microsoft") && !eq_ignore_case(p, WHQL_SIGNER)
        })
}

/// The Windows default shell: exactly "explorer.exe" (case and spaces ignored).
fn shell_is_default(value: &str) -> bool {
    eq_ignore_case(value.trim(), "explorer.exe")
}

/// The Windows default Userinit: "<SystemRoot>\system32\userinit.exe,", with or
/// without the trailing comma, any case.
fn userinit_is_default(value: &str, root: &str) -> bool {
    let v = value.trim().trim_end_matches(',').trim();
    let expected = format!("{root}\\system32\\userinit.exe");
    eq_ignore_case(v, &expected) || eq_ignore_case(v, r"C:\Windows\system32\userinit.exe")
}

// ---------------------------------------------------------------------------
// Registry (read only)
// ---------------------------------------------------------------------------

struct RegKey(HKEY);

impl Drop for RegKey {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

impl RegKey {
    /// Opens a key for reading. `sam` only ever holds query/enumerate rights
    /// plus a WOW64 view flag.
    fn open_read(root: HKEY, subkey: &str, view: REG_SAM_FLAGS) -> Option<RegKey> {
        let sub = wide(subkey);
        let mut handle = HKEY::default();
        let sam = KEY_QUERY_VALUE | KEY_ENUMERATE_SUB_KEYS | view;
        let rc = unsafe { RegOpenKeyExW(root, PCWSTR(sub.as_ptr()), 0, sam, &mut handle) };
        (rc == ERROR_SUCCESS).then_some(RegKey(handle))
    }

    fn subkey_names(&self) -> Vec<String> {
        // 255 characters is the documented maximum key-name length.
        let mut buf = vec![0u16; 256];
        let mut names = Vec::new();
        let mut index = 0u32;
        while index < MAX_SUBKEYS {
            let mut len = buf.len() as u32;
            let rc = unsafe {
                RegEnumKeyExW(self.0, index, PWSTR(buf.as_mut_ptr()), &mut len, None, PWSTR::null(), None, None)
            };
            if rc == ERROR_NO_MORE_ITEMS {
                break;
            }
            if rc == ERROR_SUCCESS {
                let len = (len as usize).min(buf.len());
                names.push(String::from_utf16_lossy(&buf[..len]));
            } else if rc != ERROR_MORE_DATA {
                break;
            }
            index += 1;
        }
        names
    }

    /// A REG_SZ / REG_EXPAND_SZ value as text (not expanded). `None` if
    /// missing or another type.
    fn string(&self, name: &str) -> Option<String> {
        let wname = wide(name);
        for _ in 0..4 {
            let mut size = 0u32;
            let mut ty = REG_VALUE_TYPE::default();
            let rc = unsafe { RegQueryValueExW(self.0, PCWSTR(wname.as_ptr()), None, Some(&mut ty), None, Some(&mut size)) };
            if rc != ERROR_SUCCESS || (ty != REG_SZ && ty != REG_EXPAND_SZ) {
                return None;
            }
            let mut data = vec![0u8; size as usize + 2];
            let mut got = data.len() as u32;
            let rc = unsafe {
                RegQueryValueExW(self.0, PCWSTR(wname.as_ptr()), None, Some(&mut ty), Some(data.as_mut_ptr()), Some(&mut got))
            };
            if rc == ERROR_SUCCESS {
                data.truncate(got as usize);
                let units: Vec<u16> = data.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
                return Some(String::from_utf16_lossy(&units[..end]));
            }
            if rc != ERROR_MORE_DATA {
                return None;
            }
        }
        None
    }
}

fn read_logon() -> LogonKeys {
    let root = system_root();
    let Some(key) = RegKey::open_read(HKEY_LOCAL_MACHINE, WINLOGON_KEY, KEY_WOW64_64KEY) else {
        return LogonKeys::default();
    };
    let shell = key.string("Shell").unwrap_or_default();
    let userinit = key.string("Userinit").unwrap_or_default();
    LogonKeys {
        shell_ok: shell_is_default(&shell),
        userinit_ok: userinit_is_default(&userinit, &root),
        shell,
        userinit,
    }
}

fn read_image_hijacks() -> Vec<ImageHijack> {
    let mut out: Vec<ImageHijack> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for view in [KEY_WOW64_64KEY, KEY_WOW64_32KEY] {
        let Some(ifeo) = RegKey::open_read(HKEY_LOCAL_MACHINE, IFEO_KEY, view) else {
            continue;
        };
        for image in ifeo.subkey_names() {
            let Some(sub) = RegKey::open_read(ifeo.0, &image, view) else {
                continue;
            };
            let Some(debugger) = sub.string("Debugger") else {
                continue;
            };
            let debugger = debugger.trim().to_string();
            if debugger.is_empty() {
                continue;
            }
            if seen.insert((image.to_lowercase(), debugger.to_lowercase())) {
                out.push(ImageHijack { image, debugger });
            }
        }
    }
    out.sort_by_cached_key(|h| h.image.to_lowercase());
    out
}

// ---------------------------------------------------------------------------
// Scheduled tasks (Task Scheduler 2.0 COM)
// ---------------------------------------------------------------------------

/// Uninitializes COM on drop when this thread's init succeeded.
struct ComGuard(bool);

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

struct RawTask {
    name: String,
    path: String,
    author: Option<String>,
    enabled: bool,
    triggers: Vec<String>,
    command: String,
}

fn bstr_text(get: impl FnOnce(*mut BSTR) -> windows::core::Result<()>) -> Option<String> {
    let mut b = BSTR::new();
    get(&mut b).ok()?;
    let s = b.to_string();
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// "03:00" from a Task Scheduler time such as "2024-01-01T03:00:00+01:00".
fn clock_of(boundary: &str) -> Option<String> {
    let t = boundary.split('T').nth(1)?;
    let hm: String = t.chars().take(5).collect();
    let ok = hm.len() == 5
        && hm.as_bytes()[2] == b':'
        && hm.chars().enumerate().all(|(i, c)| i == 2 || c.is_ascii_digit());
    ok.then_some(hm)
}

/// "Monday, Friday" from a DaysOfWeek mask (bit 0 = Sunday).
fn weekdays(mask: i16) -> String {
    const NAMES: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
    let mask = mask as u16 & 0x7f;
    if mask == 0x7f {
        return "every day".to_string();
    }
    let days: Vec<&str> = (0..7).filter(|i| mask & (1 << i) != 0).map(|i| NAMES[i]).collect();
    days.join(", ")
}

fn with_clock(base: &str, clock: Option<&str>) -> String {
    match clock {
        Some(c) => format!("{base} at {c}"),
        None => base.to_string(),
    }
}

fn trigger_name(trigger: &ITrigger, ty: TASK_TRIGGER_TYPE2) -> String {
    let clock = bstr_text(|b| unsafe { trigger.StartBoundary(b) }).and_then(|s| clock_of(&s));
    let clock = clock.as_deref();
    match ty {
        TASK_TRIGGER_LOGON => "At log on".to_string(),
        TASK_TRIGGER_BOOT => "At system start".to_string(),
        TASK_TRIGGER_TIME => with_clock("One time", clock),
        TASK_TRIGGER_DAILY => with_clock("Daily", clock),
        TASK_TRIGGER_WEEKLY => {
            let mut mask = 0i16;
            let days = trigger
                .cast::<IWeeklyTrigger>()
                .ok()
                .and_then(|w| unsafe { w.DaysOfWeek(&mut mask) }.ok())
                .map(|_| weekdays(mask))
                .filter(|d| !d.is_empty());
            match days {
                Some(d) => with_clock(&format!("Weekly on {d}"), clock),
                None => with_clock("Weekly", clock),
            }
        }
        TASK_TRIGGER_MONTHLY | TASK_TRIGGER_MONTHLYDOW => with_clock("Monthly", clock),
        TASK_TRIGGER_IDLE => "When idle".to_string(),
        TASK_TRIGGER_REGISTRATION => "When created or changed".to_string(),
        TASK_TRIGGER_EVENT => "On an event".to_string(),
        TASK_TRIGGER_SESSION_STATE_CHANGE => {
            let mut change = TASK_SESSION_STATE_CHANGE_TYPE::default();
            let read = trigger
                .cast::<ISessionStateChangeTrigger>()
                .ok()
                .and_then(|s| unsafe { s.StateChange(&mut change) }.ok());
            match (read, change) {
                (Some(()), TASK_CONSOLE_CONNECT) => "On local connect".to_string(),
                (Some(()), TASK_CONSOLE_DISCONNECT) => "On local disconnect".to_string(),
                (Some(()), TASK_REMOTE_CONNECT) => "On remote connect".to_string(),
                (Some(()), TASK_REMOTE_DISCONNECT) => "On remote disconnect".to_string(),
                (Some(()), TASK_SESSION_LOCK) => "On lock".to_string(),
                (Some(()), TASK_SESSION_UNLOCK) => "On unlock".to_string(),
                _ => "On session change".to_string(),
            }
        }
        TASK_TRIGGER_CUSTOM_TRIGGER_01 => "Custom trigger".to_string(),
        _ => "Other trigger".to_string(),
    }
}

/// Quotes a program path that has spaces, as a command line would.
fn quoted(path: &str) -> String {
    if path.contains(' ') && !path.starts_with('"') {
        format!("\"{path}\"")
    } else {
        path.to_string()
    }
}

/// (exe path, command) of the task's first action that runs something.
fn task_action(action: &IAction, root: &str) -> Option<(String, String)> {
    let mut ty = TASK_ACTION_TYPE::default();
    unsafe { action.Type(&mut ty) }.ok()?;
    match ty {
        TASK_ACTION_EXEC => {
            let exec: IExecAction = action.cast().ok()?;
            let program = bstr_text(|b| unsafe { exec.Path(b) })?;
            let args = bstr_text(|b| unsafe { exec.Arguments(b) });
            let command = match args {
                Some(a) => format!("{} {a}", quoted(&program)),
                None => quoted(&program),
            };
            let path = resolve_command_file(&quoted(&program), root);
            Some((path, command))
        }
        TASK_ACTION_COM_HANDLER => {
            let com: IComHandlerAction = action.cast().ok()?;
            let clsid = bstr_text(|b| unsafe { com.ClassId(b) }).unwrap_or_default();
            Some((String::new(), format!("COM handler {clsid}").trim().to_string()))
        }
        TASK_ACTION_SEND_EMAIL => Some((String::new(), "Send an email".to_string())),
        TASK_ACTION_SHOW_MESSAGE => Some((String::new(), "Show a message".to_string())),
        _ => None,
    }
}

/// The task if an enabled trigger starts it at log on or at system start.
fn read_task(task: &IRegisteredTask, root: &str) -> Option<RawTask> {
    let name = unsafe { task.Path() }.ok()?.to_string();
    let enabled = unsafe { task.Enabled() }.map(|b| b.0 != 0).unwrap_or(false);
    let def = unsafe { task.Definition() }.ok()?;

    let triggers = unsafe { def.Triggers() }.ok()?;
    let mut count = 0i32;
    unsafe { triggers.Count(&mut count) }.ok()?;
    let mut names = Vec::new();
    let mut autostart = false;
    for i in 1..=count {
        let Ok(trigger) = (unsafe { triggers.get_Item(i) }) else {
            continue;
        };
        let mut on = VARIANT_BOOL::default();
        if unsafe { trigger.Enabled(&mut on) }.is_ok() && on.0 == 0 {
            continue;
        }
        let mut ty = TASK_TRIGGER_TYPE2::default();
        if unsafe { trigger.Type(&mut ty) }.is_err() {
            continue;
        }
        if ty == TASK_TRIGGER_LOGON || ty == TASK_TRIGGER_BOOT {
            autostart = true;
        }
        let label = trigger_name(&trigger, ty);
        if !names.contains(&label) {
            names.push(label);
        }
    }
    if !autostart {
        return None;
    }

    let author = unsafe { def.RegistrationInfo() }
        .ok()
        .and_then(|info| bstr_text(|b| unsafe { info.Author(b) }));

    let mut path = String::new();
    let mut commands: Vec<String> = Vec::new();
    if let Ok(actions) = unsafe { def.Actions() } {
        let mut n = 0i32;
        let _ = unsafe { actions.Count(&mut n) };
        for i in 1..=n {
            let Ok(action) = (unsafe { actions.get_Item(i) }) else {
                continue;
            };
            if let Some((p, c)) = task_action(&action, root) {
                if path.is_empty() && !p.is_empty() {
                    path = p;
                }
                commands.push(c);
            }
        }
    }

    Some(RawTask { name, path, author, enabled, triggers: names, command: commands.join("; ") })
}

fn read_folder_tasks(folder: &ITaskFolder, root: &str, out: &mut Vec<RawTask>) {
    let Ok(tasks) = (unsafe { folder.GetTasks(TASK_ENUM_HIDDEN.0) }) else {
        return;
    };
    let n = unsafe { tasks.Count() }.unwrap_or(0);
    for i in 1..=n {
        if let Ok(task) = unsafe { tasks.get_Item(&VARIANT::from(i)) } {
            if let Some(raw) = read_task(&task, root) {
                out.push(raw);
            }
        }
    }
}

fn collect_tasks_com(root: &str) -> Result<Vec<RawTask>, String> {
    let service: ITaskService = unsafe { CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER) }
        .map_err(|e| format!("Could not open Task Scheduler ({e})."))?;
    let empty = VARIANT::default();
    unsafe { service.Connect(&empty, &empty, &empty, &empty) }
        .map_err(|e| format!("Could not connect to Task Scheduler ({e})."))?;
    let top = unsafe { service.GetFolder(&BSTR::from("\\")) }
        .map_err(|e| format!("Could not read the task folders ({e})."))?;

    let mut out = Vec::new();
    let mut stack = vec![top];
    let mut visited = 0usize;
    while let Some(folder) = stack.pop() {
        visited += 1;
        if visited > MAX_TASK_FOLDERS {
            break;
        }
        read_folder_tasks(&folder, root, &mut out);
        if let Ok(subs) = unsafe { folder.GetFolders(0) } {
            let n = unsafe { subs.Count() }.unwrap_or(0);
            for i in 1..=n {
                if let Ok(sub) = unsafe { subs.get_Item(&VARIANT::from(i)) } {
                    stack.push(sub);
                }
            }
        }
    }
    Ok(out)
}

fn collect_tasks(root: &str) -> Result<Vec<RawTask>, String> {
    // S_OK and S_FALSE both need a matching CoUninitialize; a thread already in
    // another apartment (RPC_E_CHANGED_MODE) can still make the calls.
    let init = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let _guard = ComGuard(init.is_ok());
    // Every COM pointer lives inside this call, so it is released before the guard.
    collect_tasks_com(root)
}

// ---------------------------------------------------------------------------
// Services and drivers
// ---------------------------------------------------------------------------

struct ScHandle(SC_HANDLE);

impl Drop for ScHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseServiceHandle(self.0);
        }
    }
}

struct RawService {
    name: String,
    display_name: String,
    path: String,
    start_type: &'static str,
}

fn as_bytes_mut(buf: &mut [u64]) -> &mut [u8] {
    unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, buf.len() * 8) }
}

fn words_for(bytes: u32) -> usize {
    (bytes as usize + 7) / 8
}

fn pwstr_text(p: PWSTR) -> Option<String> {
    if p.is_null() {
        None
    } else {
        unsafe { p.to_string().ok() }
    }
}

/// (name, display name) of every Win32 service and driver.
fn enumerate_services(scm: &ScHandle) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    let mut resume = 0u32;
    let mut buf: Vec<u64> = vec![0; words_for(128 * 1024)];
    for _ in 0..256 {
        let mut needed = 0u32;
        let mut returned = 0u32;
        let result = unsafe {
            EnumServicesStatusExW(
                scm.0,
                SC_ENUM_PROCESS_INFO,
                SERVICE_WIN32 | SERVICE_DRIVER,
                SERVICE_STATE_ALL,
                Some(as_bytes_mut(&mut buf)),
                &mut needed,
                &mut returned,
                Some(&mut resume),
                PCWSTR::null(),
            )
        };
        let more = match result {
            Ok(()) => false,
            Err(e) if e.code() == ERROR_MORE_DATA.to_hresult() => true,
            Err(e) => return Err(format!("Could not list services ({e}).")),
        };
        let items = unsafe {
            std::slice::from_raw_parts(buf.as_ptr() as *const ENUM_SERVICE_STATUS_PROCESSW, returned as usize)
        };
        for item in items {
            let Some(name) = pwstr_text(item.lpServiceName) else {
                continue;
            };
            let display = pwstr_text(item.lpDisplayName)
                .filter(|d| !d.trim().is_empty())
                .unwrap_or_else(|| name.clone());
            out.push((name, display));
        }
        if !more {
            break;
        }
        if returned == 0 {
            let want = words_for(needed);
            if want <= buf.len() {
                break;
            }
            buf = vec![0; want];
        }
    }
    Ok(out)
}

fn is_delayed(svc: &ScHandle) -> bool {
    let mut info = SERVICE_DELAYED_AUTO_START_INFO::default();
    let mut needed = 0u32;
    let ok = unsafe {
        let bytes = std::slice::from_raw_parts_mut(
            &mut info as *mut SERVICE_DELAYED_AUTO_START_INFO as *mut u8,
            std::mem::size_of::<SERVICE_DELAYED_AUTO_START_INFO>(),
        );
        QueryServiceConfig2W(svc.0, SERVICE_CONFIG_DELAYED_AUTO_START_INFO, Some(bytes), &mut needed).is_ok()
    };
    ok && info.fDelayedAutostart.as_bool()
}

/// (start type, image path) for a service that starts by itself, else None.
fn autostart_config(svc: &ScHandle) -> Option<(&'static str, String)> {
    let mut needed = 0u32;
    unsafe {
        let _ = QueryServiceConfigW(svc.0, None, 0, &mut needed);
    }
    if needed == 0 {
        return None;
    }
    let mut buf: Vec<u64> = vec![0; words_for(needed)];
    let cfg = buf.as_mut_ptr() as *mut QUERY_SERVICE_CONFIGW;
    let size = (buf.len() * 8) as u32;
    unsafe { QueryServiceConfigW(svc.0, Some(cfg), size, &mut needed) }.ok()?;
    let st = unsafe { (*cfg).dwStartType };
    let start = if st == SERVICE_AUTO_START {
        if is_delayed(svc) {
            "autoDelayed"
        } else {
            "auto"
        }
    } else if st == SERVICE_BOOT_START {
        "boot"
    } else if st == SERVICE_SYSTEM_START {
        "system"
    } else {
        return None;
    };
    let image = pwstr_text(unsafe { (*cfg).lpBinaryPathName }).unwrap_or_default();
    Some((start, image))
}

/// The ServiceDll of an svchost-hosted service (Parameters key first).
fn service_dll(name: &str) -> Option<String> {
    let base = format!("{SERVICES_KEY}\\{name}");
    for sub in [format!("{base}\\Parameters"), base.clone()] {
        if let Some(key) = RegKey::open_read(HKEY_LOCAL_MACHINE, &sub, KEY_WOW64_64KEY) {
            if let Some(dll) = key.string("ServiceDll").filter(|d| !d.trim().is_empty()) {
                return Some(dll);
            }
        }
    }
    None
}

/// The binary a service runs: its image path made absolute, or for svchost
/// services the ServiceDll; a driver with no image path loads
/// `System32\drivers\<name>.sys`.
fn service_binary(name: &str, image: &str, root: &str) -> String {
    if image.trim().is_empty() {
        return format!("{root}\\System32\\drivers\\{name}.sys");
    }
    let file = resolve_command_file(image, root);
    if eq_ignore_case(file_name(&file), "svchost.exe") {
        if let Some(dll) = service_dll(name) {
            return absolute_path(&expand_env(&dll), root);
        }
    }
    file
}

fn collect_services(root: &str) -> Result<Vec<RawService>, String> {
    let scm = unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT | SC_MANAGER_ENUMERATE_SERVICE) }
        .map(ScHandle)
        .map_err(|e| format!("Could not open the service manager ({e})."))?;
    let mut out = Vec::new();
    for (name, display_name) in enumerate_services(&scm)? {
        let wname = wide(&name);
        let Ok(h) = (unsafe { OpenServiceW(scm.0, PCWSTR(wname.as_ptr()), SERVICE_QUERY_CONFIG) }) else {
            continue;
        };
        let svc = ScHandle(h);
        let Some((start_type, image)) = autostart_config(&svc) else {
            continue;
        };
        let path = service_binary(&name, &image, root);
        out.push(RawService { name, display_name, path, start_type });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Command
// ---------------------------------------------------------------------------

fn collect_all() -> Autoruns {
    let root = system_root();
    // A category that can't be read is left empty; the others still load.
    let raw_tasks = collect_tasks(&root).unwrap_or_default();
    let raw_services = collect_services(&root).unwrap_or_default();

    let mut paths: Vec<String> = Vec::new();
    paths.extend(raw_tasks.iter().filter(|t| !t.path.is_empty()).map(|t| t.path.clone()));
    paths.extend(raw_services.iter().filter(|s| !s.path.is_empty()).map(|s| s.path.clone()));
    let sigs = signatures_for(&paths);
    let sig_of = |p: &str| -> Option<SignatureInfo> {
        if p.is_empty() {
            None
        } else {
            Some(sigs.get(p).cloned().unwrap_or_else(|| SignatureInfo { status: "unknown".into(), publisher: None }))
        }
    };

    let mut tasks: Vec<AutorunTask> = raw_tasks
        .into_iter()
        .map(|t| {
            let sig = sig_of(&t.path);
            AutorunTask {
                name: t.name,
                path: t.path,
                author: t.author,
                enabled: t.enabled,
                triggers: t.triggers,
                command: t.command,
                publisher: sig.as_ref().and_then(|s| s.publisher.clone()),
                signature_status: sig.map(|s| s.status),
            }
        })
        .collect();
    tasks.sort_by_cached_key(|t| t.name.to_lowercase());

    let mut services: Vec<AutorunService> = raw_services
        .into_iter()
        .filter_map(|s| {
            let sig = sig_of(&s.path);
            let microsoft = sig.as_ref().map_or(false, is_microsoft);
            (!microsoft).then(|| AutorunService {
                name: s.name,
                display_name: s.display_name,
                path: s.path,
                start_type: s.start_type.to_string(),
                publisher: sig.as_ref().and_then(|x| x.publisher.clone()),
                signature_status: sig.map(|x| x.status),
                microsoft,
            })
        })
        .collect();
    services.sort_by_cached_key(|s| s.display_name.to_lowercase());

    Autoruns { tasks, services, logon: read_logon(), image_hijacks: read_image_hijacks() }
}

#[tauri::command]
pub async fn list_autoruns() -> Result<Autoruns, String> {
    tauri::async_runtime::spawn_blocking(collect_all)
        .await
        .map_err(|e| format!("Something went wrong: {e}."))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = r"C:\Windows";

    #[test]
    fn finds_file_in_command() {
        assert_eq!(file_from_command(r#""C:\Program Files\X\x.exe" /svc"#), r"C:\Program Files\X\x.exe");
        assert_eq!(file_from_command(r"C:\Program Files\X\x.exe -k run"), r"C:\Program Files\X\x.exe");
        assert_eq!(file_from_command(r"System32\drivers\foo.sys"), r"System32\drivers\foo.sys");
        assert_eq!(file_from_command(r"C:\Windows\system32\svchost.exe -k netsvcs -p"), r"C:\Windows\system32\svchost.exe");
        assert_eq!(file_from_command("rundll32 x.dll,Entry"), "rundll32");
        assert_eq!(
            file_from_command("C:/Program Files (x86)/LagoFast/NetworkDaemon.exe --N=NetworkDaemon"),
            "C:/Program Files (x86)/LagoFast/NetworkDaemon.exe"
        );
        assert_eq!(file_from_command(""), "");
        // Characters whose Unicode lowercase takes more bytes.
        assert_eq!(
            file_from_command(r"C:\Users\İsmail\AppData\Local\x.exe /a"),
            r"C:\Users\İsmail\AppData\Local\x.exe"
        );
        assert_eq!(file_from_command("C:\\\u{212A}it\\Tool.EXE --run"), "C:\\\u{212A}it\\Tool.EXE");
    }

    #[test]
    fn makes_service_paths_absolute() {
        assert_eq!(absolute_path(r"\SystemRoot\System32\drivers\a.sys", ROOT), r"C:\Windows\System32\drivers\a.sys");
        assert_eq!(absolute_path(r"System32\drivers\b.sys", ROOT), r"C:\Windows\System32\drivers\b.sys");
        assert_eq!(absolute_path(r"\??\C:\Tools\c.sys", ROOT), r"C:\Tools\c.sys");
        assert_eq!(absolute_path(r#""D:\Apps\d.exe""#, ROOT), r"D:\Apps\d.exe");
        assert_eq!(service_binary("e", "", ROOT), r"C:\Windows\System32\drivers\e.sys");
        assert_eq!(
            service_binary("Steam", r#""C:\Program Files (x86)\Common Files\Steam\steamservice.exe" /RunAsService"#, ROOT),
            r"C:\Program Files (x86)\Common Files\Steam\steamservice.exe"
        );
    }

    #[test]
    fn reads_svchost_service_dll() {
        // Schedule is hosted by svchost on every Windows 10; its DLL is schedsvc.dll.
        let root = system_root();
        let p = service_binary("Schedule", r"%SystemRoot%\system32\svchost.exe -k netsvcs -p", &root);
        assert!(p.to_lowercase().ends_with(r"\schedsvc.dll"), "{p}");
    }

    #[test]
    fn checks_logon_defaults() {
        assert!(shell_is_default("explorer.exe"));
        assert!(shell_is_default("  Explorer.EXE "));
        assert!(!shell_is_default("explorer.exe, evil.exe"));
        assert!(!shell_is_default(r"C:\Windows\explorer.exe"));
        assert!(userinit_is_default(r"C:\Windows\system32\userinit.exe,", ROOT));
        assert!(userinit_is_default(r"c:\windows\SYSTEM32\userinit.exe", ROOT));
        assert!(userinit_is_default(r"D:\Windows\system32\userinit.exe,", r"D:\Windows"));
        assert!(!userinit_is_default(r"C:\Windows\system32\userinit.exe,C:\evil.exe,", ROOT));
        assert!(!userinit_is_default("", ROOT));
    }

    #[test]
    fn tells_microsoft_from_whql() {
        let s = |st: &str, p: Option<&str>| SignatureInfo { status: st.into(), publisher: p.map(String::from) };
        assert!(is_microsoft(&s("signed", Some("Microsoft Windows"))));
        assert!(is_microsoft(&s("signed", Some("Microsoft Corporation"))));
        assert!(!is_microsoft(&s("signed", Some(WHQL_SIGNER))));
        assert!(!is_microsoft(&s("invalid", Some("Microsoft Windows"))));
        assert!(!is_microsoft(&s("signed", Some("NVIDIA Corporation"))));
        assert!(!is_microsoft(&s("unknown", None)));
    }

    #[test]
    fn names_trigger_times() {
        assert_eq!(clock_of("2024-01-01T03:00:00").as_deref(), Some("03:00"));
        assert_eq!(clock_of("2019-05-12T11:42:00+02:00").as_deref(), Some("11:42"));
        assert_eq!(clock_of("garbage"), None);
        assert_eq!(weekdays(0b10), "Monday");
        assert_eq!(weekdays(0b100010), "Monday, Friday");
        assert_eq!(weekdays(0x7f), "every day");
        assert_eq!(with_clock("Daily", Some("03:00")), "Daily at 03:00");
    }

    #[test]
    fn reads_live_categories() {
        // Read only; just checks that each category loads on this PC.
        let root = system_root();
        let tasks = collect_tasks(&root).expect("tasks");
        assert!(tasks.iter().all(|t| !t.triggers.is_empty()));
        let services = collect_services(&root).expect("services");
        assert!(!services.is_empty());
        let logon = read_logon();
        assert!(!logon.shell.is_empty() && !logon.userinit.is_empty(), "{logon:?}");
        let _ = read_image_hijacks();
    }

    /// `cargo test --lib dump_autoruns -- --ignored --nocapture`: prints what
    /// this PC returns and how long it took.
    #[test]
    #[ignore]
    fn dump_autoruns() {
        for run in 0..2 {
            let t = std::time::Instant::now();
            let a = collect_all();
            println!(
                "run {run}: {} ms, {} tasks, {} services, {} hijacks, logon {:?}",
                t.elapsed().as_millis(),
                a.tasks.len(),
                a.services.len(),
                a.image_hijacks.len(),
                a.logon
            );
            if run == 0 {
                for t in &a.tasks {
                    println!("  T {} | {} | {:?} | {:?} | {:?}", t.name, t.command, t.triggers, t.publisher, t.signature_status);
                }
                for s in &a.services {
                    println!("  S {} | {} | {} | {:?} | {:?}", s.name, s.path, s.start_type, s.publisher, s.signature_status);
                }
                for h in &a.image_hijacks {
                    println!("  H {} | {}", h.image, h.debugger);
                }
            }
        }
    }
}
