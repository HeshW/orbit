//! Security: Authenticode signatures, a heuristic process scan, file hashes,
//! loaded modules and environment of a process.
//!
//! Signatures: `WinVerifyTrust` (WINTRUST_ACTION_GENERIC_VERIFY_V2) on the
//! file's embedded signature first; a file without one (most of System32) is
//! looked up in the Windows catalogs (`CryptCATAdmin*`, SHA-256 then SHA-1)
//! and verified against its catalog. Revocation and network fetches are off,
//! so a check never waits on the internet. The publisher is the signer
//! certificate's display name. Results are cached per path, size and
//! modification time, and a batch runs on up to eight threads.

use std::collections::HashMap;
use std::ffi::c_void;
use std::fs::File;
use std::io::{ErrorKind, Read, Seek, SeekFrom};
use std::mem::size_of;
use std::os::windows::fs::MetadataExt;
use std::os::windows::io::AsRawHandle;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use tauri::State;
use windows::core::{w, HRESULT, PCWSTR};
use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_PARTIAL_COPY, BOOL, HANDLE, HMODULE, HWND};
use windows::Win32::Security::Cryptography::Catalog::{
    CryptCATAdminAcquireContext2, CryptCATAdminCalcHashFromFileHandle2,
    CryptCATAdminEnumCatalogFromHash, CryptCATAdminReleaseCatalogContext,
    CryptCATAdminReleaseContext, CryptCATCatalogInfoFromContext, CATALOG_INFO,
};
use windows::Win32::Security::Cryptography::{CertGetNameStringW, CERT_NAME_SIMPLE_DISPLAY_TYPE};
use windows::Win32::Security::WinTrust::{
    WinVerifyTrust, CRYPT_PROVIDER_SGNR, WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_CATALOG_INFO, WINTRUST_DATA, WINTRUST_DATA_0,
    WINTRUST_DATA_UNION_CHOICE, WINTRUST_FILE_INFO, WTD_CACHE_ONLY_URL_RETRIEVAL,
    WTD_CHOICE_CATALOG, WTD_CHOICE_FILE, WTD_REVOCATION_CHECK_NONE, WTD_REVOKE_NONE,
    WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY, WTD_UI_NONE,
};
use windows::Win32::System::ProcessStatus::{
    EnumProcessModulesEx, GetMappedFileNameW, GetModuleFileNameExW, GetModuleInformation,
    LIST_MODULES_ALL, MODULEINFO,
};
use windows::Win32::System::Threading::{IsWow64Process, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ};

use crate::actions::{handle_creation_filetime, normalize_path, open_process};
use crate::model::same_start;
use crate::sampler::AppState;

const ACCESS_DENIED: &str = "Access denied. Try Restart as administrator.";
const EXITED: &str = "That process has exited.";
const UNVERIFIED: &str = "Could not confirm which process that is. Refresh and try again.";

const TRUST_E_NOSIGNATURE: i32 = 0x800B_0100_u32 as i32;
const TRUST_E_SUBJECT_FORM_UNKNOWN: i32 = 0x800B_0003_u32 as i32;
const TRUST_E_PROVIDER_UNKNOWN: i32 = 0x800B_0001_u32 as i32;
const CRYPT_E_FILE_ERROR: i32 = 0x8009_2003_u32 as i32;

/// Files above this size are checked for zero padding.
const PAD_MIN_BYTES: u64 = 200 * 1024 * 1024;
/// How much of the end of a large file is sampled for padding.
const PAD_SAMPLE_BYTES: u64 = 1024 * 1024;
const SIG_WORKERS: usize = 8;
const SIG_CACHE_MAX: usize = 8192;

/// Signature of one file. `status`: "signed" | "unsigned" | "invalid" | "unknown".
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SignatureInfo {
    pub status: String,
    /// Signer's common name, e.g. "Microsoft Windows".
    pub publisher: Option<String>,
}

impl SignatureInfo {
    fn new(status: &str, publisher: Option<String>) -> Self {
        Self { status: status.to_string(), publisher }
    }

    fn unknown() -> Self {
        Self::new("unknown", None)
    }
}

/// One process worth a look. `severity`: "high" | "medium" | "info".
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SecurityFinding {
    pub pid: u32,
    pub name: String,
    pub exe: Option<String>,
    pub severity: String,
    /// Plain sentences, e.g. "Runs from a temporary folder.".
    pub reasons: Vec<String>,
    /// As in `SignatureInfo::status`.
    pub signature_status: String,
    pub publisher: Option<String>,
}

/// One DLL or other module loaded in a process.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ModuleInfo {
    pub path: String,
    /// Bytes.
    pub size: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EnvVar {
    pub name: String,
    pub value: String,
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The path as Windows can open it: no quotes, no `\\?\` prefix, backslashes.
fn clean_path(path: &str) -> String {
    let p = path.trim().trim_matches('"').replace('/', "\\");
    match p.strip_prefix(r"\\?\") {
        Some(rest) => rest.to_string(),
        None => p,
    }
}

fn hex(bytes: &[u8], upper: bool) -> String {
    const LOWER: &[u8; 16] = b"0123456789abcdef";
    const UPPER: &[u8; 16] = b"0123456789ABCDEF";
    let digits = if upper { UPPER } else { LOWER };
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(digits[(b >> 4) as usize] as char);
        out.push(digits[(b & 15) as usize] as char);
    }
    out
}

fn filetime_to_unix(ticks: u64) -> u64 {
    (ticks / 10_000_000).saturating_sub(11_644_473_600)
}

fn join_error(e: impl std::fmt::Display) -> String {
    format!("Something went wrong: {e}.")
}

// ---------------------------------------------------------------------------
// Signatures
// ---------------------------------------------------------------------------

// The `windows` crate gates these two behind `Win32_Security_Cryptography_Sip`,
// which Orbit's Cargo.toml does not enable, so they are declared here.
// `CRYPT_PROVIDER_DATA` is only passed through, as an opaque pointer.
#[link(name = "wintrust")]
extern "system" {
    fn WTHelperProvDataFromStateData(hstatedata: HANDLE) -> *mut c_void;
    fn WTHelperGetProvSignerFromChain(
        pprovdata: *mut c_void,
        idxsigner: u32,
        fcountersigner: BOOL,
        idxcountersigner: u32,
    ) -> *mut CRYPT_PROVIDER_SGNR;
}

fn base_trust_data(choice: WINTRUST_DATA_UNION_CHOICE, union: WINTRUST_DATA_0) -> WINTRUST_DATA {
    WINTRUST_DATA {
        cbStruct: size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: choice,
        Anonymous: union,
        dwProvFlags: WTD_REVOCATION_CHECK_NONE | WTD_CACHE_ONLY_URL_RETRIEVAL,
        ..Default::default()
    }
}

/// Display name of the first signer's certificate in a verified state.
///
/// # Safety
/// `state` must be the `hWVTStateData` of a WinVerifyTrust call that has
/// not been closed yet.
unsafe fn signer_name(state: HANDLE) -> Option<String> {
    if state.is_invalid() {
        return None;
    }
    let prov = WTHelperProvDataFromStateData(state);
    if prov.is_null() {
        return None;
    }
    let sgnr = WTHelperGetProvSignerFromChain(prov, 0, BOOL(0), 0);
    if sgnr.is_null() || (*sgnr).csCertChain == 0 || (*sgnr).pasCertChain.is_null() {
        return None;
    }
    let cert = (*(*sgnr).pasCertChain).pCert;
    if cert.is_null() {
        return None;
    }
    let mut buf = [0u16; 256];
    let n = CertGetNameStringW(cert, CERT_NAME_SIMPLE_DISPLAY_TYPE, 0, None, Some(&mut buf)) as usize;
    let len = n.saturating_sub(1).min(buf.len());
    let name = String::from_utf16_lossy(&buf[..len]).trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// Verifies `data`, reads the signer, then releases the verify state.
///
/// # Safety
/// Every pointer inside `data` must stay valid for the call.
unsafe fn run_trust(data: &mut WINTRUST_DATA) -> (i32, Option<String>) {
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    // INVALID_HANDLE_VALUE: there is no interactive user to ask.
    let hwnd = HWND(-1isize as *mut c_void);
    data.dwStateAction = WTD_STATEACTION_VERIFY;
    let status = WinVerifyTrust(hwnd, &mut action, data as *mut WINTRUST_DATA as *mut c_void);
    let publisher = signer_name(data.hWVTStateData);
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    WinVerifyTrust(hwnd, &mut action, data as *mut WINTRUST_DATA as *mut c_void);
    (status, publisher)
}

/// A catalog admin context for one hash algorithm ("SHA256" or "SHA1").
struct CatAdmin(isize);

impl CatAdmin {
    fn new(algorithm: PCWSTR) -> Option<Self> {
        let mut handle = 0isize;
        unsafe { CryptCATAdminAcquireContext2(&mut handle, None, algorithm, None, 0) }.ok()?;
        (handle != 0).then_some(Self(handle))
    }

    /// Verifies `file` against the system catalog that lists its hash.
    /// `None` when no catalog lists it.
    fn verify(&self, file: &mut File, wide_path: &[u16]) -> Option<(i32, Option<String>)> {
        file.seek(SeekFrom::Start(0)).ok()?;
        let handle = HANDLE(file.as_raw_handle() as *mut c_void);
        let mut hash = [0u8; 64];
        let mut len = hash.len() as u32;
        unsafe { CryptCATAdminCalcHashFromFileHandle2(self.0, handle, &mut len, Some(hash.as_mut_ptr()), 0) }
            .ok()?;
        let len = (len as usize).min(hash.len());
        let cat = unsafe { CryptCATAdminEnumCatalogFromHash(self.0, &hash[..len], 0, None) };
        if cat == 0 {
            return None;
        }
        let mut info = CATALOG_INFO {
            cbStruct: size_of::<CATALOG_INFO>() as u32,
            wszCatalogFile: [0; 260],
        };
        let result = if unsafe { CryptCATCatalogInfoFromContext(cat, &mut info, 0) }.is_ok() {
            let tag = to_wide(&hex(&hash[..len], true));
            let mut cat_info = WINTRUST_CATALOG_INFO {
                cbStruct: size_of::<WINTRUST_CATALOG_INFO>() as u32,
                dwCatalogVersion: 0,
                pcwszCatalogFilePath: PCWSTR(info.wszCatalogFile.as_ptr()),
                pcwszMemberTag: PCWSTR(tag.as_ptr()),
                pcwszMemberFilePath: PCWSTR(wide_path.as_ptr()),
                hMemberFile: handle,
                pbCalculatedFileHash: hash.as_mut_ptr(),
                cbCalculatedFileHash: len as u32,
                pcCatalogContext: std::ptr::null_mut(),
                hCatAdmin: self.0,
            };
            let mut data = base_trust_data(WTD_CHOICE_CATALOG, WINTRUST_DATA_0 { pCatalog: &mut cat_info });
            // SAFETY: cat_info, tag, info and hash outlive the call.
            Some(unsafe { run_trust(&mut data) })
        } else {
            None
        };
        unsafe {
            let _ = CryptCATAdminReleaseCatalogContext(self.0, cat, 0);
        }
        result
    }
}

impl Drop for CatAdmin {
    fn drop(&mut self) {
        unsafe {
            let _ = CryptCATAdminReleaseContext(self.0, 0);
        }
    }
}

/// Per-thread verifier: the catalog contexts are made on first need.
#[derive(Default)]
struct Verifier {
    sha256: Option<Option<CatAdmin>>,
    sha1: Option<Option<CatAdmin>>,
}

impl Verifier {
    fn catalog(&mut self, file: &mut File, wide_path: &[u16]) -> Option<(i32, Option<String>)> {
        let sha256 = self.sha256.get_or_insert_with(|| CatAdmin::new(w!("SHA256")));
        if let Some(found) = sha256.as_ref().and_then(|a| a.verify(file, wide_path)) {
            return Some(found);
        }
        let sha1 = self.sha1.get_or_insert_with(|| CatAdmin::new(w!("SHA1")));
        sha1.as_ref().and_then(|a| a.verify(file, wide_path))
    }

    fn verify(&mut self, path: &str) -> SignatureInfo {
        let clean = clean_path(path);
        if clean.is_empty() {
            return SignatureInfo::unknown();
        }
        let Ok(mut file) = File::open(&clean) else {
            return SignatureInfo::unknown();
        };
        let wide = to_wide(&clean);
        let mut file_info = WINTRUST_FILE_INFO {
            cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32,
            pcwszFilePath: PCWSTR(wide.as_ptr()),
            hFile: HANDLE(file.as_raw_handle() as *mut c_void),
            pgKnownSubject: std::ptr::null_mut(),
        };
        let mut data = base_trust_data(WTD_CHOICE_FILE, WINTRUST_DATA_0 { pFile: &mut file_info });
        // SAFETY: file_info, wide and file outlive the call.
        let (status, publisher) = unsafe { run_trust(&mut data) };
        match status {
            0 => SignatureInfo::new("signed", publisher),
            TRUST_E_NOSIGNATURE | TRUST_E_SUBJECT_FORM_UNKNOWN | TRUST_E_PROVIDER_UNKNOWN => {
                match self.catalog(&mut file, &wide) {
                    Some((0, publisher)) => SignatureInfo::new("signed", publisher),
                    Some((_, publisher)) => SignatureInfo::new("invalid", publisher),
                    None => SignatureInfo::new("unsigned", None),
                }
            }
            CRYPT_E_FILE_ERROR => SignatureInfo::unknown(),
            _ => SignatureInfo::new("invalid", publisher),
        }
    }
}

/// (modified, size) of a file, the cache key next to its path.
fn file_stamp(path: &str) -> Option<(u128, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_nanos();
    Some((modified, meta.len()))
}

struct CacheEntry {
    stamp: (u128, u64),
    info: SignatureInfo,
}

fn sig_cache() -> &'static Mutex<HashMap<String, CacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Verifies `paths` on up to `SIG_WORKERS` threads, in order.
fn verify_parallel(paths: &[String]) -> Vec<SignatureInfo> {
    let n = paths.len();
    if n == 0 {
        return Vec::new();
    }
    let workers = std::thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(4)
        .clamp(1, SIG_WORKERS)
        .min(n);
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<SignatureInfo>>> = Mutex::new(vec![None; n]);
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                let mut verifier = Verifier::default();
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= n {
                        break;
                    }
                    let info = verifier.verify(&paths[i]);
                    lock(&results)[i] = Some(info);
                }
            });
        }
    });
    results
        .into_inner()
        .unwrap_or_else(|p| p.into_inner())
        .into_iter()
        .map(|o| o.unwrap_or_else(SignatureInfo::unknown))
        .collect()
}

/// Signatures for `paths`, keyed by the path strings as given. Cached per
/// path, size and modification time; "unknown" results are not cached.
pub(crate) fn signatures_for(paths: &[String]) -> HashMap<String, SignatureInfo> {
    let mut out: HashMap<String, SignatureInfo> = HashMap::new();
    // normalized key -> (path to open, stamp, original spellings)
    let mut todo: HashMap<String, (String, Option<(u128, u64)>, Vec<String>)> = HashMap::new();
    {
        let cache = lock(sig_cache());
        for original in paths {
            if out.contains_key(original) {
                continue;
            }
            let clean = clean_path(original);
            if clean.is_empty() {
                out.insert(original.clone(), SignatureInfo::unknown());
                continue;
            }
            let key = normalize_path(&clean);
            if let Some(entry) = todo.get_mut(&key) {
                entry.2.push(original.clone());
                continue;
            }
            let stamp = file_stamp(&clean);
            if let (Some(stamp), Some(hit)) = (stamp, cache.get(&key)) {
                if hit.stamp == stamp {
                    out.insert(original.clone(), hit.info.clone());
                    continue;
                }
            }
            todo.insert(key, (clean, stamp, vec![original.clone()]));
        }
    }
    if todo.is_empty() {
        return out;
    }
    let work: Vec<(String, (String, Option<(u128, u64)>, Vec<String>))> = todo.into_iter().collect();
    let to_open: Vec<String> = work.iter().map(|(_, (clean, _, _))| clean.clone()).collect();
    let infos = verify_parallel(&to_open);
    let mut cache = lock(sig_cache());
    if cache.len() > SIG_CACHE_MAX {
        cache.clear();
    }
    for ((key, (_, stamp, originals)), info) in work.into_iter().zip(infos) {
        if let Some(stamp) = stamp.filter(|_| info.status != "unknown") {
            cache.insert(key, CacheEntry { stamp, info: info.clone() });
        }
        for original in originals {
            out.insert(original, info.clone());
        }
    }
    out
}

/// Signature per path (path -> info). Paths that can't be read are reported
/// as "unknown".
#[tauri::command]
pub async fn get_signatures(paths: Vec<String>) -> Result<HashMap<String, SignatureInfo>, String> {
    tauri::async_runtime::spawn_blocking(move || signatures_for(&paths))
        .await
        .map_err(join_error)
}

// ---------------------------------------------------------------------------
// Scan
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Severity {
    Info,
    Medium,
    High,
}

impl Severity {
    fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Medium => "medium",
            Severity::High => "high",
        }
    }
}

/// Where an exe lives, for the location checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Place {
    Temp,
    Downloads,
    BrowserProfile,
    ProgramDataRoot,
    AppData,
}

/// Folders the checks compare against: lowercase, no trailing backslash.
struct Dirs {
    system_root: String,
    system32: String,
    syswow64: String,
    program_data: String,
    temp: Option<String>,
}

impl Dirs {
    fn from_env() -> Self {
        let env = |name: &str| std::env::var(name).ok().map(|v| normalize_path(&v)).map(|v| v.trim_end_matches('\\').to_string());
        let system_root = env("SystemRoot").or_else(|| env("windir")).unwrap_or_else(|| r"c:\windows".to_string());
        Self {
            system32: format!(r"{system_root}\system32"),
            syswow64: format!(r"{system_root}\syswow64"),
            program_data: env("ProgramData").unwrap_or_else(|| r"c:\programdata".to_string()),
            temp: env("TEMP").filter(|t| !t.is_empty()),
            system_root,
        }
    }

    fn get() -> &'static Dirs {
        static DIRS: OnceLock<Dirs> = OnceLock::new();
        DIRS.get_or_init(Dirs::from_env)
    }
}

fn under(path: &str, dir: &str) -> bool {
    path.len() > dir.len() + 1 && path.starts_with(dir) && path.as_bytes()[dir.len()] == b'\\'
}

const BROWSER_PROFILES: &[&str] = &[
    r"\google\chrome\user data\",
    r"\google\chrome beta\user data\",
    r"\google\chrome sxs\user data\",
    r"\chromium\user data\",
    r"\microsoft\edge\user data\",
    r"\microsoft\edge beta\user data\",
    r"\bravesoftware\brave-browser\user data\",
    r"\vivaldi\user data\",
    r"\yandex\yandexbrowser\user data\",
    r"\mozilla\firefox\profiles\",
    r"\opera software\",
];

/// `path`: lowercase, normalized.
fn place_of(path: &str, dirs: &Dirs) -> Option<Place> {
    if BROWSER_PROFILES.iter().any(|p| path.contains(p)) {
        return Some(Place::BrowserProfile);
    }
    let in_temp = path.contains(r"\appdata\local\temp\")
        || under(path, &format!(r"{}\temp", dirs.system_root))
        || dirs.temp.as_deref().is_some_and(|t| under(path, t));
    if in_temp {
        return Some(Place::Temp);
    }
    if path.contains(r"\downloads\") {
        return Some(Place::Downloads);
    }
    if let Some((dir, _)) = path.rsplit_once('\\') {
        if dir == dirs.program_data {
            return Some(Place::ProgramDataRoot);
        }
    }
    if path.contains(r"\appdata\") {
        return Some(Place::AppData);
    }
    None
}

/// Image names of core Windows processes (without ".exe").
const WINDOWS_NAMES: &[&str] = &[
    "svchost", "csrss", "lsass", "smss", "wininit", "winlogon", "services", "explorer", "dwm",
    "taskhostw", "spoolsv", "conhost", "rundll32", "ctfmon", "sihost", "fontdrvhost", "dllhost",
    "runtimebroker", "searchindexer", "taskmgr", "wuauclt", "audiodg", "regsvr32", "lsm",
];

/// Names that are also ordinary words: only an exact copy counts for them.
const NO_LOOKALIKE: &[&str] = &["services", "explorer"];

/// True when `a` and `b` differ by exactly one edit (insert, delete,
/// substitute, or swap of two neighbours).
fn one_edit_apart(a: &str, b: &str) -> bool {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a == b || a.len().abs_diff(b.len()) > 1 {
        return false;
    }
    if a.len() == b.len() {
        let diffs: Vec<usize> = (0..a.len()).filter(|&i| a[i] != b[i]).collect();
        return match diffs.as_slice() {
            [_] => true,
            [i, j] => *j == i + 1 && a[*i] == b[*j] && a[*j] == b[*i],
            _ => false,
        };
    }
    let (long, short) = if a.len() > b.len() { (&a, &b) } else { (&b, &a) };
    let mut i = 0;
    while i < short.len() && long[i] == short[i] {
        i += 1;
    }
    long[i + 1..] == short[i..]
}

/// "Windows Update.exe", "WindowsHelper.exe": a name that starts with the
/// word Windows (a space, punctuation or a capital letter after it).
fn starts_with_windows_word(file_name: &str) -> bool {
    let Some(head) = file_name.get(..7) else {
        return false;
    };
    if !head.eq_ignore_ascii_case("windows") {
        return false;
    }
    match file_name[7..].chars().next() {
        Some(c) => !c.is_lowercase() && !c.is_ascii_digit(),
        None => false,
    }
}

/// A reason when the exe's name imitates a Windows component.
fn imitation(file_name: &str, path: &str, dirs: &Dirs) -> Option<String> {
    let lower = file_name.to_lowercase();
    let stem = lower.strip_suffix(".exe").unwrap_or(&lower);
    let in_windows = under(path, &dirs.system_root);
    if WINDOWS_NAMES.contains(&stem) {
        return (!in_windows)
            .then(|| format!("Has the name of a Windows process ({stem}.exe) but runs outside the Windows folder."));
    }
    if let Some(real) = WINDOWS_NAMES
        .iter()
        .filter(|n| n.len() >= 5 && !NO_LOOKALIKE.contains(n))
        .find(|n| one_edit_apart(stem, n))
    {
        return Some(format!("Name looks like a Windows process ({real}.exe) but it is not."));
    }
    if !in_windows && starts_with_windows_word(file_name) {
        return Some("Uses \"Windows\" in its name but is not part of Windows.".to_string());
    }
    None
}

/// What the scan reads about the exe file itself.
#[derive(Clone, Debug, Default)]
struct FileFacts {
    missing: bool,
    hidden: bool,
    system: bool,
    size: u64,
    zero_padded: bool,
}

/// True when the last `PAD_SAMPLE_BYTES` of a file are at least 99% zeros.
fn tail_is_zeros(path: &str, size: u64) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let sample = PAD_SAMPLE_BYTES.min(size);
    if file.seek(SeekFrom::Start(size - sample)).is_err() {
        return false;
    }
    let mut buf = vec![0u8; sample as usize];
    if file.read_exact(&mut buf).is_err() {
        return false;
    }
    let zeros = buf.iter().filter(|&&b| b == 0).count() as u64;
    zeros * 100 >= sample * 99
}

fn file_facts(path: &str) -> FileFacts {
    match std::fs::metadata(path) {
        Ok(meta) => {
            let attrs = meta.file_attributes();
            let size = meta.len();
            FileFacts {
                missing: false,
                hidden: attrs & 0x2 != 0,
                system: attrs & 0x4 != 0,
                size,
                zero_padded: size > PAD_MIN_BYTES && tail_is_zeros(path, size),
            }
        }
        Err(e) => FileFacts {
            missing: e.kind() == ErrorKind::NotFound,
            ..Default::default()
        },
    }
}

fn mb(bytes: u64) -> u64 {
    bytes / (1024 * 1024)
}

/// The checks for one exe. `None`: nothing to report.
fn evaluate(exe: &str, sig: &SignatureInfo, facts: &FileFacts, dirs: &Dirs) -> Option<(Severity, Vec<String>)> {
    let path = normalize_path(exe);
    let dir = path.rsplit_once('\\').map(|(d, _)| d).unwrap_or("");
    let file_name = clean_path(exe).rsplit('\\').next().unwrap_or("").to_string();
    let signed = sig.status == "signed";
    let microsoft = signed
        && sig
            .publisher
            .as_deref()
            .is_some_and(|p| p.to_lowercase().starts_with("microsoft"));
    let in_system32 = dir == dirs.system32
        || under(dir, &dirs.system32)
        || dir == dirs.syswow64
        || under(dir, &dirs.syswow64);
    if microsoft && in_system32 {
        return None;
    }

    let imitates = if microsoft { None } else { imitation(&file_name, &path, dirs) };
    let place = place_of(&path, dirs);
    let odd_place = place.is_some_and(|p| p != Place::AppData);
    let unsigned = !facts.missing && matches!(sig.status.as_str(), "unsigned" | "invalid");
    let strong = imitates.is_some() || facts.hidden || facts.system || facts.zero_padded;

    let mut severity: Option<Severity> = None;
    let mut reasons = Vec::new();
    let mut add = |s: Severity, reason: String| {
        severity = Some(severity.map_or(s, |cur| cur.max(s)));
        reasons.push(reason);
    };

    if let Some(reason) = imitates {
        add(Severity::High, reason);
    }
    match (facts.hidden, facts.system) {
        (true, true) => add(Severity::High, "The file is hidden and marked as a system file.".into()),
        (true, false) => add(Severity::High, "The file is hidden.".into()),
        (false, true) => add(Severity::High, "The file is marked as a system file.".into()),
        _ => {}
    }
    if facts.zero_padded {
        add(
            Severity::High,
            format!("The file is {} MB and mostly zeros. Padding a file is a trick to get past virus scanners.", mb(facts.size)),
        );
    }
    if facts.missing {
        add(Severity::Medium, "The program file is no longer on disk.".into());
    }
    if unsigned {
        let level = if odd_place || strong { Severity::High } else { Severity::Medium };
        let text = if sig.status == "invalid" { "The signature is not valid." } else { "Not signed." };
        add(level, text.into());
    }
    if let Some(place) = place {
        let (level, text) = match place {
            Place::Temp => (Severity::Medium, "Runs from a temporary folder."),
            Place::Downloads => (Severity::Medium, "Runs from a Downloads folder."),
            Place::BrowserProfile => (Severity::Medium, "Runs from a browser profile folder."),
            Place::ProgramDataRoot => (Severity::Medium, "Runs straight from the ProgramData folder."),
            Place::AppData => (Severity::Info, "Runs from a user profile (AppData), not Program Files."),
        };
        let level = if odd_place && unsigned { Severity::High } else { level };
        add(level, text.into());
    }
    severity.map(|s| (s, reasons))
}

/// A running process as the scan needs it.
struct ScanRow {
    pid: u32,
    name: String,
    exe: String,
    start_time: u64,
}

fn scan_blocking(rows: Vec<ScanRow>) -> Vec<SecurityFinding> {
    // One finding per program file: the oldest process that runs it.
    let mut by_exe: HashMap<String, ScanRow> = HashMap::new();
    for row in rows {
        let key = normalize_path(&row.exe);
        match by_exe.get(&key) {
            Some(cur) if (cur.start_time, cur.pid) <= (row.start_time, row.pid) => {}
            _ => {
                by_exe.insert(key, row);
            }
        }
    }
    let paths: Vec<String> = by_exe.values().map(|r| r.exe.clone()).collect();
    let sigs = signatures_for(&paths);
    let dirs = Dirs::get();

    let mut findings: Vec<(Severity, SecurityFinding)> = Vec::new();
    for row in by_exe.into_values() {
        let sig = sigs.get(&row.exe).cloned().unwrap_or_else(SignatureInfo::unknown);
        let facts = file_facts(&clean_path(&row.exe));
        if let Some((severity, reasons)) = evaluate(&row.exe, &sig, &facts, dirs) {
            findings.push((
                severity,
                SecurityFinding {
                    pid: row.pid,
                    name: row.name,
                    exe: Some(row.exe),
                    severity: severity.as_str().to_string(),
                    reasons,
                    signature_status: sig.status,
                    publisher: sig.publisher,
                },
            ));
        }
    }
    findings.sort_by(|(sa, a), (sb, b)| {
        sb.cmp(sa)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.pid.cmp(&b.pid))
    });
    findings.into_iter().map(|(_, f)| f).collect()
}

/// Checks every running process and returns the ones worth a look, most
/// severe first. One finding per program file (the oldest process running it).
#[tauri::command]
pub async fn security_scan(state: State<'_, AppState>) -> Result<Vec<SecurityFinding>, String> {
    let rows: Vec<ScanRow> = {
        let latest = lock(&state.latest);
        let snapshot = latest
            .as_ref()
            .ok_or_else(|| "Orbit has no process list yet. Try again in a moment.".to_string())?;
        snapshot
            .processes
            .iter()
            .filter(|r| r.pid != 0 && r.pid != 4)
            .filter_map(|r| {
                let exe = r.exe.as_deref().map(str::trim).filter(|e| !e.is_empty())?;
                Some(ScanRow {
                    pid: r.pid,
                    name: r.name.clone(),
                    exe: exe.to_string(),
                    start_time: r.start_time,
                })
            })
            .collect()
    };
    tauri::async_runtime::spawn_blocking(move || scan_blocking(rows))
        .await
        .map_err(join_error)
}

// ---------------------------------------------------------------------------
// Hash
// ---------------------------------------------------------------------------

fn sha256_reader(reader: &mut impl Read) -> std::io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize(), false))
}

fn file_sha256_blocking(path: &str) -> Result<String, String> {
    let clean = clean_path(path);
    if clean.is_empty() {
        return Err("No file path is known for that process.".to_string());
    }
    let mut file = File::open(&clean).map_err(|e| match e.kind() {
        ErrorKind::NotFound => "That file no longer exists.".to_string(),
        ErrorKind::PermissionDenied => ACCESS_DENIED.to_string(),
        _ => format!("Could not read that file: {e}."),
    })?;
    sha256_reader(&mut file).map_err(|e| format!("Could not read that file: {e}."))
}

/// SHA-256 of a file as lowercase hex.
#[tauri::command]
pub async fn file_sha256(path: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || file_sha256_blocking(&path))
        .await
        .map_err(join_error)?
}

// ---------------------------------------------------------------------------
// Modules
// ---------------------------------------------------------------------------

fn modules_blocking(pid: u32, start_time: Option<u64>) -> Result<Vec<ModuleInfo>, String> {
    if pid == 0 || pid == 4 {
        return Err("Windows does not list modules for this process.".to_string());
    }
    let handle = open_process(pid, PROCESS_QUERY_INFORMATION | PROCESS_VM_READ)?;
    if let Some(expected) = start_time.filter(|&t| t != 0) {
        let actual = handle_creation_filetime(handle.0)
            .map(filetime_to_unix)
            .ok_or_else(|| UNVERIFIED.to_string())?;
        if !same_start(actual, expected) {
            return Err(EXITED.to_string());
        }
    }

    let mut mods: Vec<HMODULE> = vec![HMODULE::default(); 512];
    let mut count = 0usize;
    let mut done = false;
    for attempt in 0..6 {
        let mut needed = 0u32;
        let cb = (mods.len() * size_of::<HMODULE>()) as u32;
        match unsafe { EnumProcessModulesEx(handle.0, mods.as_mut_ptr(), cb, &mut needed, LIST_MODULES_ALL) } {
            Ok(()) => {
                count = needed as usize / size_of::<HMODULE>();
                if count > mods.len() {
                    mods.resize(count + 64, HMODULE::default());
                    continue;
                }
                done = true;
                break;
            }
            Err(e) => {
                if e.code() == HRESULT::from_win32(ERROR_ACCESS_DENIED.0) {
                    return Err(ACCESS_DENIED.to_string());
                }
                // A starting or exiting process can fail with a partial copy: retry.
                if e.code() == HRESULT::from_win32(ERROR_PARTIAL_COPY.0) && attempt < 5 {
                    std::thread::sleep(Duration::from_millis(40));
                    continue;
                }
                return Err("Could not read the modules of that process.".to_string());
            }
        }
    }
    if !done {
        return Err("Could not read the modules of that process.".to_string());
    }
    mods.truncate(count);

    // For a 32-bit process GetModuleFileNameExW names SysWOW64 DLLs under
    // System32; the mapped file name gives the file that is really loaded.
    let wow64 = {
        let mut b = BOOL::default();
        unsafe { IsWow64Process(handle.0, &mut b) }.is_ok() && b.as_bool()
    };
    let devices = if wow64 { crate::winenum::dos_device_map() } else { Vec::new() };

    let mut out: Vec<ModuleInfo> = Vec::with_capacity(mods.len());
    let mut seen = std::collections::HashSet::new();
    let mut buf = vec![0u16; 1024];
    for m in mods {
        let mapped = if wow64 { mapped_module_path(handle.0, m, &devices, &mut buf) } else { None };
        let path = match mapped {
            Some(p) => p,
            None => {
                let n = unsafe { GetModuleFileNameExW(handle.0, m, &mut buf) } as usize;
                if n == 0 {
                    continue;
                }
                clean_path(&String::from_utf16_lossy(&buf[..n.min(buf.len())]))
            }
        };
        if !seen.insert(path.to_lowercase()) {
            continue;
        }
        let size = std::fs::metadata(&path).map(|m| m.len()).ok().or_else(|| {
            let mut info = MODULEINFO::default();
            unsafe { GetModuleInformation(handle.0, m, &mut info, size_of::<MODULEINFO>() as u32) }
                .ok()
                .map(|_| u64::from(info.SizeOfImage))
        });
        out.push(ModuleInfo { path, size: size.unwrap_or(0) });
    }
    Ok(out)
}

/// The drive path of the file mapped at a module's base address, or `None`.
fn mapped_module_path(
    process: HANDLE,
    module: HMODULE,
    devices: &[(String, String)],
    buf: &mut [u16],
) -> Option<String> {
    let mut info = MODULEINFO::default();
    unsafe { GetModuleInformation(process, module, &mut info, size_of::<MODULEINFO>() as u32) }.ok()?;
    let n = unsafe { GetMappedFileNameW(process, info.lpBaseOfDll, buf) } as usize;
    if n == 0 {
        return None;
    }
    let nt = String::from_utf16_lossy(&buf[..n.min(buf.len())]);
    crate::winenum::nt_to_dos(&nt, devices).map(|p| clean_path(&p))
}

/// Modules loaded in a process. `start_time` guards against pid reuse as in
/// the process actions (omitted or 0 skips the check).
#[tauri::command]
pub async fn get_process_modules(
    pid: u32,
    start_time: Option<u64>,
) -> Result<Vec<ModuleInfo>, String> {
    tauri::async_runtime::spawn_blocking(move || modules_blocking(pid, start_time))
        .await
        .map_err(join_error)?
}

// ---------------------------------------------------------------------------
// Environment
// ---------------------------------------------------------------------------

/// "NAME=value" entries to variables, sorted by name. Windows keeps hidden
/// per-drive folders as "=C:=C:\path"; the leading '=' belongs to the name.
fn parse_environ<'a>(entries: impl IntoIterator<Item = &'a str>) -> Vec<EnvVar> {
    let mut vars: Vec<EnvVar> = entries
        .into_iter()
        .filter_map(|entry| {
            let split = entry.char_indices().skip(1).find(|&(_, c)| c == '=').map(|(i, _)| i);
            match split {
                Some(i) => Some(EnvVar { name: entry[..i].to_string(), value: entry[i + 1..].to_string() }),
                None if !entry.is_empty() => Some(EnvVar { name: entry.to_string(), value: String::new() }),
                None => None,
            }
        })
        .collect();
    vars.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.name.cmp(&b.name)));
    vars
}

fn environment_blocking(pid: u32) -> Result<Vec<EnvVar>, String> {
    if pid == 0 || pid == 4 {
        return Err("This process has no environment.".to_string());
    }
    // Reading the environment needs these rights: say so instead of
    // returning an empty list.
    drop(open_process(pid, PROCESS_QUERY_INFORMATION | PROCESS_VM_READ)?);
    let spid = Pid::from_u32(pid);
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[spid]),
        true,
        ProcessRefreshKind::nothing().with_environ(UpdateKind::Always),
    );
    let process = sys.process(spid).ok_or_else(|| EXITED.to_string())?;
    let entries: Vec<String> = process
        .environ()
        .iter()
        .map(|e| e.to_string_lossy().into_owned())
        .collect();
    let vars = parse_environ(entries.iter().map(String::as_str));
    if vars.is_empty() {
        return Err("Orbit could not read this process's environment. Try Restart as administrator.".to_string());
    }
    Ok(vars)
}

/// Environment variables of a process.
#[tauri::command]
pub async fn get_process_environment(pid: u32) -> Result<Vec<EnvVar>, String> {
    tauri::async_runtime::spawn_blocking(move || environment_blocking(pid))
        .await
        .map_err(join_error)?
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs() -> Dirs {
        Dirs {
            system_root: r"c:\windows".into(),
            system32: r"c:\windows\system32".into(),
            syswow64: r"c:\windows\syswow64".into(),
            program_data: r"c:\programdata".into(),
            temp: Some(r"c:\users\hesh\appdata\local\temp".into()),
        }
    }

    fn sig(status: &str, publisher: Option<&str>) -> SignatureInfo {
        SignatureInfo::new(status, publisher.map(str::to_string))
    }

    fn plain() -> FileFacts {
        FileFacts { size: 1_000_000, ..Default::default() }
    }

    fn eval(exe: &str, s: &SignatureInfo, f: &FileFacts) -> Option<(Severity, Vec<String>)> {
        evaluate(exe, s, f, &dirs())
    }

    #[test]
    fn microsoft_system32_is_never_flagged() {
        let ms = sig("signed", Some("Microsoft Windows"));
        assert!(eval(r"C:\Windows\System32\svchost.exe", &ms, &plain()).is_none());
        let hidden = FileFacts { hidden: true, system: true, ..plain() };
        assert!(eval(r"C:\Windows\System32\drivers\x.exe", &ms, &hidden).is_none());
        assert!(eval(r"C:\Windows\SysWOW64\rundll32.exe", &ms, &plain()).is_none());
    }

    #[test]
    fn signed_program_files_is_clean() {
        let g = sig("signed", Some("Google LLC"));
        assert!(eval(r"C:\Program Files\Google\Chrome\Application\chrome.exe", &g, &plain()).is_none());
        assert!(eval(r"C:\Program Files\x\tool.exe", &sig("unknown", None), &plain()).is_none());
    }

    #[test]
    fn unsigned_alone_is_medium() {
        let (s, r) = eval(r"E:\Tools\thing.exe", &sig("unsigned", None), &plain()).unwrap();
        assert_eq!(s, Severity::Medium);
        assert_eq!(r, vec!["Not signed.".to_string()]);
        let (s, r) = eval(r"E:\Tools\thing.exe", &sig("invalid", Some("X")), &plain()).unwrap();
        assert_eq!(s, Severity::Medium);
        assert_eq!(r, vec!["The signature is not valid.".to_string()]);
    }

    #[test]
    fn appdata_is_info_unless_unsigned() {
        let d = sig("signed", Some("Discord Inc."));
        let (s, _) = eval(r"C:\Users\Hesh\AppData\Local\Discord\app-1.0\Discord.exe", &d, &plain()).unwrap();
        assert_eq!(s, Severity::Info);
        let (s, r) = eval(r"C:\Users\Hesh\AppData\Local\Orbit\orbit.exe", &sig("unsigned", None), &plain()).unwrap();
        assert_eq!(s, Severity::Medium);
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn odd_folders_are_medium_or_high() {
        let signed = sig("signed", Some("Some Vendor"));
        let unsigned = sig("unsigned", None);
        for exe in [
            r"C:\Users\Hesh\AppData\Local\Temp\setup.exe",
            r"C:\Users\Hesh\Downloads\setup.exe",
            r"C:\Users\Hesh\AppData\Local\Google\Chrome\User Data\Default\x.exe",
            r"C:\ProgramData\x.exe",
            r"C:\Windows\Temp\x.exe",
        ] {
            assert_eq!(eval(exe, &signed, &plain()).unwrap().0, Severity::Medium, "{exe}");
            assert_eq!(eval(exe, &unsigned, &plain()).unwrap().0, Severity::High, "{exe}");
        }
        // A program in its own ProgramData subfolder is not "straight from ProgramData".
        assert!(eval(r"C:\ProgramData\Vendor\x.exe", &signed, &plain()).is_none());
    }

    #[test]
    fn hidden_padded_and_missing() {
        let signed = sig("signed", Some("Vendor"));
        let hidden = FileFacts { hidden: true, ..plain() };
        assert_eq!(eval(r"C:\Program Files\V\a.exe", &signed, &hidden).unwrap().0, Severity::High);
        let padded = FileFacts { size: 300 * 1024 * 1024, zero_padded: true, ..plain() };
        let (s, r) = eval(r"C:\Program Files\V\a.exe", &signed, &padded).unwrap();
        assert_eq!(s, Severity::High);
        assert!(r[0].contains("300 MB"));
        let missing = FileFacts { missing: true, ..Default::default() };
        let (s, r) = eval(r"C:\Program Files\V\a.exe", &sig("unknown", None), &missing).unwrap();
        assert_eq!(s, Severity::Medium);
        assert_eq!(r, vec!["The program file is no longer on disk.".to_string()]);
        // Unsigned plus a strong sign is high.
        assert_eq!(eval(r"E:\a.exe", &sig("unsigned", None), &hidden).unwrap().0, Severity::High);
    }

    #[test]
    fn names_that_imitate_windows() {
        let unsigned = sig("unsigned", None);
        let signed = sig("signed", Some("Vendor"));
        let (s, r) = eval(r"E:\x\svchost.exe", &signed, &plain()).unwrap();
        assert_eq!(s, Severity::High);
        assert!(r[0].contains("svchost.exe"));
        assert_eq!(eval(r"C:\Users\Hesh\AppData\Local\Temp\svch0st.exe", &unsigned, &plain()).unwrap().0, Severity::High);
        assert!(eval(r"E:\x\scvhost.exe", &signed, &plain()).unwrap().1[0].contains("svchost.exe"));
        assert!(eval(r"E:\x\Windows Update.exe", &signed, &plain()).is_some());
        assert!(eval(r"E:\x\WindowsHelper.exe", &signed, &plain()).is_some());
        // Not imitations.
        assert!(eval(r"E:\x\WindowSpy.exe", &signed, &plain()).is_none());
        assert!(eval(r"E:\x\service.exe", &signed, &plain()).is_none());
        assert!(eval(r"C:\Windows\explorer.exe", &signed, &plain()).is_none());
        let ms = sig("signed", Some("Microsoft Corporation"));
        assert!(eval(r"C:\Program Files\WindowsApps\Microsoft.WindowsTerminal_1\WindowsTerminal.exe", &ms, &plain()).is_none());
    }

    #[test]
    fn edit_distance() {
        assert!(one_edit_apart("svch0st", "svchost"));
        assert!(one_edit_apart("scvhost", "svchost"));
        assert!(one_edit_apart("svchosts", "svchost"));
        assert!(one_edit_apart("svchst", "svchost"));
        assert!(!one_edit_apart("svchost", "svchost"));
        assert!(!one_edit_apart("chrome", "svchost"));
        assert!(!one_edit_apart("lsasss1", "lsass"));
    }

    #[test]
    fn parses_environment() {
        let vars = parse_environ(["Path=C:\\Windows;C:\\x", "=C:=C:\\Users", "TEMP=C:\\T", "EMPTY=", "A=b=c"]);
        let names: Vec<&str> = vars.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["=C:", "A", "EMPTY", "Path", "TEMP"]);
        assert_eq!(vars[0].value, "C:\\Users");
        assert_eq!(vars[1].value, "b=c");
        assert_eq!(vars[2].value, "");
    }

    #[test]
    fn hashes_and_hex() {
        assert_eq!(
            sha256_reader(&mut &b""[..]).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(hex(&[0x0a, 0xff], true), "0AFF");
        assert_eq!(clean_path("\"\\\\?\\C:/x/y.exe\""), r"C:\x\y.exe");
    }

    #[test]
    fn verifies_real_files() {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        // Catalog-signed (no embedded signature on Windows 10).
        let notepad = format!(r"{root}\System32\notepad.exe");
        let kernel = format!(r"{root}\System32\kernel32.dll");
        let manifest = concat!(env!("CARGO_MANIFEST_DIR"), r"\Cargo.toml").to_string();
        let missing = format!(r"{root}\System32\orbit-no-such-file.exe");
        let map = signatures_for(&[notepad.clone(), kernel.clone(), manifest.clone(), missing.clone()]);
        for p in [&notepad, &kernel] {
            let s = &map[p];
            assert_eq!(s.status, "signed", "{p}");
            assert!(s.publisher.as_deref().unwrap_or("").starts_with("Microsoft"), "{p}: {s:?}");
        }
        assert_eq!(map[&manifest].status, "unsigned");
        assert_eq!(map[&missing].status, "unknown");
        // Second call comes from the cache with the same answer.
        let again = signatures_for(&[notepad.clone()]);
        assert_eq!(again[&notepad], map[&notepad]);
    }

    #[test]
    fn reads_own_modules_and_environment() {
        let pid = std::process::id();
        let mods = modules_blocking(pid, None).unwrap();
        assert!(mods.iter().any(|m| m.path.to_lowercase().ends_with("ntdll.dll")));
        assert!(mods.iter().all(|m| m.size > 0));
        let env = environment_blocking(pid).unwrap();
        assert!(env.iter().any(|v| v.name.eq_ignore_ascii_case("path")));
        assert_eq!(modules_blocking(pid, Some(1)).unwrap_err(), EXITED);
    }
}
