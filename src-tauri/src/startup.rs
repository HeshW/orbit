//! Startup entries: the Run registry keys (HKCU, HKLM, HKLM 32-bit) and both
//! Startup folders, with the enabled state Windows keeps in
//! `Explorer\StartupApproved` (the same switch Task Manager flips).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use windows::core::{GUID, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    HANDLE, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS,
    ERROR_SUCCESS, WIN32_ERROR,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegEnumValueW, RegOpenKeyExW, RegQueryValueExW,
    RegSetValueExW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_READ,
    KEY_NOTIFY, KEY_SET_VALUE, KEY_WOW64_64KEY, REG_BINARY, REG_DWORD, REG_EXPAND_SZ, REG_OPTION_NON_VOLATILE,
    REG_SAM_FLAGS, REG_SZ, REG_VALUE_TYPE,
};
use windows::Win32::UI::Shell::{
    FOLDERID_CommonStartup, FOLDERID_Startup, SHGetKnownFolderPath, KF_FLAG_DEFAULT,
};

use crate::model::*;

mod packaged;
mod watch;

pub use watch::start as start_watch;

const ACCESS_DENIED_MSG: &str = "Access denied. Try Restart as administrator.";

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN32_KEY: &str = r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Run";
const APPROVED_RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
const APPROVED_RUN32: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run32";
const APPROVED_FOLDER: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\StartupFolder";

/// Seconds between 1601-01-01 (FILETIME epoch) and 1970-01-01 (Unix epoch).
const FILETIME_UNIX_OFFSET_SECS: u64 = 11_644_473_600;

// ---------------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    HkcuRun,
    HklmRun,
    HklmRun32,
    UserFolder,
    CommonFolder,
}

const ALL_SOURCES: [Source; 5] = [
    Source::HkcuRun,
    Source::HklmRun,
    Source::HklmRun32,
    Source::UserFolder,
    Source::CommonFolder,
];

impl Source {
    /// Prefix of the stable id, including the trailing backslash.
    fn prefix(self) -> &'static str {
        match self {
            Source::HkcuRun => "HKCU\\Run\\",
            Source::HklmRun => "HKLM\\Run\\",
            Source::HklmRun32 => "HKLM\\Run32\\",
            Source::UserFolder => "UserFolder\\",
            Source::CommonFolder => "CommonFolder\\",
        }
    }

    fn location(self) -> &'static str {
        match self {
            Source::HkcuRun => "HKCU Run",
            Source::HklmRun => "HKLM Run",
            Source::HklmRun32 => "HKLM Run (32-bit)",
            Source::UserFolder => "Startup folder",
            Source::CommonFolder => "Startup folder (all users)",
        }
    }

    fn needs_admin(self) -> bool {
        !matches!(self, Source::HkcuRun | Source::UserFolder)
    }

    /// The Run key holding the commands (registry sources only).
    fn run_key(self) -> Option<(HKEY, &'static str)> {
        match self {
            Source::HkcuRun => Some((HKEY_CURRENT_USER, RUN_KEY)),
            Source::HklmRun => Some((HKEY_LOCAL_MACHINE, RUN_KEY)),
            Source::HklmRun32 => Some((HKEY_LOCAL_MACHINE, RUN32_KEY)),
            Source::UserFolder | Source::CommonFolder => None,
        }
    }

    /// The StartupApproved key that stores the on/off state for this source.
    fn approved_key(self) -> (HKEY, &'static str) {
        match self {
            Source::HkcuRun => (HKEY_CURRENT_USER, APPROVED_RUN),
            Source::HklmRun => (HKEY_LOCAL_MACHINE, APPROVED_RUN),
            Source::HklmRun32 => (HKEY_LOCAL_MACHINE, APPROVED_RUN32),
            Source::UserFolder => (HKEY_CURRENT_USER, APPROVED_FOLDER),
            Source::CommonFolder => (HKEY_LOCAL_MACHINE, APPROVED_FOLDER),
        }
    }

    /// The Startup folder on disk (folder sources only).
    fn folder(self) -> Option<PathBuf> {
        match self {
            Source::UserFolder => known_folder(&FOLDERID_Startup),
            Source::CommonFolder => known_folder(&FOLDERID_CommonStartup),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Registry helpers (RAII key guard)
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
    fn open(root: HKEY, subkey: &str, sam: REG_SAM_FLAGS) -> Result<RegKey, WIN32_ERROR> {
        let sub = wide(subkey);
        let mut handle = HKEY::default();
        let rc = unsafe { RegOpenKeyExW(root, PCWSTR(sub.as_ptr()), 0, sam, &mut handle) };
        if rc == ERROR_SUCCESS {
            Ok(RegKey(handle))
        } else {
            Err(rc)
        }
    }

    /// Opens the key, creating it (and any missing parents) if needed.
    fn create(root: HKEY, subkey: &str, sam: REG_SAM_FLAGS) -> Result<RegKey, WIN32_ERROR> {
        let sub = wide(subkey);
        let mut handle = HKEY::default();
        let rc = unsafe {
            RegCreateKeyExW(
                root,
                PCWSTR(sub.as_ptr()),
                0,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                sam,
                None,
                &mut handle,
                None,
            )
        };
        if rc == ERROR_SUCCESS {
            Ok(RegKey(handle))
        } else {
            Err(rc)
        }
    }

    /// All value names in this key (the unnamed default value comes back as "").
    fn value_names(&self) -> Vec<String> {
        // 16383 characters is the documented maximum value-name length.
        let mut buf = vec![0u16; 16_384];
        let mut names = Vec::new();
        let mut index = 0u32;
        loop {
            let mut len = buf.len() as u32;
            let rc = unsafe {
                RegEnumValueW(
                    self.0,
                    index,
                    PWSTR(buf.as_mut_ptr()),
                    &mut len,
                    None,
                    None,
                    None,
                    None,
                )
            };
            if rc == ERROR_NO_MORE_ITEMS {
                break;
            }
            if rc == ERROR_SUCCESS {
                let len = (len as usize).min(buf.len());
                names.push(String::from_utf16_lossy(&buf[..len]));
            } else if rc != ERROR_MORE_DATA {
                // Unexpected failure: stop rather than loop over a broken key.
                break;
            }
            index += 1;
            if index > 65_536 {
                break;
            }
        }
        names
    }

    /// Reads a value's type and raw bytes.
    fn query(&self, name: &str) -> Result<(REG_VALUE_TYPE, Vec<u8>), WIN32_ERROR> {
        let wname = wide(name);
        for _ in 0..4 {
            let mut size = 0u32;
            let mut ty = REG_VALUE_TYPE::default();
            let rc = unsafe {
                RegQueryValueExW(
                    self.0,
                    PCWSTR(wname.as_ptr()),
                    None,
                    Some(&mut ty),
                    None,
                    Some(&mut size),
                )
            };
            if rc != ERROR_SUCCESS {
                return Err(rc);
            }
            if size == 0 {
                return Ok((ty, Vec::new()));
            }
            let mut data = vec![0u8; size as usize];
            let mut got = size;
            let rc = unsafe {
                RegQueryValueExW(
                    self.0,
                    PCWSTR(wname.as_ptr()),
                    None,
                    Some(&mut ty),
                    Some(data.as_mut_ptr()),
                    Some(&mut got),
                )
            };
            if rc == ERROR_SUCCESS {
                data.truncate(got as usize);
                return Ok((ty, data));
            }
            if rc != ERROR_MORE_DATA {
                return Err(rc);
            }
            // The value grew between the two calls; try again.
        }
        Err(ERROR_MORE_DATA)
    }

    fn exists(&self, name: &str) -> bool {
        let wname = wide(name);
        let mut size = 0u32;
        let rc = unsafe {
            RegQueryValueExW(
                self.0,
                PCWSTR(wname.as_ptr()),
                None,
                None,
                None,
                Some(&mut size),
            )
        };
        rc == ERROR_SUCCESS
    }

    fn set_binary(&self, name: &str, data: &[u8]) -> Result<(), WIN32_ERROR> {
        let wname = wide(name);
        let rc = unsafe { RegSetValueExW(self.0, PCWSTR(wname.as_ptr()), 0, REG_BINARY, Some(data)) };
        if rc == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(rc)
        }
    }

    /// Writes a REG_SZ value (UTF-16LE with the terminating NUL).
    fn set_string(&self, name: &str, value: &str) -> Result<(), WIN32_ERROR> {
        let wname = wide(name);
        let data: Vec<u8> = wide(value).iter().flat_map(|u| u.to_le_bytes()).collect();
        let rc = unsafe { RegSetValueExW(self.0, PCWSTR(wname.as_ptr()), 0, REG_SZ, Some(&data)) };
        if rc == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(rc)
        }
    }

    /// Deletes a value; a value that is already gone counts as deleted.
    fn delete_value(&self, name: &str) -> Result<(), WIN32_ERROR> {
        let wname = wide(name);
        let rc = unsafe { RegDeleteValueW(self.0, PCWSTR(wname.as_ptr())) };
        if rc == ERROR_SUCCESS || rc == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            Err(rc)
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Decodes a REG_SZ / REG_EXPAND_SZ payload (UTF-16LE, usually NUL-terminated).
fn reg_string(data: &[u8]) -> String {
    let units: Vec<u16> = data
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end])
}

fn reg_error(code: WIN32_ERROR, what: &str) -> String {
    if code == ERROR_ACCESS_DENIED {
        ACCESS_DENIED_MSG.to_string()
    } else {
        format!("{what} (error {}).", code.0)
    }
}

// ---------------------------------------------------------------------------
// Shell / environment helpers
// ---------------------------------------------------------------------------

fn known_folder(id: &GUID) -> Option<PathBuf> {
    unsafe {
        let raw = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, HANDLE::default()).ok()?;
        let text = raw.to_string().ok();
        CoTaskMemFree(Some(raw.0 as *const core::ffi::c_void));
        text.filter(|s| !s.is_empty()).map(PathBuf::from)
    }
}

/// Expands `%VAR%` references with ExpandEnvironmentStringsW.
pub(crate) fn expand_env(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let src = wide(s);
    let mut buf = vec![0u16; (s.len() + 1).max(512)];
    for _ in 0..4 {
        let needed =
            unsafe { ExpandEnvironmentStringsW(PCWSTR(src.as_ptr()), Some(buf.as_mut_slice())) }
                as usize;
        if needed == 0 {
            return s.to_string();
        }
        if needed <= buf.len() {
            let end = buf[..needed].iter().position(|&u| u == 0).unwrap_or(needed);
            return String::from_utf16_lossy(&buf[..end]);
        }
        buf.resize(needed, 0);
    }
    s.to_string()
}

/// Pulls the executable out of a Run command line: a quoted path, or the first
/// token ending in `.exe` (unquoted paths may contain spaces), or the first word.
fn exe_from_command(command: &str) -> Option<String> {
    let expanded = expand_env(command.trim());
    let s = expanded.trim();
    if s.is_empty() {
        return None;
    }
    let candidate: String = if let Some(rest) = s.strip_prefix('"') {
        rest.split('"').next().unwrap_or("").trim().to_string()
    } else {
        let lower = s.to_ascii_lowercase();
        let mut found = None;
        let mut from = 0usize;
        while let Some(idx) = lower[from..].find(".exe") {
            let end = from + idx + 4;
            let next = lower[end..].chars().next();
            if next.map_or(true, |c| c.is_whitespace() || c == '"' || c == ',') {
                found = Some(end);
                break;
            }
            from = end;
        }
        match found {
            Some(end) => s[..end].trim().to_string(),
            None => s.split_whitespace().next().unwrap_or("").to_string(),
        }
    };
    resolve_exe_path(&candidate)
}

/// Returns an absolute path, resolving bare names (e.g. `rundll32.exe`) against
/// System32 and the Windows directory. Relative paths that cannot be resolved give None.
fn resolve_exe_path(candidate: &str) -> Option<String> {
    let p = candidate.trim().trim_matches('"').trim();
    if p.is_empty() {
        return None;
    }
    if Path::new(p).is_absolute() || p.starts_with(r"\\") {
        return Some(p.to_string());
    }
    if p.contains('\\') || p.contains('/') {
        return None;
    }
    let file_name = if Path::new(p).extension().is_none() {
        format!("{p}.exe")
    } else {
        p.to_string()
    };
    let windir = std::env::var("SystemRoot")
        .or_else(|_| std::env::var("windir"))
        .unwrap_or_else(|_| r"C:\Windows".to_string());
    let dirs = [Path::new(&windir).join("System32"), PathBuf::from(&windir)];
    dirs.iter()
        .map(|d| d.join(&file_name))
        .find(|c| c.is_file())
        .map(|c| c.to_string_lossy().into_owned())
}

// ---------------------------------------------------------------------------
// .lnk target (MS-SHLLINK LinkInfo LocalBasePath + CommonPathSuffix)
// ---------------------------------------------------------------------------

fn u16_at(data: &[u8], off: usize) -> Option<u16> {
    let b = data.get(off..off.checked_add(2)?)?;
    Some(u16::from_le_bytes([b[0], b[1]]))
}

fn u32_at(data: &[u8], off: usize) -> Option<u32> {
    let b = data.get(off..off.checked_add(4)?)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// NUL-terminated ASCII string at `off`; None if it holds non-ASCII bytes
/// (those depend on the system code page, so we do not guess).
fn ascii_z(data: &[u8], off: usize) -> Option<String> {
    let tail = data.get(off..)?;
    let end = tail.iter().position(|&b| b == 0)?;
    let bytes = &tail[..end];
    if bytes.is_ascii() {
        Some(String::from_utf8_lossy(bytes).into_owned())
    } else {
        None
    }
}

/// NUL-terminated UTF-16LE string at `off`.
fn utf16_z(data: &[u8], off: usize) -> Option<String> {
    let tail = data.get(off..)?;
    let mut units = Vec::new();
    for c in tail.chunks_exact(2) {
        let u = u16::from_le_bytes([c[0], c[1]]);
        if u == 0 {
            return Some(String::from_utf16_lossy(&units));
        }
        units.push(u);
    }
    None
}

/// Resolves a shortcut to its local target path by reading the LinkInfo block.
/// Returns None for shortcuts without a local path (advertised MSI shortcuts,
/// shell-namespace targets, network-only links).
fn lnk_target(path: &Path) -> Option<String> {
    let data = std::fs::read(path).ok()?;
    if u32_at(&data, 0)? != 0x4C {
        return None;
    }
    let link_flags = u32_at(&data, 0x14)?;
    const HAS_LINK_TARGET_ID_LIST: u32 = 0x1;
    const HAS_LINK_INFO: u32 = 0x2;
    let mut off = 0x4Cusize;
    if link_flags & HAS_LINK_TARGET_ID_LIST != 0 {
        let id_list_size = u16_at(&data, off)? as usize;
        off = off.checked_add(2)?.checked_add(id_list_size)?;
    }
    if link_flags & HAS_LINK_INFO == 0 {
        return None;
    }
    let info_size = u32_at(&data, off)? as usize;
    if info_size < 0x1C {
        return None;
    }
    let info = data.get(off..off.checked_add(info_size)?)?;
    let header_size = u32_at(info, 4)? as usize;
    let info_flags = u32_at(info, 8)?;
    const VOLUME_ID_AND_LOCAL_BASE_PATH: u32 = 0x1;
    if info_flags & VOLUME_ID_AND_LOCAL_BASE_PATH == 0 {
        return None;
    }
    let base_off = u32_at(info, 0x10)? as usize;
    let suffix_off = u32_at(info, 0x18)? as usize;

    let mut base = None;
    let mut suffix = None;
    if header_size >= 0x24 {
        let base_u = u32_at(info, 0x1C)? as usize;
        let suffix_u = u32_at(info, 0x20)? as usize;
        if base_u != 0 {
            base = utf16_z(info, base_u);
        }
        if suffix_u != 0 {
            suffix = utf16_z(info, suffix_u);
        }
    }
    let base = match base {
        Some(b) => b,
        None => ascii_z(info, base_off)?,
    };
    let suffix = suffix
        .or_else(|| if suffix_off != 0 { ascii_z(info, suffix_off) } else { None })
        .unwrap_or_default();

    let mut target = base;
    if !suffix.is_empty() {
        if !target.ends_with('\\') {
            target.push('\\');
        }
        target.push_str(&suffix);
    }
    let target = target.trim().to_string();
    if target.is_empty() {
        None
    } else {
        Some(target)
    }
}

// ---------------------------------------------------------------------------
// Collection
// ---------------------------------------------------------------------------

/// Lower-cased value name -> enabled, from the matching StartupApproved key.
/// First byte even (0x02, 0x06) = enabled, odd (0x03, 0x07) = disabled.
fn approved_states(source: Source) -> HashMap<String, bool> {
    let mut states = HashMap::new();
    let (root, sub) = source.approved_key();
    let Ok(key) = RegKey::open(root, sub, KEY_READ | KEY_WOW64_64KEY) else {
        return states;
    };
    for name in key.value_names() {
        if let Ok((_, data)) = key.query(&name) {
            let enabled = data.first().map_or(true, |b| b % 2 == 0);
            states.insert(name.to_lowercase(), enabled);
        }
    }
    states
}

fn registry_entries(source: Source, out: &mut Vec<StartupEntry>) {
    let Some((root, sub)) = source.run_key() else {
        return;
    };
    let Ok(key) = RegKey::open(root, sub, KEY_READ | KEY_WOW64_64KEY) else {
        return;
    };
    let approved = approved_states(source);
    let mut entries = Vec::new();
    for name in key.value_names() {
        if name.is_empty() {
            continue;
        }
        let Ok((ty, data)) = key.query(&name) else {
            continue;
        };
        if ty != REG_SZ && ty != REG_EXPAND_SZ {
            continue;
        }
        let command = reg_string(&data);
        if command.trim().is_empty() {
            continue;
        }
        let enabled = approved.get(&name.to_lowercase()).copied().unwrap_or(true);
        let path = exe_from_command(&command);
        entries.push(StartupEntry {
            id: format!("{}{}", source.prefix(), name),
            name,
            command,
            location: source.location().to_string(),
            enabled,
            path,
            needs_admin: source.needs_admin(),
            impact: None,
            impact_measured_at: None,
            packaged_state: None,
        });
    }
    entries.sort_by_cached_key(|e| e.name.to_lowercase());
    out.extend(entries);
}

fn folder_entries(source: Source, out: &mut Vec<StartupEntry>) {
    let Some(dir) = source.folder() else {
        return;
    };
    let Ok(read_dir) = std::fs::read_dir(&dir) else {
        return;
    };
    let approved = approved_states(source);
    let mut entries = Vec::new();
    for item in read_dir.flatten() {
        let Ok(file_type) = item.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            continue;
        }
        let file_name = item.file_name().to_string_lossy().into_owned();
        if file_name.eq_ignore_ascii_case("desktop.ini") {
            continue;
        }
        let full = item.path();
        let is_lnk = full
            .extension()
            .map_or(false, |e| e.eq_ignore_ascii_case("lnk"));
        let name = if is_lnk && file_name.len() > 4 {
            file_name[..file_name.len() - 4].to_string()
        } else {
            file_name.clone()
        };
        let full_text = full.to_string_lossy().into_owned();
        let target = if is_lnk { lnk_target(&full) } else { None };
        let command = target.clone().unwrap_or_else(|| full_text.clone());
        let path = Some(target.unwrap_or(full_text));
        let enabled = approved
            .get(&file_name.to_lowercase())
            .copied()
            .unwrap_or(true);
        entries.push(StartupEntry {
            id: format!("{}{}", source.prefix(), file_name),
            name,
            command,
            location: source.location().to_string(),
            enabled,
            path,
            needs_admin: source.needs_admin(),
            impact: None,
            impact_measured_at: None,
            packaged_state: None,
        });
    }
    entries.sort_by_cached_key(|e| e.name.to_lowercase());
    out.extend(entries);
}

/// Every startup entry, without impact (also used by the measurement run).
pub(crate) fn collect_startup() -> Vec<StartupEntry> {
    let mut out = Vec::new();
    for source in ALL_SOURCES {
        if source.run_key().is_some() {
            registry_entries(source, &mut out);
        } else {
            folder_entries(source, &mut out);
        }
    }
    out
}

fn parse_id(id: &str) -> Option<(Source, &str)> {
    ALL_SOURCES.iter().find_map(|&source| {
        id.strip_prefix(source.prefix())
            .filter(|rest| !rest.is_empty())
            .map(|rest| (source, rest))
    })
}

/// Current time as a Windows FILETIME (100 ns ticks since 1601-01-01 UTC),
/// the same value GetSystemTimeAsFileTime returns.
fn filetime_now() -> u64 {
    let since_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (since_unix.as_secs() + FILETIME_UNIX_OFFSET_SECS) * 10_000_000
        + u64::from(since_unix.subsec_nanos() / 100)
}

/// The 12-byte StartupApproved payload: enabled = 02 + 11 zero bytes,
/// disabled = 03 00 00 00 + FILETIME of when it was disabled.
fn approved_payload(enabled: bool) -> [u8; 12] {
    let mut data = [0u8; 12];
    if enabled {
        data[0] = 0x02;
    } else {
        data[0] = 0x03;
        data[4..12].copy_from_slice(&filetime_now().to_le_bytes());
    }
    data
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Startup entries with the impact from Orbit's last measurement (impact.rs),
/// then this user's Store app startup tasks (read only, `packagedState` set).
/// Only `collect_startup` feeds Debloat, the suggestions and the measurement, so
/// those never see the packaged rows.
#[tauri::command]
pub async fn list_startup() -> Result<Vec<StartupEntry>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let mut list = collect_startup();
        crate::impact::apply(&mut list, crate::impact::load().as_ref());
        list.extend(packaged::collect_packaged_startup(true));
        watch::note_list(&list);
        list
    })
    .await
    .map_err(|e| format!("Could not read startup entries: {e}"))
}

// ---------------------------------------------------------------------------
// Startup impact: one-shot RunOnce value that runs `orbit.exe --measure-startup`
// at the next sign-in. Windows deletes RunOnce values before running them, so
// nothing stays behind.
// ---------------------------------------------------------------------------

const RUNONCE_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\RunOnce";
const RUNONCE_VALUE: &str = "OrbitStartupImpact";

fn measure_command() -> Result<String, String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("Could not find Orbit's executable: {e}."))?;
    let exe = exe.to_string_lossy();
    let exe = exe.strip_prefix(r"\\?\").unwrap_or(&exe);
    Ok(format!("\"{exe}\" {}", crate::impact::MEASURE_FLAG))
}

/// True when a measurement is scheduled for the next sign-in.
#[tauri::command]
pub fn get_startup_measure() -> Result<bool, String> {
    match RegKey::open(HKEY_CURRENT_USER, RUNONCE_KEY, KEY_QUERY_VALUE | KEY_WOW64_64KEY) {
        Ok(key) => Ok(key.exists(RUNONCE_VALUE)),
        Err(e) if e == ERROR_FILE_NOT_FOUND => Ok(false),
        Err(e) => Err(reg_error(e, "Could not read the RunOnce key")),
    }
}

#[tauri::command]
pub fn schedule_startup_measure() -> Result<(), String> {
    let command = measure_command()?;
    let key = RegKey::create(
        HKEY_CURRENT_USER,
        RUNONCE_KEY,
        KEY_SET_VALUE | KEY_QUERY_VALUE | KEY_WOW64_64KEY,
    )
    .map_err(|e| reg_error(e, "Could not open the RunOnce key"))?;
    key.set_string(RUNONCE_VALUE, &command)
        .map_err(|e| reg_error(e, "Could not schedule the measurement"))
}

#[tauri::command]
pub fn cancel_startup_measure() -> Result<(), String> {
    let key = match RegKey::open(HKEY_CURRENT_USER, RUNONCE_KEY, KEY_SET_VALUE | KEY_WOW64_64KEY) {
        Ok(key) => key,
        Err(e) if e == ERROR_FILE_NOT_FOUND => return Ok(()),
        Err(e) => return Err(reg_error(e, "Could not open the RunOnce key")),
    };
    key.delete_value(RUNONCE_VALUE)
        .map_err(|e| reg_error(e, "Could not cancel the measurement"))
}

#[tauri::command]
pub fn set_startup_enabled(id: String, enabled: bool) -> Result<(), String> {
    if id.starts_with(packaged::PREFIX) {
        return Err("Windows doesn't let other apps change a Store app's startup. Use Startup settings.".to_string());
    }
    let (source, value_name) =
        parse_id(&id).ok_or_else(|| "Unknown startup entry.".to_string())?;

    // Make sure the entry still exists before writing its approval state.
    match source.run_key() {
        Some((root, sub)) => {
            let key = RegKey::open(root, sub, KEY_QUERY_VALUE | KEY_WOW64_64KEY).map_err(|e| {
                if e == ERROR_FILE_NOT_FOUND {
                    "This startup entry no longer exists. Refresh the list.".to_string()
                } else {
                    reg_error(e, "Could not open the Run key")
                }
            })?;
            if !key.exists(value_name) {
                return Err("This startup entry no longer exists. Refresh the list.".to_string());
            }
        }
        None => {
            if value_name.contains('\\') || value_name.contains('/') || value_name == ".." {
                return Err("Unknown startup entry.".to_string());
            }
            let dir = source
                .folder()
                .ok_or_else(|| "Could not find the Startup folder.".to_string())?;
            if !dir.join(value_name).exists() {
                return Err("This startup entry no longer exists. Refresh the list.".to_string());
            }
        }
    }

    let (root, sub) = source.approved_key();
    let key = RegKey::create(root, sub, KEY_SET_VALUE | KEY_QUERY_VALUE | KEY_WOW64_64KEY)
        .map_err(|e| reg_error(e, "Could not open the StartupApproved key"))?;
    key.set_binary(value_name, &approved_payload(enabled))
        .map_err(|e| reg_error(e, "Could not change the startup entry"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ids() {
        assert!(matches!(parse_id("HKCU\\Run\\Discord"), Some((Source::HkcuRun, "Discord"))));
        assert!(matches!(parse_id("HKLM\\Run32\\X"), Some((Source::HklmRun32, "X"))));
        assert!(matches!(parse_id("HKLM\\Run\\A\\B"), Some((Source::HklmRun, "A\\B"))));
        assert!(matches!(parse_id("UserFolder\\X.lnk"), Some((Source::UserFolder, "X.lnk"))));
        assert!(parse_id("HKCU\\Run\\").is_none());
        assert!(parse_id("Nope\\X").is_none());
    }

    #[test]
    fn extracts_exe() {
        assert_eq!(
            exe_from_command(r#""C:\Program Files\App\app.exe" --min"#).as_deref(),
            Some(r"C:\Program Files\App\app.exe")
        );
        assert_eq!(
            exe_from_command(r"C:\Program Files\App\app.exe -silent").as_deref(),
            Some(r"C:\Program Files\App\app.exe")
        );
        assert_eq!(exe_from_command("relative\\thing.exe"), None);
    }

    #[test]
    fn payloads() {
        assert_eq!(approved_payload(true), [2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let d = approved_payload(false);
        assert_eq!(&d[..4], &[3, 0, 0, 0]);
        assert_ne!(&d[4..], &[0u8; 8]);
    }
}
