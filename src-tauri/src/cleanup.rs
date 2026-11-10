//! Disk cleanup and "biggest folders" (Optimize > Cleanup, 0.5, feature 7c).
//!
//! Cleanup works on a fixed list of junk categories (user and Windows temp,
//! the Windows Update download cache, crash dumps, the thumbnail cache, four
//! browsers' cache folders and the Recycle Bin). The folders behind each
//! category are computed here, in Rust, from known folders; the UI only ever
//! sends category ids, never paths.
//!
//! Safety rules for deleting (all unit-tested below):
//! - Only inside a category root, never the root itself. Every file and
//!   folder is opened by handle without following reparse points
//!   (`FILE_FLAG_OPEN_REPARSE_POINT`), its real location is read back with
//!   `GetFinalPathNameByHandleW` and it is deleted through that same handle
//!   only if that location is strictly inside the root's real location. So a
//!   junction or symlink swapped in during a clean cannot redirect it.
//! - Reparse points (junctions, symlinks, cloud placeholders) are never
//!   entered and never deleted.
//! - A root is refused when it is a reparse point, a drive root, too shallow,
//!   or equal to / a parent of a protected folder (the user profile, Documents,
//!   Windows, Program Files, ...). User temp must also have a "Temp"/"Tmp"
//!   component, so a broken %TEMP% can't point the clean at the profile.
//! - Files in use, read-only files and files Orbit may not delete are
//!   skipped and counted, never forced.
//! - "Only files older than 24 hours" (temp folders and the update cache)
//!   looks at the newer of the created and modified times, so files an
//!   installer is unpacking right now stay.
//! - Browser caches are skipped while that browser runs, and only the cache
//!   folders are touched (never profiles, cookies, history or passwords).
//!
//! Testing: debug builds honour `ORBIT_CLEANUP_ROOT`, which maps every
//! category root into that folder (and turns the Recycle Bin into a plain
//! folder there). Release builds ignore it. Tests build that test layout with
//! `Layout::under` and never clean the real one.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use windows::core::{GUID, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, BOOLEAN, HANDLE, HWND};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FileDispositionInfo, GetFileInformationByHandle, GetFinalPathNameByHandleW,
    SetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ATTRIBUTE_DIRECTORY,
    FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_DISPOSITION_INFO,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_NAME_NORMALIZED,
    FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    GETFINALPATHNAMEBYHANDLE_FLAGS, OPEN_EXISTING,
    VOLUME_NAME_DOS,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::SystemInformation::GetSystemWindowsDirectoryW;
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_LocalAppData,
    FOLDERID_Music, FOLDERID_Pictures, FOLDERID_Profile, FOLDERID_ProgramData,
    FOLDERID_ProgramFiles, FOLDERID_ProgramFilesX86, FOLDERID_RoamingAppData, FOLDERID_System,
    FOLDERID_Videos, SHEmptyRecycleBinW, SHGetKnownFolderPath, SHQueryRecycleBinW,
    KF_FLAG_DEFAULT, SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI, SHERB_NOSOUND, SHQUERYRBINFO,
};

// ---------------------------------------------------------------------------
// Shapes (docs/CONTRACT.md "v0.5")
// ---------------------------------------------------------------------------

/// One junk category as `cleanup_scan` found it.
#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct CategoryScan {
    pub id: String,
    pub label: String,
    pub description: String,
    pub needs_admin: bool,
    /// "Only files older than 24 hours" applies to this category.
    pub age_filter: bool,
    /// The folders behind it, for display.
    pub paths: Vec<String>,
    /// At least one of its folders exists.
    pub found: bool,
    /// False when a folder could not be listed (usually: needs admin).
    pub readable: bool,
    pub bytes: u64,
    pub files: u64,
    /// What the 24-hour rule would leave to clean (equals bytes/files when
    /// `age_filter` is false).
    pub old_bytes: u64,
    pub old_files: u64,
    /// Subfolders that could not be listed.
    pub unreadable: u64,
    /// Why this category can't be cleaned right now ("Edge is open. ...").
    pub blocked: Option<String>,
    /// Extra plain sentence, e.g. why a folder was left out.
    pub note: Option<String>,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct CleanupScan {
    /// Unix seconds.
    pub scanned_at: u64,
    pub is_admin: bool,
    pub categories: Vec<CategoryScan>,
    pub cancelled: bool,
    /// Set when ORBIT_CLEANUP_ROOT remapped every root (debug builds only).
    pub test_root: Option<String>,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CategoryClean {
    pub id: String,
    pub label: String,
    pub freed_bytes: u64,
    pub deleted_files: u64,
    /// Open in another program.
    pub skipped_in_use: u64,
    /// Access denied or read-only.
    pub skipped_denied: u64,
    /// Links, files that moved outside the folder, other errors.
    pub skipped_other: u64,
    /// Newer than 24 hours, kept by the age rule.
    pub kept_new: u64,
    /// The whole category was skipped, and why.
    pub skipped: Option<String>,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct CleanResult {
    pub freed_bytes: u64,
    pub deleted_files: u64,
    pub skipped_in_use: u64,
    pub skipped_denied: u64,
    pub skipped_other: u64,
    pub kept_new: u64,
    pub cancelled: bool,
    pub categories: Vec<CategoryClean>,
}

/// Payload of the `orbit://cleanup-progress` event.
#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    /// "scan" | "clean" | "folders"
    pub job: String,
    /// What is being worked on: a category label or a folder name.
    pub label: String,
    /// 1-based step and step count (categories, or top-level folders).
    pub step: u32,
    pub steps: u32,
    /// Files counted (scan, folders) or deleted (clean) so far.
    pub files: u64,
    pub bytes: u64,
    pub done: bool,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct FolderEntry {
    pub name: String,
    pub path: String,
    pub bytes: u64,
    pub files: u64,
    /// Subfolders inside it that could not be listed.
    pub unreadable: u64,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct FolderSizes {
    pub path: String,
    pub bytes: u64,
    pub files: u64,
    /// Biggest subfolders first, at most `TOP_FOLDERS`.
    pub folders: Vec<FolderEntry>,
    /// Subfolders past the top list, summed.
    pub other_bytes: u64,
    pub other_count: u64,
    /// Files directly in this folder.
    pub files_here_bytes: u64,
    pub files_here: u64,
    pub unreadable: u64,
    /// Junctions / symlinks not followed.
    pub links: u64,
    pub cancelled: bool,
    pub scanned_at: u64,
    /// Answered from the last scan of a parent folder.
    pub cached: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DriveInfo {
    /// "C:\\"
    pub mount: String,
    pub name: String,
    pub total: u64,
    pub available: u64,
    pub removable: bool,
}

// ---------------------------------------------------------------------------
// Categories
// ---------------------------------------------------------------------------

pub struct CatDef {
    id: &'static str,
    label: &'static str,
    description: &'static str,
    needs_admin: bool,
    age_filter: bool,
    /// (display name, exe) for browser caches: skipped while it runs.
    browser: Option<(&'static str, &'static str)>,
}

const BROWSER_NOTE: &str =
    "Cached pages and images only. Cookies, history, passwords and settings are never touched.";

const CATEGORIES: &[CatDef] = &[
    CatDef {
        id: "user-temp",
        label: "Your temporary files",
        description: "Files apps leave in your Temp folder.",
        needs_admin: false,
        age_filter: true,
        browser: None,
    },
    CatDef {
        id: "windows-temp",
        label: "Windows temporary files",
        description: "The Windows Temp folder, used by installers and services.",
        needs_admin: true,
        age_filter: true,
        browser: None,
    },
    CatDef {
        id: "update-cache",
        label: "Windows Update downloads",
        description: "Update files Windows already downloaded. It fetches them again if it still needs them.",
        needs_admin: true,
        age_filter: true,
        browser: None,
    },
    CatDef {
        id: "crash-dumps",
        label: "App crash dumps",
        description: "Memory snapshots apps save when they crash.",
        needs_admin: false,
        age_filter: false,
        browser: None,
    },
    CatDef {
        id: "system-dumps",
        label: "Windows crash dumps",
        description: "Minidumps and MEMORY.DMP from blue screens.",
        needs_admin: true,
        age_filter: false,
        browser: None,
    },
    CatDef {
        id: "thumbnails",
        label: "Thumbnail cache",
        description: "Picture previews File Explorer saves. It rebuilds them; the ones it has open are skipped.",
        needs_admin: false,
        age_filter: false,
        browser: None,
    },
    CatDef {
        id: "browser-edge",
        label: "Microsoft Edge cache",
        description: BROWSER_NOTE,
        needs_admin: false,
        age_filter: false,
        browser: Some(("Edge", "msedge.exe")),
    },
    CatDef {
        id: "browser-chrome",
        label: "Google Chrome cache",
        description: BROWSER_NOTE,
        needs_admin: false,
        age_filter: false,
        browser: Some(("Chrome", "chrome.exe")),
    },
    CatDef {
        id: "browser-brave",
        label: "Brave cache",
        description: BROWSER_NOTE,
        needs_admin: false,
        age_filter: false,
        browser: Some(("Brave", "brave.exe")),
    },
    CatDef {
        id: "browser-firefox",
        label: "Firefox cache",
        description: BROWSER_NOTE,
        needs_admin: false,
        age_filter: false,
        browser: Some(("Firefox", "firefox.exe")),
    },
    CatDef {
        id: "recycle-bin",
        label: "Recycle Bin",
        description: "Files you deleted. Emptying it removes them for good.",
        needs_admin: false,
        age_filter: false,
        browser: None,
    },
];

fn category(id: &str) -> Option<&'static CatDef> {
    CATEGORIES.iter().find(|c| c.id == id)
}

/// Checks and de-duplicates the ids the UI sent. Unknown ids are an error,
/// so a typo can't silently clean less (or anything else).
pub fn validate_ids(ids: &[String]) -> Result<Vec<&'static CatDef>, String> {
    let mut out: Vec<&'static CatDef> = Vec::new();
    for id in ids {
        let def = category(id).ok_or_else(|| "Unknown cleanup category.".to_string())?;
        if !out.iter().any(|d| d.id == def.id) {
            out.push(def);
        }
    }
    if out.is_empty() {
        return Err("Pick at least one category.".to_string());
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Layout: where each category's folders are
// ---------------------------------------------------------------------------

/// The base folders every category root is built from.
#[derive(Clone, Debug)]
pub struct Layout {
    user_temp: PathBuf,
    windows: PathBuf,
    local: PathBuf,
    /// Some: the test layout under this folder (ORBIT_CLEANUP_ROOT).
    test_root: Option<PathBuf>,
    /// Folders a root may never be, or contain.
    protected: Vec<String>,
}

impl Layout {
    /// This PC's real folders.
    pub fn system() -> Result<Layout, String> {
        let windows = system_windows_dir().ok_or("Orbit could not find the Windows folder.")?;
        let local = known_folder(&FOLDERID_LocalAppData)
            .ok_or("Orbit could not find your AppData folder.")?;
        Ok(Layout {
            user_temp: std::env::temp_dir(),
            windows,
            local,
            test_root: None,
            protected: protected_dirs(),
        })
    }

    /// Every root remapped into `root`: `<root>\User\Temp`, `<root>\Windows`,
    /// `<root>\LocalAppData`, and the Recycle Bin is `<root>\RecycleBin`.
    pub fn under(root: &Path) -> Layout {
        let mut protected = protected_dirs();
        protected.push(root.to_string_lossy().into_owned());
        Layout {
            user_temp: root.join("User").join("Temp"),
            windows: root.join("Windows"),
            local: root.join("LocalAppData"),
            test_root: Some(root.to_path_buf()),
            protected,
        }
    }

    /// What the commands use: the test layout when a debug build runs with
    /// ORBIT_CLEANUP_ROOT, otherwise the real one.
    pub fn current() -> Result<Layout, String> {
        #[cfg(debug_assertions)]
        if let Some(root) = std::env::var_os("ORBIT_CLEANUP_ROOT").filter(|v| !v.is_empty()) {
            let root = PathBuf::from(root);
            if !root.is_absolute() {
                return Err("ORBIT_CLEANUP_ROOT must be a full path.".to_string());
            }
            return Ok(Layout::under(&root));
        }
        Layout::system()
    }

    pub fn is_test(&self) -> bool {
        self.test_root.is_some()
    }
}

fn system_windows_dir() -> Option<PathBuf> {
    let mut buf = vec![0u16; 512];
    // SAFETY: the buffer is valid for its length.
    let n = unsafe { GetSystemWindowsDirectoryW(Some(&mut buf)) } as usize;
    if n == 0 || n >= buf.len() {
        return None;
    }
    Some(PathBuf::from(String::from_utf16_lossy(&buf[..n])))
}

fn known_folder(id: &GUID) -> Option<PathBuf> {
    // SAFETY: SHGetKnownFolderPath allocates the string; it is freed below.
    unsafe {
        let raw = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, HANDLE::default()).ok()?;
        let text = raw.to_string().ok();
        CoTaskMemFree(Some(raw.0 as *const core::ffi::c_void));
        text.filter(|s| !s.is_empty()).map(PathBuf::from)
    }
}

/// Folders no cleanup root may equal or contain.
fn protected_dirs() -> Vec<String> {
    let ids = [
        &FOLDERID_Profile,
        &FOLDERID_Desktop,
        &FOLDERID_Documents,
        &FOLDERID_Downloads,
        &FOLDERID_Pictures,
        &FOLDERID_Music,
        &FOLDERID_Videos,
        &FOLDERID_LocalAppData,
        &FOLDERID_RoamingAppData,
        &FOLDERID_ProgramData,
        &FOLDERID_ProgramFiles,
        &FOLDERID_ProgramFilesX86,
        &FOLDERID_System,
    ];
    let mut out: Vec<String> = ids
        .iter()
        .filter_map(|id| known_folder(id))
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    if let Some(w) = system_windows_dir() {
        out.push(w.to_string_lossy().into_owned());
    }
    out
}

/// How a target's files are picked.
#[derive(Clone, Debug, PartialEq)]
pub enum Pattern {
    /// Exactly this name (lowercase).
    Exact(&'static str),
    /// `<prefix>*<suffix>` (lowercase).
    Wrapped(&'static str, &'static str),
}

impl Pattern {
    pub fn matches(&self, name: &str) -> bool {
        let n = name.to_lowercase();
        if n.contains(['\\', '/', ':']) {
            return false;
        }
        match self {
            Pattern::Exact(e) => n == *e,
            Pattern::Wrapped(p, s) => n.len() > p.len() + s.len() && n.starts_with(p) && n.ends_with(s),
        }
    }
}

/// One place a category cleans.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    /// Everything inside this folder, never the folder itself.
    Contents(PathBuf),
    /// Files directly in `dir` whose name matches; nothing else in `dir`.
    Matching { dir: PathBuf, pattern: Pattern },
    /// The real Recycle Bin (Shell API).
    RecycleBin,
}

fn targets(layout: &Layout, id: &str) -> Vec<Target> {
    use Target::*;
    let win = &layout.windows;
    let local = &layout.local;
    match id {
        "user-temp" => vec![Contents(layout.user_temp.clone())],
        "windows-temp" => vec![Contents(win.join("Temp"))],
        "update-cache" => vec![Contents(win.join("SoftwareDistribution").join("Download"))],
        "crash-dumps" => vec![Contents(local.join("CrashDumps"))],
        "system-dumps" => vec![
            Contents(win.join("Minidump")),
            Matching { dir: win.clone(), pattern: Pattern::Exact("memory.dmp") },
        ],
        "thumbnails" => vec![Matching {
            dir: local.join("Microsoft").join("Windows").join("Explorer"),
            pattern: Pattern::Wrapped("thumbcache_", ".db"),
        }],
        "browser-edge" => chromium_targets(&local.join(r"Microsoft\Edge\User Data")),
        "browser-chrome" => chromium_targets(&local.join(r"Google\Chrome\User Data")),
        "browser-brave" => chromium_targets(&local.join(r"BraveSoftware\Brave-Browser\User Data")),
        "browser-firefox" => firefox_targets(&local.join(r"Mozilla\Firefox\Profiles")),
        "recycle-bin" => match &layout.test_root {
            Some(root) => vec![Contents(root.join("RecycleBin"))],
            None => vec![RecycleBin],
        },
        _ => Vec::new(),
    }
}

/// The folders shown under a category (the browser's base folder, not each
/// profile's cache).
fn display_paths(layout: &Layout, id: &str) -> Vec<String> {
    let local = &layout.local;
    let p = |x: PathBuf| vec![x.to_string_lossy().into_owned()];
    match id {
        "browser-edge" => p(local.join(r"Microsoft\Edge\User Data")),
        "browser-chrome" => p(local.join(r"Google\Chrome\User Data")),
        "browser-brave" => p(local.join(r"BraveSoftware\Brave-Browser\User Data")),
        "browser-firefox" => p(local.join(r"Mozilla\Firefox\Profiles")),
        "recycle-bin" if layout.test_root.is_none() => vec!["Recycle Bin (every drive)".to_string()],
        _ => targets(layout, id)
            .into_iter()
            .filter_map(|t| match t {
                Target::Contents(r) => Some(r.to_string_lossy().into_owned()),
                Target::Matching { dir, pattern } => Some(
                    dir.join(match pattern {
                        Pattern::Exact(n) => n.to_string(),
                        Pattern::Wrapped(a, b) => format!("{a}*{b}"),
                    })
                    .to_string_lossy()
                    .into_owned(),
                ),
                Target::RecycleBin => None,
            })
            .collect(),
    }
}

/// A folder that exists and is not a reparse point (no link following).
fn is_real_dir(p: &Path) -> bool {
    fs::symlink_metadata(p)
        .map(|m| m.is_dir() && m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0)
        .unwrap_or(false)
}

/// Chromium keeps one folder per profile (Default, Profile 1, ...), each
/// with a `Preferences` file. Only these cache folders are cleaned:
/// `<profile>\Cache`, `Code Cache`, `GPUCache`, and the shared `ShaderCache`
/// and `GrShaderCache`. Everything else (cookies, history, logins) stays.
fn chromium_targets(base: &Path) -> Vec<Target> {
    let mut out = Vec::new();
    if !is_real_dir(base) {
        return out;
    }
    for top in ["ShaderCache", "GrShaderCache"] {
        let p = base.join(top);
        if is_real_dir(&p) {
            out.push(Target::Contents(p));
        }
    }
    let Ok(rd) = fs::read_dir(base) else { return out };
    let mut profiles: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| is_real_dir(p) && p.join("Preferences").is_file())
        .collect();
    profiles.sort();
    for prof in profiles {
        for sub in ["Cache", "Code Cache", "GPUCache"] {
            let p = prof.join(sub);
            if is_real_dir(&p) {
                out.push(Target::Contents(p));
            }
        }
    }
    out
}

/// Firefox's LocalAppData profiles hold only caches (the real profile is in
/// Roaming); only `<profile>\cache2` is cleaned.
fn firefox_targets(base: &Path) -> Vec<Target> {
    let mut out = Vec::new();
    if !is_real_dir(base) {
        return out;
    }
    let Ok(rd) = fs::read_dir(base) else { return out };
    let mut profiles: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| is_real_dir(p)).collect();
    profiles.sort();
    for prof in profiles {
        let p = prof.join("cache2");
        if is_real_dir(&p) {
            out.push(Target::Contents(p));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Path rules
// ---------------------------------------------------------------------------

/// Splits a path into lowercase components for comparing: verbatim
/// prefixes (`\\?\`, `\\?\UNC\`) dropped, `/` as `\`, empty parts ignored.
/// The first component is the drive ("c:") or the UNC share
/// ("\\server\share"). None for relative paths, `.` / `..` parts, device
/// or volume-GUID paths: those are never compared as "inside".
pub fn path_parts(path: &str) -> Option<Vec<String>> {
    let mut s = path.trim().replace('/', "\\").to_lowercase();
    if let Some(rest) = s.strip_prefix(r"\\?\unc\") {
        s = format!(r"\\{rest}");
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        s = rest.to_string();
    } else if s.starts_with(r"\\.\") {
        return None;
    }
    let (head, rest) = if let Some(unc) = s.strip_prefix(r"\\") {
        let mut it = unc.splitn(3, '\\');
        let server = it.next().unwrap_or("");
        let share = it.next().unwrap_or("");
        if server.is_empty() || share.is_empty() || server == "?" || server == "." {
            return None;
        }
        (format!(r"\\{server}\{share}"), it.next().unwrap_or("").to_string())
    } else {
        let b = s.as_bytes();
        if b.len() < 2 || !b[0].is_ascii_alphabetic() || b[1] != b':' {
            return None;
        }
        if b.len() > 2 && b[2] != b'\\' {
            return None; // "C:foo" is relative to the drive's current folder
        }
        (s[..2].to_string(), s.get(3..).unwrap_or("").to_string())
    };
    let mut parts = vec![head];
    for c in rest.split('\\') {
        match c {
            "" => {}
            "." | ".." => return None,
            c => parts.push(c.to_string()),
        }
    }
    Some(parts)
}

/// True when `candidate` is strictly inside `root` (never `root` itself),
/// compared case-insensitively by whole components.
pub fn is_within(root: &str, candidate: &str) -> bool {
    match (path_parts(root), path_parts(candidate)) {
        (Some(r), Some(c)) => c.len() > r.len() && c[..r.len()] == r[..],
        _ => false,
    }
}

/// `a` equals `b` or contains it.
fn is_same_or_parent(a: &[String], b: &[String]) -> bool {
    b.len() >= a.len() && b[..a.len()] == a[..]
}

/// A cleanup root must be at least two folders below a drive, and must not
/// be (or contain) a protected folder.
pub fn root_allowed(root: &str, protected: &[String]) -> bool {
    let Some(r) = path_parts(root) else { return false };
    if r.len() < 3 {
        return false;
    }
    !protected
        .iter()
        .filter_map(|p| path_parts(p))
        .any(|p| is_same_or_parent(&r, &p))
}

/// %TEMP% must look like a temp folder: one of its parts is "temp" or "tmp".
pub fn looks_like_temp(root: &str) -> bool {
    path_parts(root).is_some_and(|parts| parts.iter().skip(1).any(|p| p == "temp" || p == "tmp"))
}

/// The `\\?\` form, so CreateFileW takes paths over MAX_PATH.
pub fn to_verbatim(path: &str) -> String {
    let p = path.replace('/', "\\");
    if p.starts_with(r"\\?\") || p.starts_with(r"\\.\") {
        p
    } else if let Some(rest) = p.strip_prefix(r"\\") {
        format!(r"\\?\UNC\{rest}")
    } else {
        format!(r"\\?\{p}")
    }
}

/// Why a root is left out, or None when it may be scanned and cleaned.
fn root_problem(layout: &Layout, id: &str, root: &Path) -> Option<&'static str> {
    let r = root.to_string_lossy();
    if !root_allowed(&r, &layout.protected) {
        return Some("Orbit left out a folder that isn't where Windows normally keeps this.");
    }
    if id == "user-temp" && !looks_like_temp(&r) {
        return Some("Your Temp folder setting points somewhere unusual, so Orbit leaves it alone.");
    }
    None
}

// ---------------------------------------------------------------------------
// Handle-based checks and deletes
// ---------------------------------------------------------------------------

struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateFileW and is closed once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Why something was not deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    InUse,
    Denied,
    /// A junction, symlink or other reparse point.
    Link,
    /// Its real location is not inside the root.
    Outside,
    /// Already gone.
    Gone,
    Other,
}

fn win32_code(e: &windows::core::Error) -> u32 {
    let h = e.code().0 as u32;
    if h & 0xFFFF_0000 == 0x8007_0000 {
        h & 0xFFFF
    } else {
        h
    }
}

fn classify(e: &windows::core::Error) -> Skip {
    match win32_code(e) {
        32 | 33 => Skip::InUse, // sharing / lock violation
        5 => Skip::Denied,
        2 | 3 => Skip::Gone,
        _ => Skip::Other,
    }
}

/// Opens without following a final reparse point. `access` is a
/// FILE_ACCESS_RIGHTS mask.
fn open_no_follow(path: &Path, access: u32) -> Result<Owned, Skip> {
    let w = wide(&to_verbatim(&path.to_string_lossy()));
    // SAFETY: `w` is NUL-terminated and outlives the call.
    unsafe {
        CreateFileW(
            PCWSTR(w.as_ptr()),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    }
    .map(Owned)
    .map_err(|e| classify(&e))
}

fn handle_info(h: &Owned) -> Result<BY_HANDLE_FILE_INFORMATION, Skip> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: valid handle and out pointer.
    unsafe { GetFileInformationByHandle(h.0, &mut info) }.map_err(|e| classify(&e))?;
    Ok(info)
}

/// Where the handle really points (`\\?\C:\...`), links resolved.
fn final_path(h: &Owned) -> Result<String, Skip> {
    let mut buf = vec![0u16; 1024];
    for _ in 0..3 {
        // SAFETY: valid handle; the buffer is valid for its length.
        let n = unsafe { GetFinalPathNameByHandleW(h.0, &mut buf, GETFINALPATHNAMEBYHANDLE_FLAGS(FILE_NAME_NORMALIZED.0 | VOLUME_NAME_DOS.0)) } as usize;
        if n == 0 {
            return Err(Skip::Other);
        }
        if n < buf.len() {
            return Ok(String::from_utf16_lossy(&buf[..n]));
        }
        buf.resize(n + 1, 0);
    }
    Err(Skip::Other)
}

/// The root's real location, if it is a plain folder (not a link).
fn open_root(root: &Path) -> Result<String, Skip> {
    let h = open_no_follow(root, FILE_READ_ATTRIBUTES.0)?;
    let info = handle_info(&h)?;
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Err(Skip::Link);
    }
    if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 == 0 {
        return Err(Skip::Other);
    }
    final_path(&h)
}

/// Which entries under a root may go.
#[derive(Clone, Copy)]
enum Rule<'a> {
    /// Anything strictly inside the root.
    Inside,
    /// Only files directly in the root whose name matches.
    DirectChild(&'a Pattern),
}

fn rule_allows(rule: Rule<'_>, root: &str, path: &str) -> bool {
    match rule {
        Rule::Inside => is_within(root, path),
        Rule::DirectChild(pat) => match (path_parts(root), path_parts(path)) {
            (Some(r), Some(c)) => {
                c.len() == r.len() + 1 && c[..r.len()] == r[..] && pat.matches(&c[r.len()])
            }
            _ => false,
        },
    }
}

/// Deletes one file (or, with `dir`, one empty folder) at `path`, only if
/// it is not a link, not read-only, of the expected kind, and both the path
/// we built and its real location are inside the root under `rule`.
/// Returns the file size. The delete goes through the checked handle.
fn delete_checked(
    path: &Path,
    root_given: &Path,
    root_final: &str,
    rule: Rule<'_>,
    dir: bool,
) -> Result<u64, Skip> {
    if !rule_allows(rule, &root_given.to_string_lossy(), &path.to_string_lossy()) {
        return Err(Skip::Outside);
    }
    let h = open_no_follow(path, DELETE.0 | FILE_READ_ATTRIBUTES.0)?;
    let info = handle_info(&h)?;
    let attrs = info.dwFileAttributes;
    if attrs & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Err(Skip::Link);
    }
    if (attrs & FILE_ATTRIBUTE_DIRECTORY.0 != 0) != dir {
        return Err(Skip::Other);
    }
    let real = final_path(&h)?;
    if !rule_allows(rule, root_final, &real) {
        return Err(Skip::Outside);
    }
    if attrs & FILE_ATTRIBUTE_READONLY.0 != 0 {
        return Err(Skip::Denied);
    }
    let disp = FILE_DISPOSITION_INFO { DeleteFile: BOOLEAN(1) };
    // SAFETY: valid handle opened with DELETE; the struct outlives the call.
    unsafe {
        SetFileInformationByHandle(
            h.0,
            FileDispositionInfo,
            &disp as *const FILE_DISPOSITION_INFO as *const core::ffi::c_void,
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    }
    .map_err(|e| classify(&e))?;
    Ok((u64::from(info.nFileSizeHigh) << 32) | u64::from(info.nFileSizeLow))
}

// ---------------------------------------------------------------------------
// Progress, cancel, one job of each kind at a time
// ---------------------------------------------------------------------------

pub const PROGRESS_EVENT: &str = "orbit://cleanup-progress";
const EMIT_EVERY: Duration = Duration::from_millis(120);

pub struct Ctl<'a> {
    cancel: &'a AtomicBool,
    sink: &'a mut dyn FnMut(&Progress),
    p: Progress,
    last: Option<Instant>,
}

impl<'a> Ctl<'a> {
    pub fn new(job: &str, cancel: &'a AtomicBool, sink: &'a mut dyn FnMut(&Progress)) -> Self {
        Ctl { cancel, sink, p: Progress { job: job.to_string(), ..Default::default() }, last: None }
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn step(&mut self, label: &str, step: u32, steps: u32) {
        self.p.label = label.to_string();
        self.p.step = step;
        self.p.steps = steps;
        self.emit(true);
    }

    fn add(&mut self, files: u64, bytes: u64) {
        self.p.files += files;
        self.p.bytes += bytes;
        self.emit(false);
    }

    fn finish(&mut self) {
        self.p.done = true;
        self.emit(true);
    }

    fn emit(&mut self, force: bool) {
        let now = Instant::now();
        if force || self.last.is_none_or(|t| now.duration_since(t) >= EMIT_EVERY) {
            self.last = Some(now);
            (self.sink)(&self.p);
        }
    }
}

pub struct Job {
    running: AtomicBool,
    cancel: AtomicBool,
}

impl Job {
    const fn new() -> Self {
        Job { running: AtomicBool::new(false), cancel: AtomicBool::new(false) }
    }

    fn start(&'static self) -> Option<JobGuard> {
        if self.running.swap(true, Ordering::AcqRel) {
            return None;
        }
        self.cancel.store(false, Ordering::Relaxed);
        Some(JobGuard(self))
    }
}

struct JobGuard(&'static Job);

impl Drop for JobGuard {
    fn drop(&mut self) {
        self.0.running.store(false, Ordering::Release);
    }
}

static SCAN_JOB: Job = Job::new();
static CLEAN_JOB: Job = Job::new();
static FOLDERS_JOB: Job = Job::new();

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn is_admin() -> bool {
    // SAFETY: no arguments; inspects the current token only.
    unsafe { windows::Win32::UI::Shell::IsUserAnAdmin() }.as_bool()
}

/// Lowercased exe names of running processes.
fn running_exes() -> HashSet<String> {
    crate::actions::snapshot_processes()
        .map(|v| v.into_iter().map(|p| p.name.to_lowercase()).collect())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Scan (read only)
// ---------------------------------------------------------------------------

/// Kept by the 24-hour rule: the newer of created and modified is after the
/// cutoff (or unknown).
fn is_new(md: &fs::Metadata, cutoff: SystemTime) -> bool {
    let newest = match (md.created().ok(), md.modified().ok()) {
        (Some(a), Some(b)) => a.max(b),
        (Some(a), None) | (None, Some(a)) => a,
        (None, None) => return true,
    };
    newest >= cutoff
}

fn is_reparse(md: &fs::Metadata) -> bool {
    md.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
}

#[derive(Default)]
struct Tally {
    bytes: u64,
    files: u64,
    old_bytes: u64,
    old_files: u64,
    unreadable: u64,
}

fn scan_dir(dir: &Path, cutoff: SystemTime, t: &mut Tally, ctl: &mut Ctl<'_>) {
    let Ok(rd) = fs::read_dir(dir) else {
        t.unreadable += 1;
        return;
    };
    for entry in rd.flatten() {
        if ctl.cancelled() {
            return;
        }
        let Ok(md) = entry.metadata() else { continue };
        if is_reparse(&md) {
            continue;
        }
        if md.is_dir() {
            scan_dir(&entry.path(), cutoff, t, ctl);
        } else {
            t.bytes += md.len();
            t.files += 1;
            if !is_new(&md, cutoff) {
                t.old_bytes += md.len();
                t.old_files += 1;
            }
            ctl.add(1, md.len());
        }
    }
}

fn recycle_query() -> Option<(u64, u64)> {
    let mut info = SHQUERYRBINFO { cbSize: std::mem::size_of::<SHQUERYRBINFO>() as u32, ..Default::default() };
    // SAFETY: valid struct with cbSize set; NULL root = every drive.
    unsafe { SHQueryRecycleBinW(PCWSTR::null(), &mut info) }.ok()?;
    Some((info.i64Size.max(0) as u64, info.i64NumItems.max(0) as u64))
}

fn scan_category(
    layout: &Layout,
    def: &CatDef,
    now: SystemTime,
    running: &HashSet<String>,
    ctl: &mut Ctl<'_>,
) -> CategoryScan {
    let cutoff = now.checked_sub(Duration::from_secs(24 * 3600)).unwrap_or(UNIX_EPOCH);
    let mut out = CategoryScan {
        id: def.id.to_string(),
        label: def.label.to_string(),
        description: def.description.to_string(),
        needs_admin: def.needs_admin,
        age_filter: def.age_filter,
        paths: display_paths(layout, def.id),
        readable: true,
        ..Default::default()
    };
    if let Some((name, exe)) = def.browser {
        if running.contains(exe) {
            out.blocked = Some(format!("{name} is open. Close it to clear its cache."));
        }
    }
    let mut t = Tally::default();
    for target in targets(layout, def.id) {
        match target {
            Target::Contents(root) => {
                if let Some(why) = root_problem(layout, def.id, &root) {
                    out.note = Some(why.to_string());
                    continue;
                }
                let Ok(md) = fs::symlink_metadata(&root) else { continue };
                out.found = true;
                if is_reparse(&md) || !md.is_dir() {
                    out.note = Some("A folder here is a link, so Orbit leaves it alone.".to_string());
                    continue;
                }
                if fs::read_dir(&root).is_err() {
                    out.readable = false;
                    continue;
                }
                scan_dir(&root, cutoff, &mut t, ctl);
            }
            Target::Matching { dir, pattern } => {
                let Ok(rd) = fs::read_dir(&dir) else {
                    if dir.exists() {
                        out.found = true;
                        out.readable = false;
                    }
                    continue;
                };
                if !is_real_dir(&dir) {
                    continue;
                }
                for entry in rd.flatten() {
                    let Ok(md) = entry.metadata() else { continue };
                    if md.is_dir() || is_reparse(&md) || !pattern.matches(&entry.file_name().to_string_lossy()) {
                        continue;
                    }
                    out.found = true;
                    t.bytes += md.len();
                    t.files += 1;
                    t.old_bytes += md.len();
                    t.old_files += 1;
                    ctl.add(1, md.len());
                }
            }
            Target::RecycleBin => {
                if let Some((bytes, items)) = recycle_query() {
                    out.found = true;
                    t.bytes += bytes;
                    t.files += items;
                    t.old_bytes += bytes;
                    t.old_files += items;
                    ctl.add(items, bytes);
                } else {
                    out.readable = false;
                }
            }
        }
    }
    out.bytes = t.bytes;
    out.files = t.files;
    out.unreadable = t.unreadable;
    if def.age_filter {
        out.old_bytes = t.old_bytes;
        out.old_files = t.old_files;
    } else {
        out.old_bytes = t.bytes;
        out.old_files = t.files;
    }
    out
}

/// Sizes every category. Read only.
pub fn scan_all(
    layout: &Layout,
    is_admin: bool,
    running: &HashSet<String>,
    now: SystemTime,
    ctl: &mut Ctl<'_>,
) -> CleanupScan {
    let mut out = CleanupScan {
        scanned_at: now_unix(),
        is_admin,
        test_root: layout.test_root.as_ref().map(|p| p.to_string_lossy().into_owned()),
        ..Default::default()
    };
    let steps = CATEGORIES.len() as u32;
    for (i, def) in CATEGORIES.iter().enumerate() {
        if ctl.cancelled() {
            out.cancelled = true;
            break;
        }
        ctl.step(def.label, i as u32 + 1, steps);
        out.categories.push(scan_category(layout, def, now, running, ctl));
    }
    out.cancelled |= ctl.cancelled();
    ctl.finish();
    out
}

// ---------------------------------------------------------------------------
// Clean
// ---------------------------------------------------------------------------

pub struct CleanOptions<'a> {
    pub is_admin: bool,
    pub older_than_day: bool,
    pub running: &'a HashSet<String>,
    /// The 24-hour rule counts back from here.
    pub now: SystemTime,
}

fn count_skip(c: &mut CategoryClean, s: Skip) {
    match s {
        Skip::InUse => c.skipped_in_use += 1,
        Skip::Denied => c.skipped_denied += 1,
        Skip::Gone => {}
        Skip::Link | Skip::Outside | Skip::Other => c.skipped_other += 1,
    }
}

fn clean_dir(
    dir: &Path,
    root: &Path,
    root_final: &str,
    cutoff: Option<SystemTime>,
    c: &mut CategoryClean,
    ctl: &mut Ctl<'_>,
) {
    let Ok(rd) = fs::read_dir(dir) else {
        c.skipped_denied += 1;
        return;
    };
    for entry in rd.flatten() {
        if ctl.cancelled() {
            return;
        }
        let path = entry.path();
        let Ok(md) = entry.metadata() else {
            c.skipped_other += 1;
            continue;
        };
        if is_reparse(&md) {
            // Never entered, never deleted.
            c.skipped_other += 1;
            continue;
        }
        if md.is_dir() {
            clean_dir(&path, root, root_final, cutoff, c, ctl);
            // Folders go only once empty. Their modified time changes as
            // files leave, so the age rule uses the created time.
            let young = cutoff.is_some_and(|cut| md.created().map_or(true, |t| t >= cut));
            if !young && !ctl.cancelled() {
                let _ = delete_checked(&path, root, root_final, Rule::Inside, true);
            }
            continue;
        }
        if let Some(cut) = cutoff {
            if is_new(&md, cut) {
                c.kept_new += 1;
                continue;
            }
        }
        match delete_checked(&path, root, root_final, Rule::Inside, false) {
            Ok(size) => {
                c.freed_bytes += size;
                c.deleted_files += 1;
                ctl.add(1, size);
            }
            Err(s) => count_skip(c, s),
        }
    }
}

fn clean_matching(dir: &Path, pattern: &Pattern, c: &mut CategoryClean, ctl: &mut Ctl<'_>) {
    let Ok(root_final) = open_root(dir) else { return };
    let Ok(rd) = fs::read_dir(dir) else {
        c.skipped_denied += 1;
        return;
    };
    for entry in rd.flatten() {
        if ctl.cancelled() {
            return;
        }
        let Ok(md) = entry.metadata() else { continue };
        if md.is_dir() || !pattern.matches(&entry.file_name().to_string_lossy()) {
            continue;
        }
        if is_reparse(&md) {
            c.skipped_other += 1;
            continue;
        }
        match delete_checked(&entry.path(), dir, &root_final, Rule::DirectChild(pattern), false) {
            Ok(size) => {
                c.freed_bytes += size;
                c.deleted_files += 1;
                ctl.add(1, size);
            }
            Err(s) => count_skip(c, s),
        }
    }
}

fn empty_recycle_bin(c: &mut CategoryClean, ctl: &mut Ctl<'_>) {
    if cfg!(test) {
        panic!("tests must never empty the real Recycle Bin");
    }
    let Some((before_bytes, before_items)) = recycle_query() else {
        c.skipped = Some("Orbit could not read the Recycle Bin.".to_string());
        return;
    };
    if before_items == 0 {
        return;
    }
    // SAFETY: NULL window and root (every drive); no UI, no sound.
    let res = unsafe {
        SHEmptyRecycleBinW(HWND::default(), PCWSTR::null(), SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND)
    };
    let (after_bytes, after_items) = recycle_query().unwrap_or((before_bytes, before_items));
    c.freed_bytes = before_bytes.saturating_sub(after_bytes);
    c.deleted_files = before_items.saturating_sub(after_items);
    c.skipped_other += after_items;
    if res.is_err() && c.deleted_files == 0 {
        c.skipped = Some("Windows could not empty the Recycle Bin.".to_string());
    }
    ctl.add(c.deleted_files, c.freed_bytes);
}

fn clean_category(layout: &Layout, def: &CatDef, opts: &CleanOptions<'_>, ctl: &mut Ctl<'_>) -> CategoryClean {
    let mut c = CategoryClean { id: def.id.to_string(), label: def.label.to_string(), ..Default::default() };
    if def.needs_admin && !opts.is_admin {
        c.skipped = Some("Needs administrator rights.".to_string());
        return c;
    }
    if let Some((name, exe)) = def.browser {
        if opts.running.contains(exe) {
            c.skipped = Some(format!("{name} is open. Close it, then clean again."));
            return c;
        }
    }
    let cutoff = (def.age_filter && opts.older_than_day)
        .then(|| opts.now.checked_sub(Duration::from_secs(24 * 3600)).unwrap_or(UNIX_EPOCH));
    for target in targets(layout, def.id) {
        if ctl.cancelled() {
            break;
        }
        match target {
            Target::Contents(root) => {
                if root_problem(layout, def.id, &root).is_some() {
                    continue;
                }
                match open_root(&root) {
                    Ok(root_final) => {
                        // The root's real location gets the same checks.
                        if root_allowed(&root_final, &layout.protected) {
                            clean_dir(&root, &root, &root_final, cutoff, &mut c, ctl);
                        }
                    }
                    Err(Skip::Gone) => {}
                    Err(Skip::Denied) => c.skipped_denied += 1,
                    Err(_) => c.skipped_other += 1,
                }
            }
            Target::Matching { dir, pattern } => clean_matching(&dir, &pattern, &mut c, ctl),
            Target::RecycleBin => empty_recycle_bin(&mut c, ctl),
        }
    }
    c
}

/// Cleans the given categories. Only ever deletes inside `targets(layout)`.
pub fn clean_all(layout: &Layout, defs: &[&CatDef], opts: &CleanOptions<'_>, ctl: &mut Ctl<'_>) -> CleanResult {
    #[cfg(test)]
    assert!(layout.is_test(), "tests must only clean the test layout");
    let mut out = CleanResult::default();
    let steps = defs.len() as u32;
    for (i, def) in defs.iter().enumerate() {
        if ctl.cancelled() {
            break;
        }
        ctl.step(def.label, i as u32 + 1, steps);
        let c = clean_category(layout, def, opts, ctl);
        out.freed_bytes += c.freed_bytes;
        out.deleted_files += c.deleted_files;
        out.skipped_in_use += c.skipped_in_use;
        out.skipped_denied += c.skipped_denied;
        out.skipped_other += c.skipped_other;
        out.kept_new += c.kept_new;
        out.categories.push(c);
    }
    out.cancelled = ctl.cancelled();
    ctl.finish();
    out
}

// ---------------------------------------------------------------------------
// Biggest folders (read only)
// ---------------------------------------------------------------------------

const TOP_FOLDERS: usize = 40;
/// Folders this many levels below a scanned folder are kept for instant drill-down.
const CACHE_DEPTH: u32 = 2;
const CACHE_FOR: Duration = Duration::from_secs(10 * 60);

/// Not stored locally (cloud placeholders, offline files): size doesn't count.
const NOT_LOCAL: u32 = 0x0040_0000 | 0x0004_0000 | 0x0000_1000;

#[derive(Default)]
struct Measure {
    bytes: u64,
    files: u64,
    unreadable: u64,
}

fn measure(dir: &Path, depth: u32, ctl: &mut Ctl<'_>, cache: &mut Vec<FolderSizes>) -> Measure {
    let mut m = Measure::default();
    let Ok(rd) = fs::read_dir(dir) else {
        m.unreadable = 1;
        return m;
    };
    let keep = depth <= CACHE_DEPTH;
    let mut here = FolderSizes { path: dir.to_string_lossy().into_owned(), ..Default::default() };
    let mut kids: Vec<FolderEntry> = Vec::new();
    for entry in rd.flatten() {
        if ctl.cancelled() {
            return m;
        }
        let Ok(md) = entry.metadata() else { continue };
        let attrs = md.file_attributes();
        // A junction or directory symlink reports is_dir() false: check the
        // reparse bit before anything else.
        if attrs & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 && attrs & FILE_ATTRIBUTE_DIRECTORY.0 != 0 {
            here.links += 1;
            continue;
        }
        if attrs & FILE_ATTRIBUTE_DIRECTORY.0 != 0 {
            let path = entry.path();
            if depth == 0 {
                ctl.p.label = entry.file_name().to_string_lossy().into_owned();
            }
            let sub = measure(&path, depth + 1, ctl, cache);
            m.bytes += sub.bytes;
            m.files += sub.files;
            m.unreadable += sub.unreadable;
            if keep {
                kids.push(FolderEntry {
                    name: entry.file_name().to_string_lossy().into_owned(),
                    path: path.to_string_lossy().into_owned(),
                    bytes: sub.bytes,
                    files: sub.files,
                    unreadable: sub.unreadable,
                });
            }
        } else {
            let size = if attrs & NOT_LOCAL != 0 { 0 } else { md.len() };
            m.bytes += size;
            m.files += 1;
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
        here.bytes = m.bytes;
        here.files = m.files;
        here.unreadable = m.unreadable;
        here.scanned_at = now_unix();
        cache.push(here);
    }
    m
}

/// Sizes of `path`'s subfolders. Read only; links are not followed.
/// Returns the result for `path` plus every folder kept for drill-down.
pub fn folder_sizes(path: &Path, ctl: &mut Ctl<'_>) -> (FolderSizes, Vec<FolderSizes>) {
    let mut cache = Vec::new();
    measure(path, 0, ctl, &mut cache);
    let cancelled = ctl.cancelled();
    ctl.finish();
    let blank = FolderSizes {
        path: path.to_string_lossy().into_owned(),
        cancelled,
        unreadable: u64::from(!cancelled),
        scanned_at: now_unix(),
        ..Default::default()
    };
    if cancelled {
        return (blank, Vec::new());
    }
    // The scanned folder is the last one pushed (post-order); none when it
    // could not be listed.
    match cache.pop() {
        Some(top) if top.path == blank.path => (top, cache),
        _ => (blank, Vec::new()),
    }
}

fn folder_cache() -> &'static Mutex<HashMap<String, (Instant, FolderSizes)>> {
    static CACHE: OnceLock<Mutex<HashMap<String, (Instant, FolderSizes)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cache_key(path: &str) -> String {
    path_parts(path).map(|p| p.join("\\")).unwrap_or_else(|| path.to_lowercase())
}

/// Checks a folder the user typed or picked: absolute and an existing folder.
pub fn valid_scan_folder(path: &str) -> Result<PathBuf, String> {
    let t = path.trim().trim_matches('"');
    if t.is_empty() || path_parts(t).is_none() {
        return Err("Type a full folder path, like C:\\Users.".to_string());
    }
    let mut p = t.replace('/', "\\");
    if p.len() == 2 {
        p.push('\\'); // "C:" means the drive root, not its current folder
    }
    let pb = PathBuf::from(&p);
    match fs::metadata(&pb) {
        Ok(m) if m.is_dir() => Ok(pb),
        Ok(_) => Err("That is a file, not a folder.".to_string()),
        Err(_) => Err("That folder doesn't exist.".to_string()),
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

fn emitter(app: AppHandle) -> impl FnMut(&Progress) {
    move |p: &Progress| {
        let _ = app.emit(PROGRESS_EVENT, p);
    }
}

#[tauri::command]
pub async fn cleanup_scan(app: AppHandle) -> Result<CleanupScan, String> {
    let guard = SCAN_JOB.start().ok_or("A scan is already running.")?;
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        let layout = Layout::current()?;
        let running = running_exes();
        let mut sink = emitter(app);
        let mut ctl = Ctl::new("scan", &SCAN_JOB.cancel, &mut sink);
        Ok(scan_all(&layout, is_admin(), &running, SystemTime::now(), &mut ctl))
    })
    .await
    .map_err(|e| format!("The scan stopped: {e}."))?
}

#[tauri::command]
pub async fn cleanup_clean(app: AppHandle, ids: Vec<String>, older_than_day: Option<bool>) -> Result<CleanResult, String> {
    let defs = validate_ids(&ids)?;
    let guard = CLEAN_JOB.start().ok_or("A clean is already running.")?;
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        let layout = Layout::current()?;
        let running = running_exes();
        let opts = CleanOptions {
            is_admin: is_admin(),
            older_than_day: older_than_day.unwrap_or(true),
            running: &running,
            now: SystemTime::now(),
        };
        let mut sink = emitter(app);
        let mut ctl = Ctl::new("clean", &CLEAN_JOB.cancel, &mut sink);
        Ok(clean_all(&layout, &defs, &opts, &mut ctl))
    })
    .await
    .map_err(|e| format!("The clean stopped: {e}."))?
}

/// `job`: "scan" | "clean" | "folders". Stops at the next file; what was
/// already deleted stays deleted.
#[tauri::command]
pub fn cleanup_cancel(job: String) -> Result<(), String> {
    let j = match job.as_str() {
        "scan" => &SCAN_JOB,
        "clean" => &CLEAN_JOB,
        "folders" => &FOLDERS_JOB,
        _ => return Err("Unknown job.".to_string()),
    };
    j.cancel.store(true, Ordering::Relaxed);
    Ok(())
}

#[tauri::command]
pub fn cleanup_drives() -> Vec<DriveInfo> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let mut out: Vec<DriveInfo> = disks
        .list()
        .iter()
        .map(|d| DriveInfo {
            mount: d.mount_point().to_string_lossy().into_owned(),
            name: d.name().to_string_lossy().into_owned(),
            total: d.total_space(),
            available: d.available_space(),
            removable: d.is_removable(),
        })
        .collect();
    out.sort_by(|a, b| a.mount.cmp(&b.mount));
    out.dedup_by(|a, b| a.mount == b.mount);
    out
}

/// `fresh`: ignore what a parent folder's scan already measured.
#[tauri::command]
pub async fn cleanup_folder_sizes(app: AppHandle, path: String, fresh: Option<bool>) -> Result<FolderSizes, String> {
    let dir = valid_scan_folder(&path)?;
    let key = cache_key(&dir.to_string_lossy());
    if !fresh.unwrap_or(false) {
        let cache = folder_cache().lock().unwrap_or_else(|p| p.into_inner());
        if let Some((at, hit)) = cache.get(&key) {
            if at.elapsed() < CACHE_FOR {
                let mut hit = hit.clone();
                hit.cached = true;
                return Ok(hit);
            }
        }
    }
    let guard = FOLDERS_JOB.start().ok_or("A folder scan is already running.")?;
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        let mut sink = emitter(app);
        let mut ctl = Ctl::new("folders", &FOLDERS_JOB.cancel, &mut sink);
        ctl.step(&dir.to_string_lossy(), 1, 1);
        let (top, kept) = folder_sizes(&dir, &mut ctl);
        if !top.cancelled {
            let mut cache = folder_cache().lock().unwrap_or_else(|p| p.into_inner());
            cache.retain(|_, (at, _)| at.elapsed() < CACHE_FOR);
            let now = Instant::now();
            for f in kept {
                cache.insert(cache_key(&f.path), (now, f));
            }
            cache.insert(key, (now, top.clone()));
        }
        Ok(top)
    })
    .await
    .map_err(|e| format!("The folder scan stopped: {e}."))?
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;
    use std::process::Command;

    /// A fresh folder under the cargo target dir (never the owner's folders).
    fn base(name: &str) -> PathBuf {
        let target = std::env::var("CARGO_TARGET_DIR")
            .map(|t| PathBuf::from(t.replace('/', "\\")))
            .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
        let b = target.join("cleanup-tests").join(format!("{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&b);
        fs::create_dir_all(&b).unwrap();
        b
    }

    fn put(p: &Path, bytes: usize) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, vec![b'x'; bytes]).unwrap();
    }

    fn junction(link: &Path, target: &Path) {
        let ok = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "mklink /J failed");
    }

    fn quiet() -> impl FnMut(&Progress) {
        |_: &Progress| {}
    }

    fn opts(running: &HashSet<String>, now: SystemTime) -> CleanOptions<'_> {
        CleanOptions { is_admin: true, older_than_day: true, running, now }
    }

    /// "now" two days ahead, so every file counts as older than 24 hours.
    fn later() -> SystemTime {
        SystemTime::now() + Duration::from_secs(2 * 86400)
    }

    fn clean_ids(layout: &Layout, ids: &[&str], o: &CleanOptions<'_>) -> CleanResult {
        let ids: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
        let defs = validate_ids(&ids).unwrap();
        let cancel = AtomicBool::new(false);
        let mut sink = quiet();
        let mut ctl = Ctl::new("clean", &cancel, &mut sink);
        clean_all(layout, &defs, o, &mut ctl)
    }

    // ---- path parts and containment

    #[test]
    fn parts_normalise() {
        assert_eq!(path_parts(r"C:\Users\A").unwrap(), ["c:", "users", "a"]);
        assert_eq!(path_parts(r"\\?\C:\Users\A\").unwrap(), ["c:", "users", "a"]);
        assert_eq!(path_parts("c:/users//a/").unwrap(), ["c:", "users", "a"]);
        assert_eq!(path_parts(r"\\srv\share\x").unwrap(), [r"\\srv\share", "x"]);
        assert_eq!(path_parts(r"\\?\UNC\srv\share\x").unwrap(), [r"\\srv\share", "x"]);
        assert_eq!(path_parts(r"C:\").unwrap(), ["c:"]);
        for bad in [
            r"Users\A",
            r"\Users\A",
            r"C:Users",
            r"C:\a\..\b",
            r"C:\a\.\b",
            r"..\a",
            r"\\.\PhysicalDrive0",
            r"\\?\Volume{1234}\x",
            r"\\srv",
            "",
        ] {
            assert!(path_parts(bad).is_none(), "{bad} should be refused");
        }
    }

    #[test]
    fn within_rules() {
        let root = r"C:\Users\A\AppData\Local\Temp";
        assert!(is_within(root, r"C:\Users\A\AppData\Local\Temp\x.tmp"));
        assert!(is_within(root, r"C:\Users\A\AppData\Local\Temp\d\e\f.txt"));
        // never the root itself
        assert!(!is_within(root, root));
        assert!(!is_within(root, r"C:\Users\A\AppData\Local\Temp\"));
        // a sibling with the same prefix is outside
        assert!(!is_within(root, r"C:\Users\A\AppData\Local\Temp2\x"));
        assert!(!is_within(root, r"C:\Users\A\AppData\Local\Tem"));
        // traversal is refused outright
        assert!(!is_within(root, r"C:\Users\A\AppData\Local\Temp\..\Microsoft\x"));
        assert!(!is_within(root, r"C:\Users\A\AppData\Local\Temp\..\..\..\..\..\Windows\System32\x"));
        // parents and other drives
        assert!(!is_within(root, r"C:\Users\A"));
        assert!(!is_within(root, r"D:\Users\A\AppData\Local\Temp\x"));
        // case, verbatim and slash forms all compare the same
        assert!(is_within(root, r"c:\users\a\appdata\local\TEMP\X"));
        assert!(is_within(&format!(r"\\?\{root}"), &format!(r"{root}\x")));
        assert!(is_within(root, &format!(r"\\?\{root}\x")));
        assert!(is_within(root, "C:/Users/A/AppData/Local/Temp/x"));
        // UNC
        assert!(is_within(r"\\srv\share\t", r"\\?\UNC\srv\share\t\x"));
        assert!(!is_within(r"\\srv\share\t", r"\\srv\other\t\x"));
        // relative roots or candidates never match
        assert!(!is_within(r"Temp", r"Temp\x"));
        assert!(!is_within(root, r"x"));
    }

    #[test]
    fn within_long_paths() {
        let root = r"C:\Users\A\AppData\Local\Temp";
        let deep = format!(r"{root}\{}", vec!["abcdefghij"; 40].join("\\"));
        assert!(deep.len() > 400);
        assert!(is_within(root, &deep));
        assert!(is_within(root, &format!(r"\\?\{deep}")));
        assert!(!is_within(&deep, root));
    }

    #[test]
    fn verbatim_form() {
        assert_eq!(to_verbatim(r"C:\a\b"), r"\\?\C:\a\b");
        assert_eq!(to_verbatim("C:/a/b"), r"\\?\C:\a\b");
        assert_eq!(to_verbatim(r"\\?\C:\a"), r"\\?\C:\a");
        assert_eq!(to_verbatim(r"\\srv\share\a"), r"\\?\UNC\srv\share\a");
    }

    #[test]
    fn root_rules() {
        let protected: Vec<String> = [
            r"C:\Users\A",
            r"C:\Users\A\Documents",
            r"C:\Users\A\AppData\Local",
            r"C:\Windows",
            r"C:\Program Files",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert!(root_allowed(r"C:\Users\A\AppData\Local\Temp", &protected));
        assert!(root_allowed(r"C:\Windows\Temp", &protected));
        assert!(root_allowed(r"C:\Windows\SoftwareDistribution\Download", &protected));
        assert!(root_allowed(r"\\?\C:\WINDOWS\TEMP", &protected));
        // drive roots and shallow folders
        assert!(!root_allowed(r"C:\", &protected));
        assert!(!root_allowed(r"D:\Temp", &protected));
        // protected folders and their parents
        assert!(!root_allowed(r"C:\Users\A", &protected));
        assert!(!root_allowed(r"c:\users\a\", &protected));
        assert!(!root_allowed(r"C:\Users", &protected));
        assert!(!root_allowed(r"C:\Windows", &protected));
        assert!(!root_allowed(r"C:\Users\A\AppData\Local", &protected));
        assert!(!root_allowed(r"C:\Users\A\AppData", &protected));
        assert!(!root_allowed(r"C:\Users\A\Documents", &protected));
        // relative or traversing
        assert!(!root_allowed(r"Temp\x\y", &protected));
        assert!(!root_allowed(r"C:\Users\A\AppData\Local\Temp\..\..", &protected));
    }

    #[test]
    fn temp_shape() {
        assert!(looks_like_temp(r"C:\Users\A\AppData\Local\Temp"));
        assert!(looks_like_temp(r"C:\Users\A\AppData\Local\Temp\2"));
        assert!(looks_like_temp(r"D:\tmp\x"));
        assert!(!looks_like_temp(r"C:\Users\A"));
        assert!(!looks_like_temp(r"C:\Temperature\x"));
        assert!(!looks_like_temp(r"Temp"));
    }

    #[test]
    fn patterns() {
        let t = Pattern::Wrapped("thumbcache_", ".db");
        assert!(t.matches("thumbcache_32.db"));
        assert!(t.matches("THUMBCACHE_IDX.DB"));
        assert!(!t.matches("thumbcache_.db"));
        assert!(!t.matches("iconcache_32.db"));
        assert!(!t.matches("thumbcache_32.db.bak"));
        assert!(!t.matches(r"x\thumbcache_32.db"));
        let m = Pattern::Exact("memory.dmp");
        assert!(m.matches("MEMORY.DMP"));
        assert!(!m.matches("memory.dmp.txt"));
        assert!(!m.matches("old-memory.dmp"));
    }

    #[test]
    fn direct_child_rule() {
        let p = Pattern::Exact("memory.dmp");
        let r = Rule::DirectChild(&p);
        assert!(rule_allows(r, r"C:\Windows", r"C:\Windows\MEMORY.DMP"));
        assert!(rule_allows(r, r"\\?\C:\Windows", r"C:\WINDOWS\memory.dmp"));
        assert!(!rule_allows(r, r"C:\Windows", r"C:\Windows\System32\memory.dmp"));
        assert!(!rule_allows(r, r"C:\Windows", r"C:\Windows\notepad.exe"));
        assert!(!rule_allows(r, r"C:\Windows", r"C:\Windows"));
        assert!(!rule_allows(r, r"C:\Windows", r"C:\memory.dmp"));
    }

    #[test]
    fn ids_are_checked() {
        assert!(validate_ids(&["user-temp".into(), "user-temp".into()]).unwrap().len() == 1);
        assert!(validate_ids(&["user-temp".into(), r"C:\Windows".into()]).is_err());
        assert!(validate_ids(&["../user-temp".into()]).is_err());
        assert!(validate_ids(&[]).is_err());
    }

    // ---- layouts

    #[test]
    fn test_layout_roots_stay_inside() {
        let b = base("layout");
        let l = Layout::under(&b);
        assert!(l.is_test());
        for def in CATEGORIES {
            for t in targets(&l, def.id) {
                match t {
                    Target::Contents(r) => {
                        let r = r.to_string_lossy();
                        assert!(is_within(&b.to_string_lossy(), &r), "{} root {r} escapes", def.id);
                        assert!(root_problem(&l, def.id, Path::new(&*r)).is_none(), "{} root {r} refused", def.id);
                    }
                    Target::Matching { dir, .. } => {
                        assert!(is_within(&b.to_string_lossy(), &dir.to_string_lossy()));
                    }
                    Target::RecycleBin => panic!("the test layout must not use the real Recycle Bin"),
                }
            }
        }
        // The test base itself is protected: a root can't be it.
        assert!(!root_allowed(&b.to_string_lossy(), &l.protected));
    }

    /// Read only: the real roots are where they should be and pass the rules.
    #[test]
    fn system_layout_roots() {
        let l = Layout::system().unwrap();
        assert!(!l.is_test());
        let temp = targets(&l, "user-temp");
        let Target::Contents(t) = &temp[0] else { panic!() };
        assert!(root_problem(&l, "user-temp", t).is_none(), "{t:?}");
        let win = l.windows.to_string_lossy().to_lowercase();
        for id in ["windows-temp", "update-cache"] {
            let Target::Contents(r) = &targets(&l, id)[0] else { panic!() };
            assert!(r.to_string_lossy().to_lowercase().starts_with(&win));
            assert!(root_problem(&l, id, r).is_none(), "{r:?}");
        }
        // The protected list holds the profile and Windows.
        assert!(!root_allowed(&l.windows.to_string_lossy(), &l.protected));
        assert!(!root_allowed(&l.local.to_string_lossy(), &l.protected));
        assert_eq!(targets(&l, "recycle-bin"), vec![Target::RecycleBin]);
    }

    // ---- deleting (test layout only)

    #[test]
    fn clean_everything_in_test_layout() {
        let b = base("all");
        let l = Layout::under(&b);
        let ut = l.user_temp.clone();
        put(&ut.join("a.tmp"), 100);
        put(&ut.join(r"sub\deeper\b.tmp"), 50);
        put(&l.windows.join(r"Temp\w.tmp"), 10);
        put(&l.windows.join(r"SoftwareDistribution\Download\abc\u.cab"), 20);
        put(&l.windows.join(r"Minidump\1.dmp"), 30);
        put(&l.windows.join("MEMORY.DMP"), 40);
        put(&l.windows.join("keep.exe"), 5);
        put(&l.windows.join("memory.dmp.txt"), 5);
        put(&l.local.join(r"CrashDumps\app.dmp"), 60);
        let explorer = l.local.join(r"Microsoft\Windows\Explorer");
        put(&explorer.join("thumbcache_32.db"), 70);
        put(&explorer.join("iconcache_32.db"), 5);
        fs::create_dir_all(explorer.join("thumbcache_dir.db")).unwrap();
        put(&b.join(r"RecycleBin\$R1.txt"), 80);

        let running = HashSet::new();
        let ids: Vec<&str> = CATEGORIES.iter().map(|c| c.id).collect();
        let r = clean_ids(&l, &ids, &opts(&running, later()));
        assert_eq!(r.freed_bytes, 100 + 50 + 10 + 20 + 30 + 40 + 60 + 70 + 80);
        assert_eq!(r.deleted_files, 9);
        assert_eq!(r.skipped_in_use + r.skipped_denied + r.skipped_other, 0);
        // roots stay, contents go
        assert!(ut.is_dir());
        assert_eq!(fs::read_dir(&ut).unwrap().count(), 0, "empty folders inside the root go too");
        assert!(l.windows.join("Temp").is_dir());
        assert!(l.windows.join(r"SoftwareDistribution\Download").is_dir());
        assert!(!l.windows.join("MEMORY.DMP").exists());
        // only the named files in the Windows folder
        assert!(l.windows.join("keep.exe").exists());
        assert!(l.windows.join("memory.dmp.txt").exists());
        assert!(explorer.join("iconcache_32.db").exists());
        assert!(explorer.join("thumbcache_dir.db").is_dir());
        assert!(!explorer.join("thumbcache_32.db").exists());
        assert!(b.join("RecycleBin").is_dir());
        let _ = fs::remove_dir_all(&b);
    }

    #[test]
    fn junctions_are_never_followed() {
        let b = base("junction");
        let l = Layout::under(&b);
        let outside = b.join("Outside");
        put(&outside.join("precious.txt"), 10);
        put(&l.user_temp.join("junk.tmp"), 10);
        junction(&l.user_temp.join("link"), &outside);
        let running = HashSet::new();
        let r = clean_ids(&l, &["user-temp"], &opts(&running, later()));
        assert!(outside.join("precious.txt").exists(), "a junction must not be followed");
        assert!(l.user_temp.join("link").exists(), "the junction itself is left alone");
        assert!(!l.user_temp.join("junk.tmp").exists());
        assert_eq!(r.deleted_files, 1);
        assert_eq!(r.skipped_other, 1);

        // A root that is itself a junction is refused.
        let crash = l.local.join("CrashDumps");
        fs::create_dir_all(&l.local).unwrap();
        junction(&crash, &outside);
        let r = clean_ids(&l, &["crash-dumps"], &opts(&running, later()));
        assert_eq!(r.deleted_files, 0);
        assert!(outside.join("precious.txt").exists());
        let _ = fs::remove_dir_all(&b);
    }

    #[test]
    fn checked_delete_refuses_outside_paths() {
        let b = base("outside");
        let root = b.join("Root");
        fs::create_dir_all(&root).unwrap();
        put(&b.join(r"Other\f.txt"), 3);
        let root_final = open_root(&root).unwrap();
        let other = b.join(r"Other\f.txt");
        assert_eq!(delete_checked(&other, &root, &root_final, Rule::Inside, false), Err(Skip::Outside));
        let sneaky = root.join(r"..\Other\f.txt");
        assert_eq!(delete_checked(&sneaky, &root, &root_final, Rule::Inside, false), Err(Skip::Outside));
        // the root itself, as a folder
        assert_eq!(delete_checked(&root, &root, &root_final, Rule::Inside, true), Err(Skip::Outside));
        assert!(other.exists());
        assert!(root.is_dir());

        // Path looks inside, but its real location is outside (via a junction
        // in the middle): the handle check catches it.
        junction(&root.join("hop"), &b.join("Other"));
        let via = root.join(r"hop\f.txt");
        assert_eq!(delete_checked(&via, &root, &root_final, Rule::Inside, false), Err(Skip::Outside));
        assert!(other.exists());
        let _ = fs::remove_dir_all(&b);
    }

    #[test]
    fn age_rule_keeps_new_files() {
        let b = base("age");
        let l = Layout::under(&b);
        put(&l.user_temp.join("new.tmp"), 10);
        // Old modified time, but created just now (an unpacked file): kept.
        let f = l.user_temp.join("unpacked.tmp");
        put(&f, 10);
        let old = SystemTime::now() - Duration::from_secs(10 * 86400);
        fs::File::options().write(true).open(&f).unwrap().set_modified(old).unwrap();
        let running = HashSet::new();
        let r = clean_ids(&l, &["user-temp"], &opts(&running, SystemTime::now()));
        assert_eq!(r.deleted_files, 0);
        assert_eq!(r.kept_new, 2);
        assert!(f.exists());
        // With the rule off they go.
        let mut o = opts(&running, SystemTime::now());
        o.older_than_day = false;
        let r = clean_ids(&l, &["user-temp"], &o);
        assert_eq!(r.deleted_files, 2);
        let _ = fs::remove_dir_all(&b);
    }

    #[test]
    fn in_use_and_read_only_are_skipped() {
        let b = base("inuse");
        let l = Layout::under(&b);
        let busy = l.user_temp.join("busy.tmp");
        let ro = l.user_temp.join("ro.tmp");
        put(&busy, 10);
        put(&ro, 10);
        let mut perm = fs::metadata(&ro).unwrap().permissions();
        perm.set_readonly(true);
        fs::set_permissions(&ro, perm.clone()).unwrap();
        let hold = fs::OpenOptions::new().read(true).share_mode(0).open(&busy).unwrap();
        let running = HashSet::new();
        let r = clean_ids(&l, &["user-temp"], &opts(&running, later()));
        assert_eq!(r.skipped_in_use, 1);
        assert_eq!(r.skipped_denied, 1);
        assert_eq!(r.deleted_files, 0);
        drop(hold);
        assert!(busy.exists() && ro.exists());
        perm.set_readonly(false);
        fs::set_permissions(&ro, perm).unwrap();
        let _ = fs::remove_dir_all(&b);
    }

    #[test]
    fn long_paths_are_cleaned() {
        let b = base("long");
        let l = Layout::under(&b);
        let mut p = l.user_temp.clone();
        for _ in 0..30 {
            p.push("abcdefghijkl");
        }
        let f = p.join("deep.tmp");
        assert!(f.to_string_lossy().len() > 400);
        put(&f, 7);
        let running = HashSet::new();
        let r = clean_ids(&l, &["user-temp"], &opts(&running, later()));
        assert_eq!(r.deleted_files, 1);
        assert_eq!(r.freed_bytes, 7);
        assert!(!f.exists());
        assert_eq!(fs::read_dir(&l.user_temp).unwrap().count(), 0);
        let _ = fs::remove_dir_all(&b);
    }

    #[test]
    fn browser_caches_only_and_not_while_running() {
        let b = base("browser");
        let l = Layout::under(&b);
        let ud = l.local.join(r"Google\Chrome\User Data");
        for prof in ["Default", "Profile 1"] {
            put(&ud.join(prof).join("Preferences"), 2);
            put(&ud.join(prof).join("Cookies"), 2);
            put(&ud.join(prof).join("History"), 2);
            put(&ud.join(prof).join(r"Login Data"), 2);
            put(&ud.join(prof).join(r"Cache\Cache_Data\f_000001"), 100);
            put(&ud.join(prof).join(r"Code Cache\js\abc"), 10);
            put(&ud.join(prof).join(r"GPUCache\data_0"), 5);
        }
        // a folder without Preferences is not a profile
        put(&ud.join(r"NotAProfile\Cache\x"), 9);
        put(&ud.join(r"ShaderCache\s"), 3);
        put(&ud.join("Local State"), 2);
        let ff = l.local.join(r"Mozilla\Firefox\Profiles\abc.default");
        put(&ff.join(r"cache2\entries\E1"), 40);
        put(&ff.join("places.sqlite"), 2);

        let mut running = HashSet::new();
        running.insert("chrome.exe".to_string());
        let r = clean_ids(&l, &["browser-chrome", "browser-firefox"], &opts(&running, later()));
        assert_eq!(r.categories[0].skipped.as_deref(), Some("Chrome is open. Close it, then clean again."));
        assert!(ud.join(r"Default\Cache\Cache_Data\f_000001").exists());
        assert_eq!(r.categories[1].deleted_files, 1);
        assert!(ff.join("places.sqlite").exists());

        running.clear();
        let r = clean_ids(&l, &["browser-chrome"], &opts(&running, later()));
        assert_eq!(r.deleted_files, 2 * 3 + 1);
        for prof in ["Default", "Profile 1"] {
            for keep in ["Preferences", "Cookies", "History", "Login Data"] {
                assert!(ud.join(prof).join(keep).exists(), "{prof}\\{keep} must stay");
            }
            assert!(ud.join(prof).join("Cache").is_dir());
        }
        assert!(ud.join(r"NotAProfile\Cache\x").exists());
        assert!(ud.join("Local State").exists());
        let _ = fs::remove_dir_all(&b);
    }

    #[test]
    fn admin_categories_need_admin() {
        let b = base("admin");
        let l = Layout::under(&b);
        put(&l.windows.join(r"Temp\w.tmp"), 10);
        let running = HashSet::new();
        let mut o = opts(&running, later());
        o.is_admin = false;
        let r = clean_ids(&l, &["windows-temp"], &o);
        assert_eq!(r.categories[0].skipped.as_deref(), Some("Needs administrator rights."));
        assert!(l.windows.join(r"Temp\w.tmp").exists());
        let _ = fs::remove_dir_all(&b);
    }

    #[test]
    fn cancel_stops_before_deleting() {
        let b = base("cancel");
        let l = Layout::under(&b);
        put(&l.user_temp.join("a.tmp"), 10);
        let defs = validate_ids(&["user-temp".to_string()]).unwrap();
        let cancel = AtomicBool::new(true);
        let mut sink = quiet();
        let mut ctl = Ctl::new("clean", &cancel, &mut sink);
        let running = HashSet::new();
        let r = clean_all(&l, &defs, &opts(&running, later()), &mut ctl);
        assert!(r.cancelled);
        assert!(l.user_temp.join("a.tmp").exists());
        let _ = fs::remove_dir_all(&b);
    }

    #[test]
    fn scan_counts_without_deleting() {
        let b = base("scan");
        let l = Layout::under(&b);
        put(&l.user_temp.join("a.tmp"), 100);
        put(&l.user_temp.join(r"d\b.tmp"), 50);
        put(&l.local.join(r"CrashDumps\x.dmp"), 30);
        let outside = b.join("Outside");
        put(&outside.join("big.bin"), 1000);
        junction(&l.user_temp.join("link"), &outside);
        let mut running = HashSet::new();
        running.insert("msedge.exe".to_string());
        let cancel = AtomicBool::new(false);
        let mut events = 0;
        let mut sink = |_: &Progress| events += 1;
        let mut ctl = Ctl::new("scan", &cancel, &mut sink);
        let s = scan_all(&l, false, &running, SystemTime::now(), &mut ctl);
        drop(ctl);
        assert!(events >= CATEGORIES.len());
        let get = |id: &str| s.categories.iter().find(|c| c.id == id).unwrap();
        let t = get("user-temp");
        assert_eq!((t.bytes, t.files), (150, 2), "the junction is not counted");
        assert_eq!((t.old_bytes, t.old_files), (0, 0), "all new");
        assert!(t.found && t.readable);
        assert_eq!(get("crash-dumps").old_bytes, 30, "no age rule on dumps");
        assert!(get("browser-edge").blocked.is_some());
        assert!(!get("windows-temp").found);
        assert_eq!(s.test_root.as_deref(), Some(&*b.to_string_lossy()));
        assert!(l.user_temp.join("a.tmp").exists());
        let _ = fs::remove_dir_all(&b);
    }

    #[test]
    fn folder_sizes_rank_and_skip_links() {
        let b = base("folders");
        put(&b.join(r"Big\a\x.bin"), 500);
        put(&b.join(r"Big\b.bin"), 300);
        put(&b.join(r"Small\c.bin"), 10);
        put(&b.join("loose.bin"), 7);
        fs::create_dir_all(b.join("Empty")).unwrap();
        junction(&b.join("Loop"), &b);
        let cancel = AtomicBool::new(false);
        let mut sink = quiet();
        let mut ctl = Ctl::new("folders", &cancel, &mut sink);
        let (top, kept) = folder_sizes(&b, &mut ctl);
        assert_eq!(top.bytes, 817);
        assert_eq!(top.files, 4);
        assert_eq!(top.files_here_bytes, 7);
        assert_eq!(top.links, 1);
        let names: Vec<&str> = top.folders.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["Big", "Small", "Empty"]);
        assert_eq!(top.folders[0].bytes, 800);
        // Big and Big\a were kept for drill-down.
        let big = kept.iter().find(|f| f.path.ends_with("Big")).unwrap();
        assert_eq!(big.folders[0].name, "a");
        assert_eq!(big.files_here_bytes, 300);
        let _ = fs::remove_dir_all(&b);
    }

    #[test]
    fn scan_folder_input() {
        assert!(valid_scan_folder("").is_err());
        assert!(valid_scan_folder(r"relative\x").is_err());
        assert!(valid_scan_folder(r"C:\Windows\..\Users").is_err());
        assert!(valid_scan_folder(r"C:\definitely-not-here-orbit").is_err());
        assert_eq!(valid_scan_folder("C:").unwrap(), PathBuf::from(r"C:\"));
        assert!(valid_scan_folder(r#""C:\Windows""#).is_ok());
    }

    /// The env override end to end, as a debug build of the app would run it.
    /// `cargo test cleanup::tests::env_override -- --ignored --test-threads=1`
    #[test]
    #[ignore]
    fn env_override_end_to_end() {
        let b = base("env");
        std::env::set_var("ORBIT_CLEANUP_ROOT", &b);
        let l = Layout::current().unwrap();
        std::env::remove_var("ORBIT_CLEANUP_ROOT");
        assert!(l.is_test());
        put(&l.user_temp.join(r"x\old.tmp"), 1234);
        put(&b.join(r"RecycleBin\deleted.txt"), 66);
        let running = HashSet::new();
        let r = clean_ids(&l, &["user-temp", "recycle-bin"], &opts(&running, later()));
        println!("{r:#?}");
        assert_eq!(r.freed_bytes, 1300);
        assert!(l.user_temp.is_dir() && b.join("RecycleBin").is_dir());
        let _ = fs::remove_dir_all(&b);
    }

    /// Read only: what the real scan finds on this PC.
    /// `cargo test cleanup::tests::live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_scan_read_only() {
        let l = Layout::system().unwrap();
        let cancel = AtomicBool::new(false);
        let mut sink = quiet();
        let mut ctl = Ctl::new("scan", &cancel, &mut sink);
        let t = Instant::now();
        let s = scan_all(&l, is_admin(), &running_exes(), SystemTime::now(), &mut ctl);
        println!("scan took {:?}, admin {}", t.elapsed(), s.is_admin);
        for c in &s.categories {
            println!(
                "{:16} found={} readable={} {:>12} B {:>7} files | old {:>12} B | blocked={:?} note={:?}",
                c.id, c.found, c.readable, c.bytes, c.files, c.old_bytes, c.blocked, c.note
            );
        }
        let mut ctl = Ctl::new("folders", &cancel, &mut sink);
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let (top, _) = folder_sizes(&dir, &mut ctl);
        println!("{} = {} B in {} files", top.path, top.bytes, top.files);
        for f in top.folders.iter().take(5) {
            println!("  {:30} {:>12} B", f.name, f.bytes);
        }
    }
}
