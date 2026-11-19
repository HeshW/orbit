//! Developer leftovers (14b) and the 7-day hold (14c), Optimize > Cleanup.
//!
//! The user adds project folders with a folder picker (opened here, saved
//! in `<Orbit data>\dev-leftovers.json`). A scan lists `node_modules` next
//! to a `package.json` and `target` next to a `Cargo.toml`, outermost only,
//! that nothing has touched for 30 days. The UI gets ids for the last scan
//! and sends only those back.
//!
//! Rules (all unit-tested below):
//! - Picked folders: not a drive root, not equal to or a parent of a
//!   protected folder (the cleanup list), not inside Program Files, Windows,
//!   AppData or ProgramData, not a link. Checked again on the real path.
//! - The walk never enters reparse points, any `node_modules`, a matched
//!   `target`, or a no-go folder; so only the outermost match is listed.
//! - "Untouched": the newest created/modified time of the build folder's
//!   direct children, the project folder, its manifest and lock files is
//!   older than 30 days. Last-access times are not used; an unknown time
//!   counts as touched.
//! - Holding: never a delete. The build folder is opened by handle without
//!   following reparse points, its real path must be inside the picked
//!   folder's real path, and it is renamed through that handle into the hold
//!   folder on the same volume: `<Orbit data>\hold` on the system drive,
//!   `X:\.orbit-hold` (hidden) elsewhere. If that can't be used, it stays.
//! - Restore / Delete now: the held folder must be a direct child of a hold
//!   folder, checked by handle; the list only says which one. Restore puts it
//!   back only inside a picked folder and never over something that exists.
//!   Delete now and the 7-day expiry reuse the cleanup's checked delete with
//!   the hold folder as the root.

use super::*;

use serde::Deserialize;
use std::sync::atomic::AtomicU64;
use windows::core::{w, HRESULT};
use windows::Win32::Foundation::ERROR_CANCELLED;
use windows::Win32::Storage::FileSystem::{
    FileRenameInfo, SetFileAttributesW, READ_CONTROL, FILE_ATTRIBUTE_HIDDEN, FILE_RENAME_INFO,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
    COINIT_DISABLE_OLE1DDE,
};
use windows::Win32::UI::Shell::{
    FileOpenDialog, IFileOpenDialog, FOS_FORCEFILESYSTEM, FOS_NOCHANGEDIR, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS,
    SIGDN_FILESYSPATH,
};

pub const UNTOUCHED_DAYS: u64 = 30;
pub const HOLD_DAYS: u64 = 7;
const DAY: u64 = 86_400;
const MAX_DEPTH: usize = 12;
const MAX_FOLDERS: usize = 20;
const HOLD_DIR_NAME: &str = ".orbit-hold";
const STATE_FILE: &str = "dev-leftovers.json";
const ERROR_ALREADY_EXISTS: u32 = 183;
const ERROR_NOT_SAME_DEVICE: u32 = 17;

// ---------------------------------------------------------------------------
// Shapes (docs/CONTRACT.md "v1.2")
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Node,
    Rust,
}

impl Kind {
    fn build(self) -> &'static str {
        match self {
            Kind::Node => "node_modules",
            Kind::Rust => "target",
        }
    }
    fn manifest(self) -> &'static str {
        match self {
            Kind::Node => "package.json",
            Kind::Rust => "Cargo.toml",
        }
    }
    fn locks(self) -> &'static [&'static str] {
        match self {
            Kind::Node => &["package-lock.json", "yarn.lock", "pnpm-lock.yaml", "bun.lockb", "bun.lock"],
            Kind::Rust => &["Cargo.lock"],
        }
    }
    fn of_build(name: &str) -> Option<Kind> {
        match name.to_lowercase().as_str() {
            "node_modules" => Some(Kind::Node),
            "target" => Some(Kind::Rust),
            _ => None,
        }
    }
}

/// What is saved: picked folders and held items (display data only).
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Stored {
    pub folders: Vec<String>,
    pub held: Vec<Held>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Held {
    pub id: String,
    /// Where it was (shown, and where Restore puts it back).
    pub original: String,
    /// Its name inside the hold folder.
    pub hold_name: String,
    /// Unix seconds.
    pub held_at: u64,
    pub bytes: u64,
    /// Saved before the move, cleared after it (see `settle`).
    pub pending: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct FolderView {
    pub path: String,
    /// Why it is not scanned, if it isn't.
    pub problem: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct HeldView {
    pub id: String,
    pub original: String,
    pub held_at: u64,
    pub expires_at: u64,
    pub bytes: u64,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DevState {
    pub folders: Vec<FolderView>,
    pub held: Vec<HeldView>,
    pub hold_days: u64,
    pub untouched_days: u64,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DevProject {
    pub id: String,
    /// The project folder's name.
    pub name: String,
    pub project: String,
    pub path: String,
    pub kind: Kind,
    pub bytes: u64,
    pub files: u64,
    /// Newest created/modified time the age rule saw (Unix seconds).
    pub last_change: u64,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct DevScan {
    pub scanned_at: u64,
    pub projects: Vec<DevProject>,
    /// Build folders left out because the project changed in the last 30 days.
    pub recent: u64,
    pub folders: Vec<FolderView>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Kept {
    pub path: String,
    pub reason: String,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct DevResult {
    /// Folders held, restored or deleted.
    pub done: u64,
    pub bytes: u64,
    /// Left where they are, and why.
    pub kept: Vec<Kept>,
}

/// One build folder the last scan found; ids map back to these.
#[derive(Clone, Debug)]
pub struct Found {
    pub id: String,
    pub root: PathBuf,
    pub project: PathBuf,
    pub build: PathBuf,
    pub kind: Kind,
    pub bytes: u64,
}

// ---------------------------------------------------------------------------
// Settings file
// ---------------------------------------------------------------------------

fn state_path(layout: &Layout) -> PathBuf {
    layout.data.join(STATE_FILE)
}

pub fn load(layout: &Layout) -> Stored {
    fs::read(state_path(layout))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save(layout: &Layout, s: &Stored) -> Result<(), String> {
    let fail = |_| "Orbit could not save the Developer leftovers settings.".to_string();
    fs::create_dir_all(&layout.data).map_err(fail)?;
    let bytes = serde_json::to_vec_pretty(s).map_err(|_| "Orbit could not save the Developer leftovers settings.".to_string())?;
    let path = state_path(layout);
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes).map_err(fail)?;
    fs::rename(&tmp, path).map_err(fail)
}

fn same_path(a: &str, b: &str) -> bool {
    matches!((path_parts(a), path_parts(b)), (Some(x), Some(y)) if x == y)
}

// ---------------------------------------------------------------------------
// Path rules
// ---------------------------------------------------------------------------

/// Inside (or equal to) Program Files, Windows, AppData or ProgramData.
fn in_nogo(layout: &Layout, path: &str) -> bool {
    let Some(p) = path_parts(path) else { return true };
    layout.nogo.iter().filter_map(|n| path_parts(n)).any(|n| is_same_or_parent(&n, &p))
}

/// The same checks as cleanup roots, on a path string: a full path, not a
/// drive root, not a protected folder or a parent of one, not in a no-go folder.
fn path_problem(layout: &Layout, path: &str) -> Option<&'static str> {
    let Some(parts) = path_parts(path) else { return Some("That is not a full folder path.") };
    if parts.len() < 2 {
        return Some("Pick a folder, not a whole drive.");
    }
    if layout.protected.iter().filter_map(|p| path_parts(p)).any(|p| is_same_or_parent(&parts, &p)) {
        return Some("That folder holds Windows or your personal folders. Pick a projects folder inside it.");
    }
    if in_nogo(layout, path) {
        return Some("Orbit never looks inside Program Files, Windows or AppData.");
    }
    None
}

/// Why a picked folder is not used, or None. Also checks its real location.
pub fn folder_problem(layout: &Layout, path: &str) -> Option<&'static str> {
    if let Some(p) = path_problem(layout, path) {
        return Some(p);
    }
    let pb = Path::new(path);
    if !is_real_dir(pb) {
        return Some("That folder is missing or is a link. Remove it or pick it again.");
    }
    match open_root(pb) {
        Ok(real) => path_problem(layout, &real),
        Err(_) => Some("Orbit could not open that folder."),
    }
}

/// The hold folder for a drive ("e:"): Orbit's data folder on the system
/// drive, a hidden `.orbit-hold` at the root of other drives. None for
/// network shares.
pub fn hold_dir_for(layout: &Layout, drive: &str) -> Option<PathBuf> {
    let drive = drive.to_lowercase();
    let b = drive.as_bytes();
    if b.len() != 2 || !b[0].is_ascii_alphabetic() || b[1] != b':' {
        return None;
    }
    let system = path_parts(&layout.local.to_string_lossy()).map(|p| p[0].clone());
    if system.as_deref() == Some(drive.as_str()) {
        Some(layout.data.join("hold"))
    } else {
        Some(PathBuf::from(format!("{}\\{HOLD_DIR_NAME}", drive.to_uppercase())))
    }
}

fn drive_of(path: &str) -> Option<String> {
    path_parts(path).map(|p| p[0].clone())
}

/// A name with no separators or dots-only parts.
fn plain_name(n: &str) -> bool {
    !n.is_empty() && n != "." && n != ".." && !n.contains(['\\', '/', ':'])
}

// ---------------------------------------------------------------------------
// Scan (read only)
// ---------------------------------------------------------------------------

fn newest(md: &fs::Metadata) -> Option<SystemTime> {
    match (md.created().ok(), md.modified().ok()) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) | (None, Some(a)) => Some(a),
        (None, None) => None,
    }
}

/// The newest created/modified time of the build folder's direct children,
/// the project folder, its manifest and lock files. None when any of them
/// can't be read (counts as touched). Access times are not looked at.
pub fn last_change(project: &Path, build: &Path, kind: Kind) -> Option<SystemTime> {
    let mut seen = vec![fs::symlink_metadata(project).ok()?, fs::symlink_metadata(project.join(kind.manifest())).ok()?];
    for lock in kind.locks() {
        if let Ok(md) = fs::symlink_metadata(project.join(lock)) {
            seen.push(md);
        }
    }
    for e in fs::read_dir(build).ok()? {
        seen.push(e.ok()?.metadata().ok()?);
    }
    let mut latest = UNIX_EPOCH;
    for md in &seen {
        latest = latest.max(newest(md)?);
    }
    Some(latest)
}

fn dir_size(dir: &Path) -> (u64, u64) {
    let (mut bytes, mut files) = (0, 0);
    let Ok(rd) = fs::read_dir(dir) else { return (0, 0) };
    for e in rd.flatten() {
        let Ok(md) = e.metadata() else { continue };
        if is_reparse(&md) {
            continue;
        }
        if md.is_dir() {
            let (b, f) = dir_size(&e.path());
            bytes += b;
            files += f;
        } else {
            bytes += md.len();
            files += 1;
        }
    }
    (bytes, files)
}

fn is_plain_file(p: &Path) -> bool {
    fs::symlink_metadata(p).map(|m| m.is_file() && !is_reparse(&m)).unwrap_or(false)
}

/// Build folders under `dir`: (project, build, kind). Outermost only.
fn walk(layout: &Layout, dir: &Path, depth: usize, out: &mut Vec<(PathBuf, PathBuf, Kind)>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut subs: Vec<(String, PathBuf)> = rd
        .flatten()
        .filter(|e| e.metadata().is_ok_and(|m| m.is_dir() && !is_reparse(&m)))
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .collect();
    subs.sort();
    for (name, sub) in subs {
        if name.eq_ignore_ascii_case(HOLD_DIR_NAME) || in_nogo(layout, &sub.to_string_lossy()) {
            continue;
        }
        if let Some(kind) = Kind::of_build(&name) {
            if is_plain_file(&dir.join(kind.manifest())) {
                out.push((dir.to_path_buf(), sub, kind));
                continue;
            }
            if kind == Kind::Node {
                continue; // never look inside any node_modules
            }
        }
        if depth < MAX_DEPTH {
            walk(layout, &sub, depth + 1, out);
        }
    }
}

static SCAN_GEN: AtomicU64 = AtomicU64::new(1);

/// Lists untouched build folders under every picked folder. Read only.
pub fn scan(layout: &Layout, stored: &Stored, now: SystemTime) -> (DevScan, Vec<Found>) {
    let cutoff = now.checked_sub(Duration::from_secs(UNTOUCHED_DAYS * DAY)).unwrap_or(UNIX_EPOCH);
    let gen = SCAN_GEN.fetch_add(1, Ordering::Relaxed);
    let mut out = DevScan { scanned_at: now_unix(), ..Default::default() };
    let mut found: Vec<Found> = Vec::new();
    for f in &stored.folders {
        let problem = folder_problem(layout, f);
        out.folders.push(FolderView { path: f.clone(), problem: problem.map(str::to_string) });
        if problem.is_some() {
            continue;
        }
        let mut hits = Vec::new();
        walk(layout, Path::new(f), 0, &mut hits);
        for (project, build, kind) in hits {
            if found.iter().any(|x| same_path(&x.build.to_string_lossy(), &build.to_string_lossy())) {
                continue; // two picked folders overlap
            }
            match last_change(&project, &build, kind) {
                Some(t) if t < cutoff => {
                    let (bytes, files) = dir_size(&build);
                    let id = format!("{gen}-{}", found.len());
                    out.projects.push(DevProject {
                        id: id.clone(),
                        name: project.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                        project: project.to_string_lossy().into_owned(),
                        path: build.to_string_lossy().into_owned(),
                        kind,
                        bytes,
                        files,
                        last_change: t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
                    });
                    found.push(Found { id, root: PathBuf::from(f), project, build, kind, bytes });
                }
                _ => out.recent += 1,
            }
        }
    }
    out.projects.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
    (out, found)
}

/// Maps ids from the last scan back to what it found. Unknown ids are an error.
pub fn resolve<'a>(found: &'a [Found], ids: &[String]) -> Result<Vec<&'a Found>, String> {
    let mut out: Vec<&Found> = Vec::new();
    for id in ids {
        let f = found
            .iter()
            .find(|f| &f.id == id)
            .ok_or_else(|| "That list is out of date. Scan again, then pick the projects.".to_string())?;
        if !out.iter().any(|o| o.id == f.id) {
            out.push(f);
        }
    }
    if out.is_empty() {
        return Err("Pick at least one project.".to_string());
    }
    Ok(out)
}

/// Checks a found build folder again right before it is moved.
pub fn recheck(layout: &Layout, stored: &Stored, f: &Found, now: SystemTime) -> Result<(), &'static str> {
    let root = f.root.to_string_lossy();
    if !stored.folders.iter().any(|s| same_path(s, &root)) || folder_problem(layout, &root).is_some() {
        return Err("Its projects folder is no longer in the list.");
    }
    let build = f.build.to_string_lossy();
    if f.build.parent() != Some(f.project.as_path())
        || Kind::of_build(&f.build.file_name().unwrap_or_default().to_string_lossy()) != Some(f.kind)
        || !(same_path(&root, &f.project.to_string_lossy()) || is_within(&root, &f.project.to_string_lossy()))
        || in_nogo(layout, &build)
    {
        return Err("It is not where the scan found it.");
    }
    if !is_real_dir(&f.build) {
        return Err("It is gone or is now a link.");
    }
    if !is_plain_file(&f.project.join(f.kind.manifest())) {
        return Err("Its project file is gone.");
    }
    let cutoff = now.checked_sub(Duration::from_secs(UNTOUCHED_DAYS * DAY)).unwrap_or(UNIX_EPOCH);
    match last_change(&f.project, &f.build, f.kind) {
        Some(t) if t < cutoff => Ok(()),
        _ => Err("The project changed in the last 30 days."),
    }
}

// ---------------------------------------------------------------------------
// Moving by handle
// ---------------------------------------------------------------------------

/// Renames the file or folder behind `h` to `target` (a full path on the
/// same volume). Never replaces. Err is the Win32 code.
fn rename_by_handle(h: &Owned, target: &str) -> Result<(), u32> {
    let name: Vec<u16> = target.encode_utf16().collect();
    let size = std::mem::size_of::<FILE_RENAME_INFO>() + name.len() * 2;
    let mut buf = vec![0u64; size.div_ceil(8)];
    let info = buf.as_mut_ptr() as *mut FILE_RENAME_INFO;
    // SAFETY: the buffer is zeroed, 8-byte aligned and big enough for the
    // header plus the name and its NUL; `h` was opened with DELETE.
    unsafe {
        (*info).Anonymous.ReplaceIfExists = BOOLEAN(0);
        (*info).FileNameLength = (name.len() * 2) as u32;
        std::ptr::copy_nonoverlapping(name.as_ptr(), std::ptr::addr_of_mut!((*info).FileName) as *mut u16, name.len());
        SetFileInformationByHandle(h.0, FileRenameInfo, info as *const core::ffi::c_void, size as u32)
    }
    .map_err(|e| win32_code(&e))
}

/// Opens a folder for moving: no link following, a real folder, and its real
/// location. The caller checks where that is.
fn open_dir_for_move(path: &Path) -> Result<(Owned, BY_HANDLE_FILE_INFORMATION, String), &'static str> {
    let h = open_no_follow(path, DELETE.0 | FILE_READ_ATTRIBUTES.0).map_err(|s| match s {
        Skip::Gone => "It is gone.",
        Skip::InUse => "Something in it is open in another program.",
        Skip::Denied => "Windows did not let Orbit move it.",
        _ => "Orbit could not open it.",
    })?;
    let info = handle_info(&h).map_err(|_| "Orbit could not open it.")?;
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Err("It is a link, so Orbit leaves it alone.");
    }
    if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 == 0 {
        return Err("It is not a folder.");
    }
    let real = final_path(&h).map_err(|_| "Orbit could not open it.")?;
    Ok((h, info, real))
}

fn rename_problem(code: u32) -> &'static str {
    match code {
        5 | 32 | 33 => "Something in it is open in another program.",
        ERROR_NOT_SAME_DEVICE => "The hold folder is on another drive.",
        ERROR_ALREADY_EXISTS => "Something already has that name.",
        _ => "Windows could not move it.",
    }
}

// ---------------------------------------------------------------------------
// Hold folders: owner-only, checked by handle
// ---------------------------------------------------------------------------

/// SYSTEM, Administrators and the folder's owner only; nothing inherited from the drive.
pub(crate) const HOLD_SDDL: &str = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;OW)";
const SID_SYSTEM: &str = "S-1-5-18";
const SID_ADMINS: &str = "S-1-5-32-544";
const SID_OWNER_RIGHTS: &str = "S-1-3-4";
const SID_CREATOR_OWNER: &str = "S-1-3-0";
/// Any of these in an allow entry counts as write access (incl. delete, ACL changes, generic).
const WRITE_BITS: u32 =
    0x2 | 0x4 | 0x10 | 0x40 | 0x100 | 0x1_0000 | 0x4_0000 | 0x8_0000 | 0x0200_0000 | 0x1000_0000 | 0x4000_0000;
const UNTRUSTED_HOLD: &str = "The hold folder on this drive can be changed by other users, so Orbit doesn't use it.";

/// A hold folder's owner and the SIDs its DACL lets write (None: no DACL,
/// which lets everyone write). Inherit-only entries count too: they apply
/// to the held folders inside.
#[derive(Clone, Debug)]
pub struct HoldAcl {
    pub owner: String,
    pub writers: Option<Vec<String>>,
}

/// Trusted when the current user or Administrators own it and only SYSTEM,
/// Administrators, the owner and the current user may write.
pub fn acl_trusted(acl: &HoldAcl, me: &str) -> bool {
    let owner_ok = acl.owner == me || acl.owner == SID_ADMINS;
    let Some(w) = &acl.writers else { return false };
    owner_ok
        && w.iter().all(|s| {
            s == SID_SYSTEM || s == SID_ADMINS || s == SID_OWNER_RIGHTS || s == SID_CREATOR_OWNER || s == me || *s == acl.owner
        })
}

fn sid_string(sid: windows::Win32::Security::PSID) -> Option<String> {
    use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
    let mut p = windows::core::PWSTR::null();
    // SAFETY: valid SID; the string is freed with LocalFree.
    unsafe {
        ConvertSidToStringSidW(sid, &mut p).ok()?;
        let s = p.to_string().ok();
        let _ = windows::Win32::Foundation::LocalFree(windows::Win32::Foundation::HLOCAL(p.0 as *mut core::ffi::c_void));
        s
    }
}

/// The current user's SID ("S-1-5-21-...").
fn current_user_sid() -> Option<String> {
    use windows::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    // SAFETY: the token handle is closed by Owned; the buffer is sized by the first call.
    unsafe {
        let mut tok = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok).ok()?;
        let tok = Owned(tok);
        let mut len = 0u32;
        let _ = GetTokenInformation(tok.0, TokenUser, None, 0, &mut len);
        let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
        GetTokenInformation(tok.0, TokenUser, Some(buf.as_mut_ptr() as *mut core::ffi::c_void), len, &mut len).ok()?;
        sid_string((*(buf.as_ptr() as *const TOKEN_USER)).User.Sid)
    }
}

/// Reads owner and DACL through a handle opened with READ_CONTROL.
fn read_acl(h: &Owned) -> Option<HoldAcl> {
    use windows::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
    use windows::Win32::Security::{
        GetAce, ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR, PSID,
    };
    // SAFETY: out pointers are valid; the descriptor is freed with LocalFree.
    unsafe {
        let mut owner = PSID::default();
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut sd = PSECURITY_DESCRIPTOR::default();
        let r = GetSecurityInfo(
            h.0,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            Some(&mut dacl),
            None,
            Some(&mut sd),
        );
        if r.0 != 0 {
            return None;
        }
        let out = (|| {
            let owner = sid_string(owner)?;
            if dacl.is_null() {
                return Some(HoldAcl { owner, writers: None });
            }
            let mut writers = Vec::new();
            for i in 0..u32::from((*dacl).AceCount) {
                let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
                GetAce(dacl, i, &mut ace).ok()?;
                match (*(ace as *const ACE_HEADER)).AceType {
                    0 => {
                        let a = ace as *const ACCESS_ALLOWED_ACE;
                        if (*a).Mask & WRITE_BITS != 0 {
                            writers.push(sid_string(PSID(std::ptr::addr_of!((*a).SidStart) as *mut core::ffi::c_void))?);
                        }
                    }
                    1 => {} // deny entries only take access away
                    _ => writers.push("?".to_string()), // object / callback allow entries: not trusted
                }
            }
            Some(HoldAcl { owner, writers: Some(writers) })
        })();
        let _ = windows::Win32::Foundation::LocalFree(windows::Win32::Foundation::HLOCAL(sd.0));
        out
    }
}

/// Makes a folder with the given SDDL security descriptor (its parent must exist).
pub(crate) fn create_with_sddl(path: &Path, sddl: &str) -> Result<(), ()> {
    use windows::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
    use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use windows::Win32::Storage::FileSystem::CreateDirectoryW;
    let s = wide(sddl);
    let p = wide(&to_verbatim(&path.to_string_lossy()));
    // SAFETY: NUL-terminated strings; the descriptor is freed with LocalFree.
    unsafe {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(PCWSTR(s.as_ptr()), 1, &mut sd, None).map_err(|_| ())?;
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0,
            bInheritHandle: false.into(),
        };
        let r = CreateDirectoryW(PCWSTR(p.as_ptr()), Some(&sa));
        let _ = windows::Win32::Foundation::LocalFree(windows::Win32::Foundation::HLOCAL(sd.0));
        r.map_err(|_| ())
    }
}

/// Opens an existing hold folder and checks it by handle: a real folder,
/// not a link, owned by the current user or Administrators and writable by
/// no one else. Returns its real path and volume serial.
fn trusted_hold(hold: &Path) -> Result<(String, u32), &'static str> {
    const CANT: &str = "Orbit could not use the hold folder on this drive.";
    let h = open_no_follow(hold, FILE_READ_ATTRIBUTES.0 | READ_CONTROL.0).map_err(|_| CANT)?;
    let info = handle_info(&h).map_err(|_| CANT)?;
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 || info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 == 0 {
        return Err(CANT);
    }
    let me = current_user_sid().ok_or(CANT)?;
    if !read_acl(&h).is_some_and(|a| acl_trusted(&a, &me)) {
        return Err(UNTRUSTED_HOLD);
    }
    Ok((final_path(&h).map_err(|_| CANT)?, info.dwVolumeSerialNumber))
}

/// The hold folder for `real` (a final path), made owner-only if missing,
/// then checked. Returns its real path and volume.
fn ready_hold(layout: &Layout, real: &str) -> Result<(String, u32), &'static str> {
    let drive = drive_of(real).ok_or("Orbit can't hold folders on this drive.")?;
    let hold = hold_dir_for(layout, &drive).ok_or("Orbit can't hold folders on network drives.")?;
    if fs::symlink_metadata(&hold).is_err() {
        let made = hold.parent().is_some_and(|p| fs::create_dir_all(p).is_ok()) && create_with_sddl(&hold, HOLD_SDDL).is_ok();
        if !made {
            return Err("Orbit could not make a hold folder on this drive.");
        }
        if hold.file_name().is_some_and(|n| n == HOLD_DIR_NAME) {
            let w = wide(&hold.to_string_lossy());
            // SAFETY: NUL-terminated path.
            let _ = unsafe { SetFileAttributesW(PCWSTR(w.as_ptr()), FILE_ATTRIBUTE_HIDDEN) };
        }
    }
    trusted_hold(&hold)
}

static HOLD_SEQ: AtomicU64 = AtomicU64::new(0);

/// Every check before a move, with the handle open: the real path is inside
/// the picked folder, its real parent is the scanned project, the age rule
/// still holds, and the hold folder is trusted and on the same volume.
/// Returns the handle, the target path and the (pending) record.
fn prepare_hold(layout: &Layout, stored: &Stored, f: &Found, now: SystemTime) -> Result<(Owned, String, Held), &'static str> {
    recheck(layout, stored, f, now)?;
    let root_final = open_root(&f.root).map_err(|_| "Orbit could not open its projects folder.")?;
    let project_final = open_root(&f.project).map_err(|_| "Orbit could not open the project folder.")?;
    let (h, info, real) = open_dir_for_move(&f.build)?;
    let parts = path_parts(&real).ok_or("Orbit could not open it.")?;
    if !is_within(&root_final, &real) || Kind::of_build(parts.last().map(String::as_str).unwrap_or("")) != Some(f.kind) {
        return Err("It is not inside the projects folder any more.");
    }
    if path_parts(&project_final).as_deref() != Some(&parts[..parts.len() - 1]) {
        return Err("It is not in the project folder the scan found.");
    }
    let cutoff = now.checked_sub(Duration::from_secs(UNTOUCHED_DAYS * DAY)).unwrap_or(UNIX_EPOCH);
    if !last_change(&f.project, &f.build, f.kind).is_some_and(|t| t < cutoff) {
        return Err("The project changed in the last 30 days.");
    }
    let (hold_final, volume) = ready_hold(layout, &real)?;
    if volume != info.dwVolumeSerialNumber {
        return Err("The hold folder is on another drive.");
    }
    let secs = now.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let hold_name = format!("{secs}-{}-{}", HOLD_SEQ.fetch_add(1, Ordering::Relaxed), f.kind.build());
    let target = format!("{}\\{hold_name}", hold_final.trim_end_matches('\\'));
    let rec = Held {
        id: hold_name.clone(),
        original: f.build.to_string_lossy().into_owned(),
        hold_name,
        held_at: secs,
        bytes: f.bytes,
        pending: true,
    };
    Ok((h, target, rec))
}

const SAVE_FAILED: &str = "Orbit could not save the held list, so it left this where it is.";

/// Moves the found folders into the hold; what can't move stays and is
/// reported. Each record is saved (pending) before its move and confirmed
/// after, so a crash never leaves an unlisted folder behind.
pub fn hold_all(layout: &Layout, found: &[&Found], now: SystemTime) -> DevResult {
    #[cfg(test)]
    assert!(layout.is_test(), "tests must only hold inside the test layout");
    let mut stored = load(layout);
    settle(layout, &mut stored);
    let mut out = DevResult::default();
    for f in found {
        let path = f.build.to_string_lossy().into_owned();
        let (h, target, rec) = match prepare_hold(layout, &stored, f, now) {
            Ok(x) => x,
            Err(why) => {
                out.kept.push(Kept { path, reason: why.to_string() });
                continue;
            }
        };
        stored.held.push(rec);
        if save(layout, &stored).is_err() {
            stored.held.pop();
            out.kept.push(Kept { path, reason: SAVE_FAILED.to_string() });
            continue;
        }
        match rename_by_handle(&h, &target) {
            Ok(()) => {
                let last = stored.held.last_mut().expect("pushed above");
                last.pending = false;
                out.done += 1;
                out.bytes += last.bytes;
            }
            Err(c) => {
                stored.held.pop();
                out.kept.push(Kept { path, reason: rename_problem(c).to_string() });
            }
        }
    }
    if out.done > 0 && save(layout, &stored).is_err() {
        // The records on disk are still pending; `settle` confirms them next time.
        out.kept.push(Kept {
            path: layout.data.to_string_lossy().into_owned(),
            reason: "Orbit could not save the held list. It checks the hold folder again next time.".to_string(),
        });
    }
    out
}

/// Pending records (a move that may not have finished): kept when the folder
/// is in the hold, dropped when it isn't (the move never happened).
fn settle(layout: &Layout, stored: &mut Stored) {
    stored.held.retain_mut(|h| {
        if !h.pending {
            return true;
        }
        let there = plain_name(&h.hold_name) && hold_of(layout, h).is_some_and(|d| fs::symlink_metadata(d.join(&h.hold_name)).is_ok());
        h.pending = false;
        there
    });
}

fn hold_of(layout: &Layout, h: &Held) -> Option<PathBuf> {
    hold_dir_for(layout, &drive_of(&h.original)?)
}

const NOT_HELD: &str = "It is not in the hold folder any more.";

/// `hold\name`, only if the hold folder is trusted and the item really is a
/// direct child of it (checked by handle). Returns (path, the hold's real
/// path, handle).
fn open_in_hold(hold: &Path, name: &str) -> Result<(PathBuf, String, Owned), &'static str> {
    if !plain_name(name) {
        return Err(NOT_HELD);
    }
    let (hold_final, _) = trusted_hold(hold).map_err(|e| if e == UNTRUSTED_HOLD { e } else { NOT_HELD })?;
    let path = hold.join(name);
    let (handle, _, real) = open_dir_for_move(&path).map_err(|e| if e == "It is gone." { NOT_HELD } else { e })?;
    match (path_parts(&hold_final), path_parts(&real)) {
        (Some(r), Some(c)) if c.len() == r.len() + 1 && c[..r.len()] == r[..] => Ok((path, hold_final, handle)),
        _ => Err(NOT_HELD),
    }
}

/// Puts a held folder back where it was, only inside a picked folder and
/// never over something that exists.
pub fn restore_one(layout: &Layout, stored: &Stored, h: &Held) -> Result<(), String> {
    let original = h.original.as_str();
    let parts = path_parts(original).ok_or("Orbit doesn't know where this came from.")?;
    if Kind::of_build(parts.last().map(String::as_str).unwrap_or("")).is_none() {
        return Err("Orbit doesn't know where this came from.".to_string());
    }
    let root = stored
        .folders
        .iter()
        .find(|r| is_within(r, original) && folder_problem(layout, r).is_none())
        .ok_or("Orbit only puts folders back inside a projects folder you added. Add it again, then restore.")?;
    if in_nogo(layout, original) {
        return Err("Orbit never puts folders inside Program Files, Windows or AppData.".to_string());
    }
    let target = Path::new(original);
    if fs::symlink_metadata(target).is_ok() {
        return Err(format!("Something is already at {original}. Orbit left both where they are."));
    }
    let parent = target.parent().ok_or("Orbit doesn't know where this came from.")?;
    let root_final = open_root(Path::new(root)).map_err(|_| "Orbit could not open the projects folder.")?;
    let parent_final = open_root(parent).map_err(|_| "The project folder is gone, so there is nowhere to put it back.")?;
    match (path_parts(&root_final), path_parts(&parent_final)) {
        (Some(r), Some(p)) if is_same_or_parent(&r, &p) => {}
        _ => return Err("The project folder is not inside the projects folder any more.".to_string()),
    }
    let hold = hold_of(layout, h).ok_or(NOT_HELD)?;
    let (_, _, handle) = open_in_hold(&hold, &h.hold_name)?;
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    match rename_by_handle(&handle, &format!("{}\\{name}", parent_final.trim_end_matches('\\'))) {
        Ok(()) => Ok(()),
        Err(ERROR_ALREADY_EXISTS) | Err(80) => Err(format!("Something is already at {original}. Orbit left both where they are.")),
        Err(c) => Err(rename_problem(c).to_string()),
    }
}

/// Deletes `hold\name` for good, through the cleanup's checked delete with
/// the hold folder as the root. Ok(bytes freed) once it is gone.
fn delete_in_hold(layout: &Layout, hold: &Path, name: &str) -> Result<u64, &'static str> {
    #[cfg(test)]
    assert!(layout.is_test(), "tests must only delete inside the test layout");
    let _ = layout;
    let (path, hold_final, handle) = open_in_hold(hold, name)?;
    drop(handle);
    let cancel = AtomicBool::new(false);
    let mut sink = |_: &Progress| {};
    let mut ctl = Ctl::new("hold", &cancel, &mut sink);
    let mut c = CategoryClean::default();
    clean_dir(&path, hold, &hold_final, None, &mut c, &mut ctl);
    let res = delete_checked(&path, hold, &hold_final, Rule::Inside, true);
    if fs::symlink_metadata(&path).is_ok() {
        return Err(match res {
            Err(Skip::Link) | Err(Skip::Outside) => "Orbit found a link in it and left it alone.",
            Err(Skip::Denied) => "Windows did not let Orbit delete some of it. Orbit tries again later.",
            _ => "Some files in it are open in another program. Orbit tries again later.",
        });
    }
    Ok(c.freed_bytes)
}

pub fn delete_held_one(layout: &Layout, h: &Held) -> Result<u64, &'static str> {
    delete_in_hold(layout, &hold_of(layout, h).ok_or(NOT_HELD)?, &h.hold_name)
}

/// Restore or delete held items by id; the list is updated and always saved.
pub fn act_on_held(layout: &Layout, ids: &[String], restore: bool) -> Result<DevResult, String> {
    let mut stored = load(layout);
    settle(layout, &mut stored);
    let mut out = DevResult::default();
    for id in ids {
        let Some(i) = stored.held.iter().position(|h| &h.id == id) else {
            out.kept.push(Kept {
                path: String::new(),
                reason: "That item is no longer held. Open Cleanup again to refresh the list.".to_string(),
            });
            continue;
        };
        let h = stored.held[i].clone();
        let res = if restore {
            restore_one(layout, &stored, &h).map(|_| h.bytes)
        } else {
            delete_held_one(layout, &h).map_err(str::to_string)
        };
        match res {
            Ok(b) => {
                out.done += 1;
                out.bytes += b;
                stored.held.remove(i);
            }
            Err(why) => {
                // A record whose folder is gone from the hold is dropped from the list.
                if why == NOT_HELD {
                    stored.held.remove(i);
                }
                out.kept.push(Kept { path: h.original.clone(), reason: why });
            }
        }
    }
    save(layout, &stored)?;
    Ok(out)
}

/// The hold time in a hold name ("<unix secs>-<n>-<build>").
fn name_time(name: &str) -> Option<u64> {
    name.split('-').next()?.parse().ok()
}

/// Deletes held items older than 7 days, and unlisted folders in a trusted
/// hold folder whose name says they are older than that (left by a crash).
/// Runs at start and once a day.
pub fn expire(layout: &Layout, now: SystemTime) -> DevResult {
    let secs = now.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let due_at = |t: u64| t.saturating_add(HOLD_DAYS * DAY) <= secs;
    let mut stored = load(layout);
    if stored.held.iter().any(|h| h.pending) {
        settle(layout, &mut stored);
        let _ = save(layout, &stored);
    }
    let due: Vec<String> = stored.held.iter().filter(|h| due_at(h.held_at)).map(|h| h.id.clone()).collect();
    let mut out = if due.is_empty() { DevResult::default() } else { act_on_held(layout, &due, false).unwrap_or_default() };
    // Unlisted leftovers: the system drive's hold and those of the picked folders' drives.
    let listed: HashSet<String> = load(layout).held.iter().map(|h| h.hold_name.to_lowercase()).collect();
    let mut holds: Vec<PathBuf> = Vec::new();
    let drives = std::iter::once(drive_of(&layout.local.to_string_lossy())).chain(stored.folders.iter().map(|f| drive_of(f)));
    for d in drives.flatten() {
        if let Some(h) = hold_dir_for(layout, &d) {
            if !holds.contains(&h) {
                holds.push(h);
            }
        }
    }
    for hold in holds {
        if trusted_hold(&hold).is_err() {
            continue;
        }
        let Ok(rd) = fs::read_dir(&hold) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if listed.contains(&name.to_lowercase()) || !name_time(&name).is_some_and(due_at) {
                continue;
            }
            match delete_in_hold(layout, &hold, &name) {
                Ok(b) => {
                    out.done += 1;
                    out.bytes += b;
                }
                Err(why) => out.kept.push(Kept { path: e.path().to_string_lossy().into_owned(), reason: why.to_string() }),
            }
        }
    }
    out
}

pub fn state(layout: &Layout) -> DevState {
    let s = load(layout);
    DevState {
        folders: s
            .folders
            .iter()
            .map(|f| FolderView { path: f.clone(), problem: folder_problem(layout, f).map(str::to_string) })
            .collect(),
        held: s
            .held
            .iter()
            .filter(|h| !h.pending)
            .map(|h| HeldView {
                id: h.id.clone(),
                original: h.original.clone(),
                held_at: h.held_at,
                expires_at: h.held_at + HOLD_DAYS * DAY,
                bytes: h.bytes,
            })
            .collect(),
        hold_days: HOLD_DAYS,
        untouched_days: UNTOUCHED_DAYS,
    }
}

/// Adds a picked folder after the checks.
pub fn add_folder(layout: &Layout, path: &str) -> Result<(), String> {
    if let Some(p) = folder_problem(layout, path) {
        return Err(p.to_string());
    }
    let mut s = load(layout);
    if s.folders.iter().any(|f| same_path(f, path)) {
        return Ok(());
    }
    if s.folders.len() >= MAX_FOLDERS {
        return Err(format!("Orbit keeps up to {MAX_FOLDERS} folders. Remove one first."));
    }
    s.folders.push(path.to_string());
    save(layout, &s)
}

pub fn remove_folder(layout: &Layout, path: &str) -> Result<(), String> {
    let mut s = load(layout);
    s.folders.retain(|f| !same_path(f, path));
    save(layout, &s)
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Serialises every read-modify-write of the settings file.
fn busy() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

fn last_scan() -> &'static Mutex<Vec<Found>> {
    static LAST: OnceLock<Mutex<Vec<Found>>> = OnceLock::new();
    LAST.get_or_init(|| Mutex::new(Vec::new()))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| format!("Orbit stopped: {e}."))?
}

fn pick_folder(owner: isize) -> Result<Option<String>, String> {
    struct Com;
    impl Drop for Com {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }
    let fail = |e: windows::core::Error| format!("Could not open the folder picker: {}", e.message());
    // SAFETY: COM calls on this thread; the result string is freed below.
    unsafe {
        let _com = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).is_ok().then_some(Com);
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).map_err(fail)?;
        dialog.SetTitle(w!("Pick a projects folder")).map_err(fail)?;
        let options = dialog.GetOptions().map_err(fail)?;
        dialog
            .SetOptions(options | FOS_PICKFOLDERS | FOS_PATHMUSTEXIST | FOS_FORCEFILESYSTEM | FOS_NOCHANGEDIR)
            .map_err(fail)?;
        if let Err(e) = dialog.Show(HWND(owner as *mut core::ffi::c_void)) {
            if e.code() == HRESULT::from_win32(ERROR_CANCELLED.0) {
                return Ok(None);
            }
            return Err(fail(e));
        }
        let item = dialog.GetResult().map_err(fail)?;
        let path = item.GetDisplayName(SIGDN_FILESYSPATH).map_err(fail)?;
        let text = path.to_string().unwrap_or_default();
        CoTaskMemFree(Some(path.0 as *const core::ffi::c_void));
        Ok(Some(text).filter(|t| !t.is_empty()))
    }
}

#[tauri::command]
pub async fn devleft_state() -> Result<DevState, String> {
    blocking(|| Ok(state(&Layout::current()?))).await
}

/// Opens the folder picker; a cancelled pick changes nothing.
#[tauri::command]
pub async fn devleft_add_folder(app: AppHandle) -> Result<DevState, String> {
    use tauri::Manager;
    let owner = app.get_webview_window("main").and_then(|w| w.hwnd().ok()).map(|h| h.0 as isize).unwrap_or(0);
    blocking(move || {
        let picked = std::thread::spawn(move || pick_folder(owner))
            .join()
            .unwrap_or_else(|_| Err("The folder picker failed.".to_string()))?;
        let layout = Layout::current()?;
        if let Some(p) = picked {
            let _g = busy();
            add_folder(&layout, &p)?;
        }
        Ok(state(&layout))
    })
    .await
}

#[tauri::command]
pub async fn devleft_remove_folder(path: String) -> Result<DevState, String> {
    blocking(move || {
        let layout = Layout::current()?;
        let _g = busy();
        remove_folder(&layout, &path)?;
        Ok(state(&layout))
    })
    .await
}

#[tauri::command]
pub async fn devleft_scan() -> Result<DevScan, String> {
    blocking(|| {
        let layout = Layout::current()?;
        let stored = load(&layout);
        let (scan, found) = scan(&layout, &stored, SystemTime::now());
        *last_scan().lock().unwrap_or_else(|p| p.into_inner()) = found;
        Ok(scan)
    })
    .await
}

/// `ids` from the last `devleft_scan`. Moves them into the hold.
#[tauri::command]
pub async fn devleft_hold(ids: Vec<String>) -> Result<DevResult, String> {
    blocking(move || {
        let layout = Layout::current()?;
        let found = last_scan().lock().unwrap_or_else(|p| p.into_inner()).clone();
        let picks = resolve(&found, &ids)?;
        let _g = busy();
        Ok(hold_all(&layout, &picks, SystemTime::now()))
    })
    .await
}

#[tauri::command]
pub async fn devleft_restore(ids: Vec<String>) -> Result<DevResult, String> {
    blocking(move || {
        let layout = Layout::current()?;
        let _g = busy();
        act_on_held(&layout, &ids, true)
    })
    .await
}

#[tauri::command]
pub async fn devleft_delete_held(ids: Vec<String>) -> Result<DevResult, String> {
    blocking(move || {
        let layout = Layout::current()?;
        let _g = busy();
        act_on_held(&layout, &ids, false)
    })
    .await
}

/// Setup hook: deletes held items past 7 days now and once a day after.
pub fn init() {
    std::thread::spawn(|| loop {
        if let Ok(layout) = Layout::current() {
            let _g = busy();
            let _ = expire(&layout, SystemTime::now());
        }
        std::thread::sleep(Duration::from_secs(DAY));
    });
}

#[cfg(test)]
mod tests;
