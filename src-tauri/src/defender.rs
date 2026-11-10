//! Malware scan through Microsoft Defender (Security view, 0.6, roadmap 8e).
//!
//! Orbit ships no antivirus engine of its own. It shows Defender's status and
//! asks Defender to scan:
//! - Status: WMI `root\Microsoft\Windows\Defender` `MSFT_MpComputerStatus`
//!   (real-time protection, definitions, last quick / full scan, versions),
//!   read only. Every antivirus Windows knows comes from `root\SecurityCenter2`
//!   `AntiVirusProduct` (`productState` decoded below). When another one is on
//!   and Defender is not, the UI names that product and hides the scan buttons.
//! - Scans: `MpCmdRun.exe -Scan -ScanType 1|2|3 [-File <path>]` from the
//!   Defender platform folder, started hidden (no console), on a worker
//!   thread, one at a time. Progress is "running" plus elapsed seconds on
//!   `orbit://defender-progress`; cancel kills the child. The result comes
//!   from the exit code (0 clean, 2 threats) and the threat names it prints.
//! - Orbit never removes, quarantines or restores anything: Defender acts on
//!   what it finds by its own settings, and the UI offers "Open Windows
//!   Security" to see and handle threats there.
//!
//! Paths for a custom scan are passed as one argument (never a shell string),
//! must exist and must be plain local paths (`X:\...` on a local drive).

use std::io::Read;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use windows::core::{w, HRESULT, HSTRING, PCWSTR, VARIANT};
use windows::Win32::Foundation::{ERROR_CANCELLED, HWND};
use windows::Win32::Storage::FileSystem::GetDriveTypeW;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
};
use windows::Win32::System::Variant::{VT_BOOL, VT_BSTR, VT_DATE};
use windows::Win32::UI::Shell::{
    FileOpenDialog, IFileOpenDialog, ShellExecuteW, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM,
    FOS_NOCHANGEDIR, FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::health::{self, lock, now_secs, prop, variant_string, variant_u64, Wmi};

/// Sent with a `DefenderScan` about once a second while a scan runs, and once
/// more with the result.
pub const PROGRESS_EVENT: &str = "orbit://defender-progress";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const MAX_PATH_CHARS: usize = 4096;
const MAX_THREATS: usize = 50;

// ---------------------------------------------------------------------------
// Shapes (docs/CONTRACT.md "v0.6")
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DefenderStatus {
    /// Unix seconds.
    pub read_at: u64,
    pub is_admin: bool,
    /// Defender's own status was read (MSFT_MpComputerStatus).
    pub available: bool,
    /// Defender is the antivirus protecting this PC (service and antivirus on,
    /// not in passive mode).
    pub active: bool,
    /// "Normal", "Passive Mode", "EDR Block Mode", ... as Defender says it.
    pub running_mode: Option<String>,
    pub real_time: Option<bool>,
    pub signature_version: Option<String>,
    /// Unix seconds.
    pub signature_updated: Option<u64>,
    pub signature_age_days: Option<u32>,
    pub signatures_out_of_date: Option<bool>,
    /// Unix seconds; `None` when that scan never ran.
    pub quick_scan_at: Option<u64>,
    pub full_scan_at: Option<u64>,
    pub engine_version: Option<String>,
    /// The Defender app (platform) version.
    pub product_version: Option<String>,
    pub tamper_protected: Option<bool>,
    /// Every antivirus registered with Windows Security (SecurityCenter2).
    pub products: Vec<AvProduct>,
    /// Another antivirus that is on while Defender is not active.
    pub other: Option<AvProduct>,
    /// Defender is active and MpCmdRun.exe was found.
    pub can_scan: bool,
    /// The running scan, else the last one this run.
    pub scan: Option<DefenderScan>,
    /// Why the status could not be read, when it could not.
    pub error: Option<String>,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AvProduct {
    pub name: String,
    pub defender: bool,
    /// "on" | "off" | "snoozed" | "expired".
    pub state: String,
    pub up_to_date: bool,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DefenderScan {
    pub id: u64,
    /// "quick" | "full" | "custom".
    pub kind: String,
    /// The file or folder of a custom scan.
    pub target: Option<String>,
    /// "running" | "clean" | "threats" | "failed" | "cancelled".
    pub state: String,
    /// Unix seconds.
    pub started_at: u64,
    pub elapsed_secs: u64,
    /// Threat names Defender printed ("Trojan:Win32/Wacatac.B!ml").
    pub threats: Vec<String>,
    pub message: Option<String>,
    /// Defender refused because Orbit is not elevated.
    pub needs_admin: bool,
    pub exit_code: Option<i32>,
}

// ---------------------------------------------------------------------------
// Pure helpers (unit tested)
// ---------------------------------------------------------------------------

/// SecurityCenter2 `productState`: bits 12-15 are the product state
/// (0 off, 1 on, 2 snoozed, 3 expired), bits 4-7 the definitions
/// (0 up to date, 1 out of date). 397568 = 0x061100: on, up to date.
pub fn decode_product_state(state: u32) -> (&'static str, bool) {
    let s = match (state >> 12) & 0xF {
        1 => "on",
        2 => "snoozed",
        3 => "expired",
        _ => "off",
    };
    (s, (state >> 4) & 0xF == 0)
}

pub fn is_defender_product(name: &str, exe: &str) -> bool {
    let n = name.to_ascii_lowercase();
    exe.trim().to_ascii_lowercase().starts_with("windowsdefender:")
        || (n.contains("defender") && (n.contains("windows") || n.contains("microsoft")))
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// WMI CIM_DATETIME ("20261110054450.000000+000", the offset in minutes) to
/// Unix seconds. `None` for blanks, wildcards and dates before 1971.
pub fn dmtf_to_unix(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.len() < 14 || !s.as_bytes()[..14].iter().all(u8::is_ascii_digit) {
        return None;
    }
    let n = |a: usize, b: usize| s[a..b].parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (n(0, 4)?, n(4, 6)?, n(6, 8)?, n(8, 10)?, n(10, 12)?, n(12, 14)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let mut secs = days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + se;
    // ".mmmmmm+UUU": the sign is at index 21.
    if s.len() >= 25 {
        let sign = &s[21..22];
        if let Ok(off) = s[22..25].parse::<i64>() {
            match sign {
                "+" => secs -= off * 60,
                "-" => secs += off * 60,
                _ => {}
            }
        }
    }
    (secs > 31_536_000).then_some(secs as u64)
}

/// "4.18.26100.4-0" -> [4, 18, 26100, 4, 0], for picking the newest platform folder.
pub fn version_key(name: &str) -> Vec<u64> {
    name.split(['.', '-']).map(|p| p.trim().parse::<u64>().unwrap_or(0)).collect()
}

/// MpCmdRun writes ANSI text; some builds write UTF-16LE. Either way, to a String.
pub fn decode_output(bytes: &[u8]) -> String {
    let utf16 = bytes.starts_with(&[0xFF, 0xFE])
        || (bytes.len() >= 8 && bytes.iter().skip(1).step_by(2).take(64).filter(|&&b| b == 0).count() * 4 >= bytes.len().min(128));
    if utf16 {
        let start = if bytes.starts_with(&[0xFF, 0xFE]) { 2 } else { 0 };
        let units: Vec<u16> = bytes[start..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// Threat names from MpCmdRun's "Threat : <name>" lines, in order, no repeats.
pub fn parse_threats(output: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in output.lines() {
        let Some((key, value)) = line.trim().split_once(':') else { continue };
        let value = value.trim();
        if key.trim().eq_ignore_ascii_case("threat") && !value.is_empty() && !out.iter().any(|t| t == value) {
            out.push(value.to_string());
            if out.len() >= MAX_THREATS {
                break;
            }
        }
    }
    out
}

/// The "hr = 0x..." code MpCmdRun prints when it fails.
fn failure_code(output: &str) -> Option<u32> {
    let lower = output.to_ascii_lowercase();
    let at = lower.find("hr = 0x").or_else(|| lower.find("hr=0x"))?;
    let hex: String = lower[at..].split("0x").nth(1)?.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
    u32::from_str_radix(&hex, 16).ok()
}

/// (state, threats, message, needs_admin) for a finished (not cancelled) scan.
pub fn parse_result(exit_code: Option<i32>, output: &str) -> (&'static str, Vec<String>, Option<String>, bool) {
    let threats = parse_threats(output);
    if exit_code == Some(2) || !threats.is_empty() {
        let msg = if threats.is_empty() {
            Some("Defender found threats. Open Windows Security to see them.".to_string())
        } else {
            None
        };
        return ("threats", threats, msg, false);
    }
    if exit_code == Some(0) {
        return ("clean", Vec::new(), None, false);
    }
    let code = failure_code(output);
    let denied = code == Some(0x8007_0005) || output.to_ascii_lowercase().contains("access is denied");
    let msg = match code {
        _ if denied => "Defender needs administrator rights for this scan.".to_string(),
        Some(0x8001_06BA) => "Microsoft Defender is not running, so it can't scan.".to_string(),
        Some(c) => format!("Defender could not finish the scan (error 0x{c:08X})."),
        None => match exit_code {
            Some(c) => format!("Defender could not finish the scan (exit code {c})."),
            None => "Defender could not finish the scan.".to_string(),
        },
    };
    ("failed", Vec::new(), Some(msg), denied)
}

/// A custom-scan target: an existing file or folder on a local drive, written
/// as a plain `X:\...` path. Returns the path to pass to MpCmdRun.
pub fn validate_target(path: &str) -> Result<String, String> {
    let p = path.trim().replace('/', "\\");
    if p.is_empty() {
        return Err("Pick a file or folder to scan.".into());
    }
    let refuse = || Err("Orbit can only scan a file or folder on this PC.".to_string());
    if p.chars().count() > MAX_PATH_CHARS
        || p.chars().any(|c| c.is_control() || matches!(c, '"' | '*' | '?' | '<' | '>' | '|'))
    {
        return refuse();
    }
    let b = p.as_bytes();
    if b.len() < 3 || !b[0].is_ascii_alphabetic() || b[1] != b':' || b[2] != b'\\' {
        return refuse(); // UNC, \\?\, relative and drive-relative paths
    }
    if p[2..].contains(':') {
        return refuse(); // alternate data streams
    }
    if p[3..].split('\\').any(|c| c == "." || c == "..") {
        return refuse();
    }
    let root = HSTRING::from(&p[..3]);
    // DRIVE_REMOVABLE 2, DRIVE_FIXED 3, DRIVE_CDROM 5, DRIVE_RAMDISK 6; not
    // DRIVE_REMOTE 4 (network shares) or unknown / missing drives.
    let kind = unsafe { GetDriveTypeW(&root) };
    if !matches!(kind, 2 | 3 | 5 | 6) {
        return refuse();
    }
    let md = std::fs::metadata(&p).map_err(|_| "That file or folder no longer exists.".to_string())?;
    if !md.is_file() && !md.is_dir() {
        return refuse();
    }
    // A trailing backslash would be doubled inside quotes; keep it only on a drive root.
    let mut out = p;
    while out.len() > 3 && out.ends_with('\\') {
        out.pop();
    }
    Ok(out)
}

/// Combine what Defender and Windows Security say into one status.
pub fn combine(mp: Option<MpStatus>, products: Vec<AvProduct>, scanner_found: bool) -> DefenderStatus {
    let mut s = DefenderStatus { products, ..Default::default() };
    let defender_on = s.products.iter().any(|p| p.defender && p.state == "on");
    match mp {
        Some(mp) => {
            s.available = true;
            let passive = mp.running_mode.as_deref().is_some_and(|m| !m.eq_ignore_ascii_case("normal"));
            s.active = mp.service_enabled != Some(false) && mp.antivirus_enabled == Some(true) && !passive;
            s.running_mode = mp.running_mode;
            s.real_time = mp.real_time;
            s.signature_version = mp.signature_version;
            s.signature_updated = mp.signature_updated;
            s.signature_age_days = mp.signature_age_days.filter(|&d| d < 65_535);
            s.signatures_out_of_date = mp.signatures_out_of_date;
            s.quick_scan_at = mp.quick_scan_at;
            s.full_scan_at = mp.full_scan_at;
            s.engine_version = mp.engine_version;
            s.product_version = mp.product_version;
            s.tamper_protected = mp.tamper_protected;
        }
        None => s.active = defender_on,
    }
    if !s.active {
        let mut others: Vec<&AvProduct> = s.products.iter().filter(|p| !p.defender && p.state == "on").collect();
        others.sort_by_key(|p| !p.up_to_date);
        s.other = others.first().map(|p| (*p).clone());
    }
    s.can_scan = s.active && scanner_found;
    s
}

/// What MSFT_MpComputerStatus says, already decoded.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MpStatus {
    pub service_enabled: Option<bool>,
    pub antivirus_enabled: Option<bool>,
    pub real_time: Option<bool>,
    pub running_mode: Option<String>,
    pub signature_version: Option<String>,
    pub signature_updated: Option<u64>,
    pub signature_age_days: Option<u32>,
    pub signatures_out_of_date: Option<bool>,
    pub quick_scan_at: Option<u64>,
    pub full_scan_at: Option<u64>,
    pub engine_version: Option<String>,
    pub product_version: Option<String>,
    pub tamper_protected: Option<bool>,
}

// ---------------------------------------------------------------------------
// Readers
// ---------------------------------------------------------------------------

fn variant_bool(v: &VARIANT) -> Option<bool> {
    let vt = unsafe { v.as_raw().Anonymous.Anonymous.vt };
    (vt == VT_BOOL.0).then(|| bool::try_from(v).ok()).flatten()
}

fn variant_time(v: &VARIANT) -> Option<u64> {
    let vt = unsafe { v.as_raw().Anonymous.Anonymous.vt };
    if vt == VT_BSTR.0 {
        return dmtf_to_unix(&variant_string(v)?);
    }
    if vt == VT_DATE.0 {
        return health::ole_date_to_unix(unsafe { v.as_raw().Anonymous.Anonymous.Anonymous.date });
    }
    None
}

fn text(v: &VARIANT) -> Option<String> {
    variant_string(v).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn read_mp_status() -> Result<MpStatus, String> {
    let wmi = Wmi::connect(r"ROOT\Microsoft\Windows\Defender")
        .map_err(|e| format!("Defender's status is not available ({}).", e.code()))?;
    let rows = wmi
        .query("SELECT * FROM MSFT_MpComputerStatus")
        .map_err(|e| format!("Defender's status is not available ({}).", e.code()))?;
    let o = rows.first().ok_or("Defender's status is not available.")?;
    Ok(MpStatus {
        service_enabled: variant_bool(&prop(o, "AMServiceEnabled")),
        antivirus_enabled: variant_bool(&prop(o, "AntivirusEnabled")),
        real_time: variant_bool(&prop(o, "RealTimeProtectionEnabled")),
        running_mode: text(&prop(o, "AMRunningMode")),
        signature_version: text(&prop(o, "AntivirusSignatureVersion")),
        signature_updated: variant_time(&prop(o, "AntivirusSignatureLastUpdated")),
        signature_age_days: variant_u64(&prop(o, "AntivirusSignatureAge")).and_then(|d| u32::try_from(d).ok()),
        signatures_out_of_date: variant_bool(&prop(o, "DefenderSignaturesOutOfDate")),
        quick_scan_at: variant_time(&prop(o, "QuickScanEndTime")),
        full_scan_at: variant_time(&prop(o, "FullScanEndTime")),
        engine_version: text(&prop(o, "AMEngineVersion")),
        product_version: text(&prop(o, "AMProductVersion")),
        tamper_protected: variant_bool(&prop(o, "IsTamperProtected")),
    })
}

fn read_products() -> Vec<AvProduct> {
    let Ok(wmi) = Wmi::connect(r"ROOT\SecurityCenter2") else { return Vec::new() };
    let Ok(rows) = wmi.query("SELECT displayName, productState, pathToSignedProductExe FROM AntiVirusProduct") else {
        return Vec::new();
    };
    let mut out: Vec<AvProduct> = rows
        .iter()
        .filter_map(|o| {
            let name = text(&prop(o, "displayName"))?;
            let exe = text(&prop(o, "pathToSignedProductExe")).unwrap_or_default();
            let (state, up_to_date) = variant_u64(&prop(o, "productState"))
                .map(|n| decode_product_state(n as u32))
                .unwrap_or(("off", false));
            Some(AvProduct { defender: is_defender_product(&name, &exe), name, state: state.into(), up_to_date })
        })
        .collect();
    out.dedup_by(|a, b| a.name == b.name && a.state == b.state);
    out
}

fn reg_install_location() -> Option<PathBuf> {
    crate::rig::reg_sz(r"SOFTWARE\Microsoft\Windows Defender", "InstallLocation").map(PathBuf::from)
}

/// MpCmdRun.exe: the platform folder Defender names, else the newest one
/// under ProgramData, else the copy in Program Files.
pub fn mpcmdrun_path() -> Option<PathBuf> {
    let exe = |dir: &Path| Some(dir.join("MpCmdRun.exe")).filter(|p| p.is_file());
    if let Some(p) = reg_install_location().as_deref().and_then(exe) {
        return Some(p);
    }
    let data = std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    let platform = data.join(r"Microsoft\Windows Defender\Platform");
    if let Ok(dirs) = std::fs::read_dir(&platform) {
        let mut found: Vec<(Vec<u64>, PathBuf)> = dirs
            .flatten()
            .filter(|d| d.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|d| exe(&d.path()).map(|p| (version_key(&d.file_name().to_string_lossy()), p)))
            .collect();
        found.sort();
        if let Some((_, p)) = found.pop() {
            return Some(p);
        }
    }
    let pf = std::env::var_os("ProgramFiles").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
    exe(&pf.join("Windows Defender"))
}

fn is_admin() -> bool {
    // SAFETY: no arguments; inspects the current token only.
    unsafe { windows::Win32::UI::Shell::IsUserAnAdmin() }.as_bool()
}

pub fn read_status() -> DefenderStatus {
    let _com = health::com_init();
    let mp = read_mp_status();
    let products = read_products();
    let error = mp.as_ref().err().cloned();
    let mut s = combine(mp.ok(), products, mpcmdrun_path().is_some());
    s.error = error;
    s.read_at = now_secs();
    s.is_admin = is_admin();
    let slot = lock(&SLOT);
    s.scan = slot.current.clone().or_else(|| slot.last.clone());
    s
}

// ---------------------------------------------------------------------------
// Scans
// ---------------------------------------------------------------------------

struct Slot {
    current: Option<DefenderScan>,
    last: Option<DefenderScan>,
    next_id: u64,
}

static SLOT: Mutex<Slot> = Mutex::new(Slot { current: None, last: None, next_id: 1 });
static CANCEL: AtomicBool = AtomicBool::new(false);
/// The running MpCmdRun, so a cancel or Orbit's exit can end it.
static CHILD: Mutex<Option<Child>> = Mutex::new(None);

/// Orbit is closing: stop a running scan instead of leaving a hidden MpCmdRun behind.
pub(crate) fn on_exit() {
    CANCEL.store(true, Ordering::Release);
    if let Some(child) = lock(&CHILD).as_mut() {
        let _ = child.kill();
    }
}

/// Clears the running scan even if the worker panics.
struct RunningGuard;

impl Drop for RunningGuard {
    fn drop(&mut self) {
        let mut slot = lock(&SLOT);
        if let Some(mut cur) = slot.current.take() {
            if cur.state == "running" {
                cur.state = "failed".into();
                cur.message = Some("The scan stopped unexpectedly.".into());
            }
            slot.last = Some(cur);
        }
    }
}

fn emit(app: Option<&AppHandle>, scan: &DefenderScan) {
    if let Some(app) = app {
        let _ = app.emit(PROGRESS_EVENT, scan);
    }
}

fn read_all(mut r: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = r.read_to_end(&mut buf);
        buf
    })
}

/// Runs MpCmdRun and waits for it (or for a cancel). Blocking.
fn run_scan(app: Option<&AppHandle>, exe: &Path, scan_type: u8, mut scan: DefenderScan) -> DefenderScan {
    let _guard = RunningGuard;
    let mut cmd = Command::new(exe);
    cmd.arg("-Scan").arg("-ScanType").arg(scan_type.to_string());
    if let Some(t) = &scan.target {
        cmd.arg("-File").arg(t);
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).creation_flags(CREATE_NO_WINDOW);
    let started = Instant::now();
    let finish = |scan: &mut DefenderScan| {
        scan.elapsed_secs = started.elapsed().as_secs();
        let mut slot = lock(&SLOT);
        slot.current = None;
        slot.last = Some(scan.clone());
        drop(slot);
        emit(app, scan);
    };
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            scan.state = "failed".into();
            scan.message = Some(format!("Could not start Microsoft Defender: {e}."));
            finish(&mut scan);
            return scan;
        }
    };
    let out = child.stdout.take().map(read_all);
    let err = child.stderr.take().map(read_all);
    *lock(&CHILD) = Some(child);
    emit(app, &scan);
    let mut shown = 0;
    let status = loop {
        let polled = {
            let mut guard = lock(&CHILD);
            let Some(child) = guard.as_mut() else { break None };
            if CANCEL.load(Ordering::Acquire) {
                let _ = child.kill();
                let _ = child.wait();
                Err(())
            } else {
                child.try_wait().map_err(|_| ())
            }
        };
        match polled {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {}
            Err(()) => break None,
        }
        std::thread::sleep(Duration::from_millis(200));
        let secs = started.elapsed().as_secs();
        if secs != shown {
            shown = secs;
            scan.elapsed_secs = secs;
            if let Some(cur) = lock(&SLOT).current.as_mut() {
                cur.elapsed_secs = secs;
            }
            emit(app, &scan);
        }
    };
    *lock(&CHILD) = None;
    let mut bytes = out.and_then(|h| h.join().ok()).unwrap_or_default();
    bytes.extend(err.and_then(|h| h.join().ok()).unwrap_or_default());
    let output = decode_output(&bytes);
    #[cfg(test)]
    {
        *lock(&tests::LAST_OUTPUT) = output.clone();
    }
    if CANCEL.load(Ordering::Acquire) {
        scan.state = "cancelled".into();
        scan.message = Some("Scan cancelled.".into());
    } else {
        scan.exit_code = status.and_then(|s| s.code());
        let (state, threats, message, needs_admin) = parse_result(scan.exit_code, &output);
        scan.state = state.into();
        scan.threats = threats;
        scan.message = message;
        scan.needs_admin = needs_admin && !is_admin();
    }
    finish(&mut scan);
    scan
}

/// Claims the one scan slot. Err when a scan is already running.
fn begin(kind: &str, target: Option<String>) -> Result<DefenderScan, String> {
    let mut slot = lock(&SLOT);
    if slot.current.is_some() {
        return Err("A Defender scan is already running. Wait for it to finish or cancel it.".into());
    }
    let scan = DefenderScan {
        id: slot.next_id,
        kind: kind.into(),
        target,
        state: "running".into(),
        started_at: now_secs(),
        ..Default::default()
    };
    slot.next_id += 1;
    slot.current = Some(scan.clone());
    CANCEL.store(false, Ordering::Release);
    Ok(scan)
}

/// Starts a scan and waits for its result. Blocking; `app` None in tests.
pub fn scan_blocking(app: Option<&AppHandle>, kind: &str, path: Option<&str>) -> Result<DefenderScan, String> {
    let scan_type = match kind {
        "quick" => 1,
        "full" => 2,
        "custom" => 3,
        _ => return Err("Unknown scan type.".into()),
    };
    let target = if scan_type == 3 { Some(validate_target(path.unwrap_or(""))?) } else { None };
    let exe = mpcmdrun_path().ok_or("Orbit could not find Microsoft Defender's scanner (MpCmdRun.exe).")?;
    let scan = begin(kind, target)?;
    Ok(run_scan(app, &exe, scan_type, scan))
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn defender_status() -> Result<DefenderStatus, String> {
    tauri::async_runtime::spawn_blocking(read_status)
        .await
        .map_err(|e| format!("Could not read Defender's status: {e}"))
}

/// kind: "quick" | "full" | "custom" (with `path`). Resolves with the result.
#[tauri::command]
pub async fn defender_scan(app: AppHandle, kind: String, path: Option<String>) -> Result<DefenderScan, String> {
    tauri::async_runtime::spawn_blocking(move || scan_blocking(Some(&app), &kind, path.as_deref()))
        .await
        .map_err(|e| format!("The scan stopped unexpectedly: {e}"))?
}

/// Stops the running scan (kills MpCmdRun). No-op when none runs.
#[tauri::command]
pub fn defender_cancel() -> Result<(), String> {
    if lock(&SLOT).current.is_some() {
        CANCEL.store(true, Ordering::Release);
    }
    Ok(())
}

/// A file or folder picker for a custom scan. Ok(None) when cancelled.
#[tauri::command]
pub async fn defender_pick_target(app: AppHandle, folder: bool) -> Result<Option<String>, String> {
    let owner = app
        .get_webview_window("main")
        .and_then(|w| w.hwnd().ok())
        .map(|h| h.0 as isize)
        .unwrap_or(0);
    tauri::async_runtime::spawn_blocking(move || {
        std::thread::spawn(move || pick(owner, folder))
            .join()
            .unwrap_or_else(|_| Err("The file picker failed.".to_string()))
    })
    .await
    .map_err(|e| e.to_string())?
}

fn pick(owner: isize, folder: bool) -> Result<Option<String>, String> {
    struct Com;
    impl Drop for Com {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }
    let fail = |e: windows::core::Error| format!("Could not open the file picker: {}", e.message());
    unsafe {
        let _com = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).is_ok().then_some(Com);
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).map_err(fail)?;
        dialog
            .SetTitle(if folder { w!("Pick a folder to scan") } else { w!("Pick a file to scan") })
            .map_err(fail)?;
        let mut options = dialog.GetOptions().map_err(fail)? | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM | FOS_NOCHANGEDIR;
        if folder {
            options |= FOS_PICKFOLDERS;
        }
        dialog.SetOptions(options).map_err(fail)?;
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

/// Opens Windows Security: its threat page for `"threats"`, else Virus &
/// threat protection. Fixed targets only.
#[tauri::command]
pub fn open_windows_security(page: Option<String>) -> Result<(), String> {
    let first = if page.as_deref() == Some("threats") { "windowsdefender://threat" } else { "windowsdefender://threatsettings" };
    for target in [first, "windowsdefender://", "ms-settings:windowsdefender"] {
        let r = unsafe {
            ShellExecuteW(HWND::default(), w!("open"), &HSTRING::from(target), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL)
        };
        if (r.0 as isize) > 32 {
            return Ok(());
        }
    }
    Err("Could not open Windows Security.".into())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// MpCmdRun's raw output from the last scan, for the live test.
    pub(super) static LAST_OUTPUT: Mutex<String> = Mutex::new(String::new());

    #[test]
    fn product_states() {
        assert_eq!(decode_product_state(397_568), ("on", true)); // Defender, on
        assert_eq!(decode_product_state(393_472), ("off", true)); // Defender, off for another AV
        assert_eq!(decode_product_state(266_240), ("on", true));
        assert_eq!(decode_product_state(266_256), ("on", false)); // out of date
        assert_eq!(decode_product_state(262_144), ("off", true));
        assert_eq!(decode_product_state(0x0004_2000), ("snoozed", true));
        assert_eq!(decode_product_state(0x0004_3000), ("expired", true));
    }

    #[test]
    fn defender_names() {
        assert!(is_defender_product("Windows Defender", "windowsdefender://"));
        assert!(is_defender_product("Microsoft Defender Antivirus", ""));
        assert!(!is_defender_product("Bitdefender Antivirus", r"C:\Program Files\Bitdefender\wsccommunicator.exe"));
        assert!(!is_defender_product("Norton 360", ""));
    }

    #[test]
    fn dmtf_dates() {
        // 2026-11-10 05:44:50 UTC
        assert_eq!(dmtf_to_unix("20261110054450.000000+000"), Some(1_794_289_490));
        assert_eq!(dmtf_to_unix("20261110074450.000000+120"), Some(1_794_289_490));
        assert_eq!(dmtf_to_unix("20261110004450.000000-300"), Some(1_794_289_490));
        assert_eq!(dmtf_to_unix("20240229000000.000000+000"), Some(1_709_164_800));
        assert_eq!(dmtf_to_unix(""), None);
        assert_eq!(dmtf_to_unix("16010101000000.000000+000"), None);
        assert_eq!(dmtf_to_unix("********************.******+***"), None);
    }

    #[test]
    fn platform_versions() {
        let mut v = vec!["4.18.26090.9-0", "4.18.26100.4-0", "4.18.2205.7-0"];
        v.sort_by_key(|n| version_key(n));
        assert_eq!(v.last(), Some(&"4.18.26100.4-0"));
    }

    #[test]
    fn decodes_both_encodings() {
        assert_eq!(decode_output(b"Scan starting...\r\n"), "Scan starting...\r\n");
        let wide: Vec<u8> = "Scan finished.".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        assert_eq!(decode_output(&wide), "Scan finished.");
        let mut bom = vec![0xFF, 0xFE];
        bom.extend(&wide);
        assert_eq!(decode_output(&bom), "Scan finished.");
    }

    const THREATS: &str = "Scan starting...\r\nScan finished.\r\nScanning C:\\Users\\you\\Downloads\\x.zip found 2 threats.\r\n\r\n\
<===========================LIST OF DETECTED THREATS==========================>\r\n\
----------------------------- Threat information ------------------------------\r\n\
Threat                  : Trojan:Win32/Wacatac.B!ml\r\n\
Resources               : 1 total\r\n\
    file                : C:\\Users\\you\\Downloads\\x.zip->a.exe\r\n\
-------------------------------------------------------------------------------\r\n\
----------------------------- Threat information ------------------------------\r\n\
Threat                  : PUA:Win32/Presenoker\r\n\
Resources               : 1 total\r\n\
    file                : C:\\Users\\you\\Downloads\\x.zip->b.exe\r\n";

    #[test]
    fn parses_results() {
        let clean = "Scan starting...\nScan finished.\nScanning C:\\t\\a.txt found no threats.\n";
        assert_eq!(parse_result(Some(0), clean), ("clean", vec![], None, false));
        let (state, threats, msg, _) = parse_result(Some(2), THREATS);
        assert_eq!(state, "threats");
        assert_eq!(threats, vec!["Trojan:Win32/Wacatac.B!ml", "PUA:Win32/Presenoker"]);
        assert_eq!(msg, None);
        // Threats found but no names printed.
        let (state, threats, msg, _) = parse_result(Some(2), "Scan finished.\n");
        assert_eq!((state, threats.len()), ("threats", 0));
        assert!(msg.unwrap().contains("Open Windows Security"));
        let denied = "CmdTool: Failed with hr = 0x80070005. Check C:\\x\\MpCmdRun.log for more information";
        let (state, _, msg, admin) = parse_result(Some(-2147024891), denied);
        assert_eq!((state, admin), ("failed", true));
        assert!(msg.unwrap().contains("administrator"));
        let (state, _, msg, admin) = parse_result(Some(1), "CmdTool: Failed with hr = 0x80508023.");
        assert_eq!((state, admin), ("failed", false));
        assert_eq!(msg.as_deref(), Some("Defender could not finish the scan (error 0x80508023)."));
        let (_, _, msg, _) = parse_result(Some(5), "");
        assert_eq!(msg.as_deref(), Some("Defender could not finish the scan (exit code 5)."));
    }

    #[test]
    fn targets() {
        let dir = std::env::temp_dir().join(format!("orbit-defender-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a b.txt");
        std::fs::write(&file, b"hello").unwrap();
        let f = file.to_string_lossy().into_owned();
        assert_eq!(validate_target(&f).as_deref(), Ok(f.as_str()));
        assert_eq!(validate_target(&format!("  {f}  ")).as_deref(), Ok(f.as_str()));
        let d = dir.to_string_lossy().into_owned();
        assert_eq!(validate_target(&format!("{d}\\")).as_deref(), Ok(d.as_str()));
        assert_eq!(validate_target(&d.replace('\\', "/")).as_deref(), Ok(d.as_str()));
        assert!(validate_target(&format!("{d}\\missing.exe")).unwrap_err().contains("no longer exists"));
        for bad in [
            "",
            "relative\\a.txt",
            "C:a.txt",
            r"\\server\share\a.exe",
            r"\\?\C:\Windows\notepad.exe",
            r"\\.\PhysicalDrive0",
            r"C:\Windows\notepad.exe:stream",
            r"C:\Windows\..\Windows\notepad.exe",
            "C:\\Windows\\\"notepad.exe",
            r"C:\Windows\*.exe",
            "C:\\Windows\\note\npad.exe",
        ] {
            assert!(validate_target(bad).is_err(), "accepted {bad:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn av(name: &str, defender: bool, state: &str, up_to_date: bool) -> AvProduct {
        AvProduct { name: name.into(), defender, state: state.into(), up_to_date }
    }

    #[test]
    fn combines_status() {
        let on = MpStatus { service_enabled: Some(true), antivirus_enabled: Some(true), real_time: Some(true), running_mode: Some("Normal".into()), ..Default::default() };
        let s = combine(Some(on.clone()), vec![av("Windows Defender", true, "on", true)], true);
        assert!(s.available && s.active && s.can_scan && s.other.is_none());
        assert!(!combine(Some(on.clone()), vec![], false).can_scan); // no MpCmdRun

        // Another antivirus took over: Defender passive, scans hidden.
        let passive = MpStatus { running_mode: Some("Passive Mode".into()), real_time: Some(false), ..on.clone() };
        let products = vec![av("Windows Defender", true, "off", true), av("Norton 360", false, "on", true)];
        let s = combine(Some(passive), products.clone(), true);
        assert!(!s.active && !s.can_scan);
        assert_eq!(s.other.as_ref().map(|p| p.name.as_str()), Some("Norton 360"));

        // Defender's own WMI gone (third-party AV disabled it): SecurityCenter2 decides.
        let s = combine(None, products, true);
        assert!(!s.available && !s.active && s.other.is_some());
        let s = combine(None, vec![av("Windows Defender", true, "on", true)], true);
        assert!(s.active && s.can_scan);

        // Defender off and nothing else on.
        let off = MpStatus { antivirus_enabled: Some(false), ..on };
        let s = combine(Some(off), vec![av("Avast", false, "off", false)], true);
        assert!(!s.active && s.other.is_none() && !s.can_scan);
    }

    #[test]
    fn one_scan_at_a_time() {
        let a = begin("quick", None).unwrap();
        assert!(begin("full", None).unwrap_err().contains("already running"));
        drop(RunningGuard); // the worker's guard on exit
        let b = begin("full", None).unwrap();
        assert!(b.id > a.id);
        let _ = lock(&SLOT).current.take();
        assert_eq!(scan_blocking(None, "deep", None).unwrap_err(), "Unknown scan type.");
        assert!(scan_blocking(None, "custom", Some(r"\\server\share\x.exe")).is_err());
    }

    /// Read-only: prints this PC's Defender and Windows Security status.
    #[test]
    fn reads_real_status() {
        let s = read_status();
        println!("{s:#?}");
        println!("MpCmdRun: {:?}", mpcmdrun_path());
    }

    /// Runs ONE real custom scan of a harmless file. Not part of the normal run:
    /// `ORBIT_DEFENDER_SCAN_FILE=<file> cargo test real_custom_scan -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_custom_scan() {
        let file = std::env::var("ORBIT_DEFENDER_SCAN_FILE").expect("set ORBIT_DEFENDER_SCAN_FILE");
        let r = scan_blocking(None, "custom", Some(&file));
        println!("{r:#?}");
        println!("MpCmdRun output:\n{}", lock(&LAST_OUTPUT));
        let r = r.unwrap();
        assert_eq!(r.state, "clean", "{r:?}");
    }
}
