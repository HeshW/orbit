//! PC health report (Optimize > Health, 0.5) and startup suggestions.
//!
//! One read-only pass over the things that most often explain "this PC feels
//! wrong": disk health and free space, uptime and a pending restart, Windows
//! Update, startup load and the security scan. Each item gets a status
//! (good / warning / problem / unknown), one plain sentence and, where Orbit
//! can help, an action the UI turns into a button. Nothing here changes the
//! system: registry keys are opened for reading, WMI and the Windows Update
//! Agent are only queried (search, never download or install).
//!
//! Windows Update can take from a second to minutes, so it runs on its own
//! thread and is cached (`get_update_status`); the report shows whatever the
//! cache has and the UI polls until the search is done.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::State;
use windows::core::{BSTR, HSTRING, PCWSTR, VARIANT};
use windows::Win32::Foundation::{ERROR_SUCCESS, HWND, VARIANT_BOOL};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoSetProxyBlanket, CoUninitialize, CLSCTX_ALL,
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, EOAC_NONE, RPC_C_AUTHN_LEVEL_CALL,
    RPC_C_IMP_LEVEL_IMPERSONATE, SAFEARRAY,
};
use windows::Win32::System::Ole::{SafeArrayGetElement, SafeArrayGetLBound, SafeArrayGetUBound};
use windows::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE,
    KEY_WOW64_64KEY,
};
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::UpdateAgent::{
    AutomaticUpdates, IAutomaticUpdates2, IUpdate, IUpdateSession, UpdateSession,
};
use windows::Win32::System::Variant::{VT_ARRAY, VT_BSTR, VT_DATE, VT_I2, VT_I4, VT_UI2, VT_UI4};
use windows::Win32::System::Wmi::{
    IWbemClassObject, IWbemLocator, IWbemServices, WbemLocator, WBEM_FLAG_FORWARD_ONLY,
    WBEM_FLAG_RETURN_IMMEDIATELY,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::actions::normalize_path;
use crate::model::StartupEntry;
use crate::sampler::AppState;

// ---------------------------------------------------------------------------
// Shapes (docs/CONTRACT.md "v0.5")
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct HealthReport {
    /// Unix seconds.
    pub generated_at: u64,
    pub is_admin: bool,
    /// In display order: disks, space, restart, updates, devices, uptime, startup, security.
    pub items: Vec<HealthItem>,
    pub drives: Vec<DriveSpace>,
    pub disks: Vec<PhysicalDisk>,
    /// Why disk details are missing, e.g. "Restart as administrator to see wear and temperature."
    pub disk_note: Option<String>,
    pub update: UpdateStatus,
    pub uptime_secs: u64,
    /// Plain sentences, one per restart flag found. Empty: no restart pending.
    pub restart_reasons: Vec<String>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HealthItem {
    /// "disks" | "space" | "restart" | "updates" | "devices" | "uptime" | "startup" | "security"
    pub key: String,
    pub title: String,
    /// "good" | "warning" | "problem" | "unknown"
    pub status: String,
    pub text: String,
    pub detail: Option<String>,
    pub action: Option<HealthAction>,
}

/// What the item's button does. kind "view": go to view `target`
/// ("startup", "security"); "optimize": open Optimize sub-tab `target`
/// ("cleanup", ...); "settings": `open_settings_page(target)`; "admin":
/// Restart as administrator (target "").
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HealthAction {
    pub label: String,
    pub kind: String,
    pub target: String,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DriveSpace {
    /// "C:\\"
    pub mount: String,
    /// Volume label, "" if none.
    pub name: String,
    pub file_system: String,
    pub total: u64,
    pub available: u64,
    pub removable: bool,
    /// "good" | "warning" | "problem"
    pub status: String,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct PhysicalDisk {
    /// Disk number as a string ("0").
    pub device_id: String,
    pub name: String,
    /// "SSD" | "HDD" | "SCM" | "Unknown"
    pub media_type: String,
    /// "NVMe", "SATA", "USB", ...
    pub bus_type: String,
    pub size: u64,
    /// "healthy" | "warning" | "unhealthy" | "unknown"
    pub health: String,
    /// Plain words for any status other than OK ("Predictive failure").
    pub problems: Vec<String>,
    /// Drive letters on this disk ("C:", "E:").
    pub drives: Vec<String>,
    /// Percent of rated wear used (SSD), admin only.
    pub wear_pct: Option<u32>,
    pub temperature_c: Option<u32>,
    pub power_on_hours: Option<u64>,
    pub read_errors: Option<u64>,
    pub write_errors: Option<u64>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    /// "idle" (never checked) | "checking" | "ready" | "error"
    pub state: String,
    /// When Windows last searched / installed successfully (Unix secs).
    pub last_search: Option<u64>,
    pub last_install: Option<u64>,
    /// Updates found and not installed (software, not optional). None until a search finished.
    pub pending: Option<u32>,
    /// Up to 10 titles.
    pub pending_titles: Vec<String>,
    /// When Orbit's search finished (Unix secs).
    pub checked_at: Option<u64>,
    /// The last finished search asked the update server (true) or Windows' own cache (false).
    pub online: bool,
    pub error: Option<String>,
}

impl UpdateStatus {
    fn idle() -> Self {
        Self {
            state: "idle".into(),
            last_search: None,
            last_install: None,
            pending: None,
            pending_titles: Vec::new(),
            checked_at: None,
            online: false,
            error: None,
        }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StartupSuggestion {
    /// StartupEntry.id, for `set_startup_enabled`.
    pub id: String,
    pub name: String,
    pub path: Option<String>,
    /// "high" | "medium"
    pub impact: String,
    pub cpu_ms: u64,
    pub disk_read_bytes: u64,
    pub publisher: Option<String>,
    pub needs_admin: bool,
    pub measured_at: u64,
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

const GB: u64 = 1024 * 1024 * 1024;
const MB: u64 = 1024 * 1024;

pub(crate) fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn item(key: &str, title: &str, status: &str, text: impl Into<String>) -> HealthItem {
    HealthItem {
        key: key.into(),
        title: title.into(),
        status: status.into(),
        text: text.into(),
        detail: None,
        action: None,
    }
}

fn action(label: &str, kind: &str, target: &str) -> Option<HealthAction> {
    Some(HealthAction { label: label.into(), kind: kind.into(), target: target.into() })
}

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// "today", "yesterday", "3 days ago".
fn days_ago(then: u64, now: u64) -> String {
    match now.saturating_sub(then) / 86_400 {
        0 => "today".into(),
        1 => "yesterday".into(),
        d => format!("{d} days ago"),
    }
}

fn gb_text(bytes: u64) -> String {
    let gb = bytes as f64 / GB as f64;
    if gb >= 10.0 {
        format!("{gb:.0} GB")
    } else {
        format!("{gb:.1} GB")
    }
}

/// Uninitializes COM on drop when this thread's init succeeded.
pub(crate) struct ComGuard(bool);

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

pub(crate) fn com_init() -> ComGuard {
    // S_OK and S_FALSE both need a matching CoUninitialize.
    ComGuard(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
}

/// OLE automation date (days since 1899-12-30, UTC here) to Unix seconds.
/// None for 0 / before 1970 (WUA's "never").
pub(crate) fn ole_date_to_unix(date: f64) -> Option<u64> {
    let secs = (date - 25_569.0) * 86_400.0;
    (date.is_finite() && secs > 0.0).then(|| secs.round() as u64)
}

fn variant_vt(v: &VARIANT) -> u16 {
    unsafe { v.as_raw().Anonymous.Anonymous.vt }
}

fn variant_date(v: &VARIANT) -> Option<u64> {
    if variant_vt(v) != VT_DATE.0 {
        return None;
    }
    ole_date_to_unix(unsafe { v.as_raw().Anonymous.Anonymous.Anonymous.date })
}

pub(crate) fn variant_string(v: &VARIANT) -> Option<String> {
    if variant_vt(v) != VT_BSTR.0 {
        return None;
    }
    BSTR::try_from(v).ok().map(|b| b.to_string())
}

/// Numbers from WMI: uint16/uint32 arrive as VT_I4, uint64 as a decimal string.
pub(crate) fn variant_u64(v: &VARIANT) -> Option<u64> {
    match variant_vt(v) {
        x if x == VT_BSTR.0 => variant_string(v)?.trim().parse().ok(),
        x if x == VT_I4.0 || x == VT_UI4.0 || x == VT_I2.0 || x == VT_UI2.0 => {
            i64::try_from(v).ok().and_then(|n| u64::try_from(n).ok())
        }
        _ => u64::try_from(v).ok(),
    }
}

/// Elements of a VT_ARRAY of 16- or 32-bit integers (WMI uint16[]).
fn variant_int_array(v: &VARIANT) -> Vec<i64> {
    let vt = variant_vt(v);
    if vt & VT_ARRAY.0 == 0 {
        return Vec::new();
    }
    let elem = vt & !VT_ARRAY.0;
    let psa = unsafe { v.as_raw().Anonymous.Anonymous.Anonymous.parray } as *const SAFEARRAY;
    if psa.is_null() {
        return Vec::new();
    }
    let (Ok(lo), Ok(hi)) = (unsafe { SafeArrayGetLBound(psa, 1) }, unsafe { SafeArrayGetUBound(psa, 1) })
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for i in lo..=hi.min(lo + 64) {
        let n = if elem == VT_I2.0 || elem == VT_UI2.0 {
            let mut x: u16 = 0;
            unsafe { SafeArrayGetElement(psa, &i, &mut x as *mut u16 as *mut _) }.map(|_| x as i64)
        } else {
            let mut x: i32 = 0;
            unsafe { SafeArrayGetElement(psa, &i, &mut x as *mut i32 as *mut _) }.map(|_| x as i64)
        };
        if let Ok(n) = n {
            out.push(n);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Free space per drive
// ---------------------------------------------------------------------------

/// Warning below 10% of the drive (at least 5 GB, at most 25 GB); problem
/// below 3% (at least 1 GB, at most 5 GB). A big drive with 60 GB free is fine.
pub fn space_status(total: u64, available: u64) -> &'static str {
    if total == 0 {
        return "good";
    }
    let warn = (total / 10).clamp(5 * GB, 25 * GB);
    let problem = (total * 3 / 100).clamp(GB, 5 * GB);
    if available < problem {
        "problem"
    } else if available < warn {
        "warning"
    } else {
        "good"
    }
}

fn read_drives() -> Vec<DriveSpace> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let mut out: Vec<DriveSpace> = disks
        .list()
        .iter()
        .filter(|d| d.total_space() > 0)
        .map(|d| DriveSpace {
            mount: d.mount_point().to_string_lossy().into_owned(),
            name: d.name().to_string_lossy().into_owned(),
            file_system: d.file_system().to_string_lossy().into_owned(),
            total: d.total_space(),
            available: d.available_space(),
            removable: d.is_removable(),
            status: space_status(d.total_space(), d.available_space()).into(),
        })
        .collect();
    out.sort_by(|a, b| a.mount.cmp(&b.mount));
    out
}

fn rank(status: &str) -> u8 {
    match status {
        "problem" => 3,
        "warning" => 2,
        "unknown" => 1,
        _ => 0,
    }
}

fn drive_letter(mount: &str) -> &str {
    mount.trim_end_matches('\\')
}

pub fn space_item(drives: &[DriveSpace]) -> HealthItem {
    let fixed: Vec<&DriveSpace> = drives.iter().filter(|d| !d.removable).collect();
    let Some(worst) = fixed.iter().copied().max_by_key(|d| (rank(&d.status), u64::MAX - d.available))
    else {
        return item("space", "Free space", "unknown", "Orbit could not read your drives.");
    };
    let mut it = match worst.status.as_str() {
        "good" => item(
            "space",
            "Free space",
            "good",
            if fixed.len() == 1 {
                format!("{} has {} free.", drive_letter(&worst.mount), gb_text(worst.available))
            } else {
                format!("All {} drives have room to spare.", fixed.len())
            },
        ),
        status => {
            let low: Vec<String> = fixed
                .iter()
                .filter(|d| d.status != "good")
                .map(|d| format!("{} ({} free)", drive_letter(&d.mount), gb_text(d.available)))
                .collect();
            let text = if status == "problem" {
                format!("{} is almost full. Windows and apps slow down or fail when a drive fills up.", low.join(", "))
            } else {
                format!("{} is getting full.", low.join(", "))
            };
            item("space", "Free space", status, text)
        }
    };
    if it.status != "good" {
        it.action = action("Free up space", "optimize", "cleanup");
    }
    it
}

// ---------------------------------------------------------------------------
// Disk health (Storage WMI)
// ---------------------------------------------------------------------------

fn media_type(code: Option<u64>) -> &'static str {
    match code {
        Some(3) => "HDD",
        Some(4) => "SSD",
        Some(5) => "SCM",
        _ => "Unknown",
    }
}

fn bus_type(code: Option<u64>) -> String {
    match code {
        Some(1) => "SCSI",
        Some(2) => "ATAPI",
        Some(3) => "ATA",
        Some(4) => "FireWire",
        Some(5) => "SSA",
        Some(6) => "Fibre Channel",
        Some(7) => "USB",
        Some(8) => "RAID",
        Some(9) => "iSCSI",
        Some(10) => "SAS",
        Some(11) => "SATA",
        Some(12) => "SD",
        Some(13) => "MMC",
        Some(14) | Some(15) => "Virtual",
        Some(16) => "Storage Spaces",
        Some(17) => "NVMe",
        Some(18) => "SCM",
        Some(19) => "UFS",
        _ => "",
    }
    .to_string()
}

fn health_status(code: Option<u64>) -> &'static str {
    match code {
        Some(0) => "healthy",
        Some(1) => "warning",
        Some(2) => "unhealthy",
        _ => "unknown",
    }
}

/// OperationalStatus codes other than OK, in plain words.
fn operational_problem(code: i64) -> Option<String> {
    let s = match code {
        2 | 11 | 17 | 18 => return None, // OK, In service, Completed, Power mode
        0 | 1 => return None,             // Unknown / Other say nothing useful
        3 => "Degraded",
        4 => "Stressed",
        5 => "Predictive failure: the disk expects to fail soon",
        6 => "Error",
        7 => "Non-recoverable error",
        8 => "Starting",
        9 => "Stopping",
        10 => "Stopped",
        12 => "No contact",
        13 => "Lost communication",
        14 => "Aborted",
        15 => "Dormant",
        16 => "A part it depends on has an error",
        0xD010 => "In maintenance",
        0xD011 => "Transient error",
        0xD012 => "Failed media",
        _ => return Some(format!("Status code {code}")),
    };
    Some(s.to_string())
}

pub(crate) struct Wmi {
    svc: IWbemServices,
}

impl Wmi {
    pub(crate) fn connect(namespace: &str) -> windows::core::Result<Self> {
        let locator: IWbemLocator =
            unsafe { CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER) }?;
        let empty = BSTR::new();
        let svc = unsafe {
            locator.ConnectServer(&BSTR::from(namespace), &empty, &empty, &empty, 0, &empty, None)
        }?;
        // RPC_C_AUTHN_WINNT = 10, RPC_C_AUTHZ_NONE = 0. Per proxy, so the
        // process-wide COM security stays as Tauri left it.
        unsafe {
            CoSetProxyBlanket(
                &svc,
                10,
                0,
                PCWSTR::null(),
                RPC_C_AUTHN_LEVEL_CALL,
                RPC_C_IMP_LEVEL_IMPERSONATE,
                None,
                EOAC_NONE,
            )
        }?;
        Ok(Self { svc })
    }

    pub(crate) fn query(&self, wql: &str) -> windows::core::Result<Vec<IWbemClassObject>> {
        // 10 s per row is plenty; a hung provider then ends the list.
        self.query_timeout(wql, 10_000)
    }

    /// `query` with its own wait per row (ms); sensors.rs reads every second.
    pub(crate) fn query_timeout(&self, wql: &str, row_ms: i32) -> windows::core::Result<Vec<IWbemClassObject>> {
        let rows = unsafe {
            self.svc.ExecQuery(
                &BSTR::from("WQL"),
                &BSTR::from(wql),
                WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
                None,
            )
        }?;
        let mut out = Vec::new();
        loop {
            let mut objs = [None];
            let mut returned = 0u32;
            let hr = unsafe { rows.Next(row_ms, &mut objs, &mut returned) };
            if returned == 0 {
                hr.ok()?;
                break;
            }
            if let Some(o) = objs[0].take() {
                out.push(o);
            }
            if out.len() > 256 {
                break;
            }
        }
        Ok(out)
    }
}

pub(crate) fn prop(obj: &IWbemClassObject, name: &str) -> VARIANT {
    let mut v = VARIANT::new();
    let _ = unsafe { obj.Get(&HSTRING::from(name), 0, &mut v, None, None) };
    v
}

/// (disks, note). Unelevated, the reliability counters are refused and the
/// note says so; without the Storage provider the list is empty.
pub(crate) fn read_physical_disks(is_admin: bool) -> (Vec<PhysicalDisk>, Option<String>) {
    let _com = com_init();
    let wmi = match Wmi::connect(r"ROOT\Microsoft\Windows\Storage") {
        Ok(w) => w,
        Err(_) => return (Vec::new(), Some("Windows did not share disk health.".into())),
    };
    let rows = match wmi.query(
        "SELECT DeviceId, FriendlyName, MediaType, BusType, HealthStatus, OperationalStatus, Size FROM MSFT_PhysicalDisk",
    ) {
        Ok(r) => r,
        Err(_) => {
            let note = if is_admin {
                "Windows did not share disk health."
            } else {
                "Windows did not share disk health. Try Restart as administrator."
            };
            return (Vec::new(), Some(note.into()));
        }
    };
    let mut disks: Vec<PhysicalDisk> = rows
        .iter()
        .map(|o| PhysicalDisk {
            device_id: variant_string(&prop(o, "DeviceId"))
                .or_else(|| variant_u64(&prop(o, "DeviceId")).map(|n| n.to_string()))
                .unwrap_or_default(),
            name: variant_string(&prop(o, "FriendlyName")).unwrap_or_else(|| "Disk".into()),
            media_type: media_type(variant_u64(&prop(o, "MediaType"))).into(),
            bus_type: bus_type(variant_u64(&prop(o, "BusType"))),
            size: variant_u64(&prop(o, "Size")).unwrap_or(0),
            health: health_status(variant_u64(&prop(o, "HealthStatus"))).into(),
            problems: variant_int_array(&prop(o, "OperationalStatus"))
                .into_iter()
                .filter_map(operational_problem)
                .collect(),
            ..Default::default()
        })
        .collect();

    // Drive letters per disk number.
    if let Ok(parts) = wmi.query("SELECT DiskNumber, DriveLetter FROM MSFT_Partition") {
        let mut letters: HashMap<String, Vec<String>> = HashMap::new();
        for p in &parts {
            let (Some(num), Some(letter)) =
                (variant_u64(&prop(p, "DiskNumber")), variant_u64(&prop(p, "DriveLetter")))
            else {
                continue;
            };
            if let Some(c) = char::from_u32(letter as u32).filter(|c| c.is_ascii_alphabetic()) {
                letters.entry(num.to_string()).or_default().push(format!("{}:", c.to_ascii_uppercase()));
            }
        }
        for d in &mut disks {
            if let Some(mut l) = letters.remove(&d.device_id) {
                l.sort();
                l.dedup();
                d.drives = l;
            }
        }
    }

    // Wear, temperature and error counts: usually admin only.
    let mut got_counters = false;
    if let Ok(counters) = wmi.query(
        "SELECT DeviceId, Temperature, Wear, PowerOnHours, ReadErrorsUncorrected, WriteErrorsUncorrected FROM MSFT_StorageReliabilityCounter",
    ) {
        for c in &counters {
            let id = variant_string(&prop(c, "DeviceId"))
                .or_else(|| variant_u64(&prop(c, "DeviceId")).map(|n| n.to_string()))
                .unwrap_or_default();
            let Some(d) = disks.iter_mut().find(|d| d.device_id == id) else {
                continue;
            };
            got_counters = true;
            // 0 means "not reported" for temperature; wear 0 is a real value.
            d.temperature_c = variant_u64(&prop(c, "Temperature")).filter(|&t| t > 0 && t < 150).map(|t| t as u32);
            d.wear_pct = variant_u64(&prop(c, "Wear")).filter(|&w| w <= 100).map(|w| w as u32);
            d.power_on_hours = variant_u64(&prop(c, "PowerOnHours")).filter(|&h| h > 0);
            d.read_errors = variant_u64(&prop(c, "ReadErrorsUncorrected"));
            d.write_errors = variant_u64(&prop(c, "WriteErrorsUncorrected"));
        }
    }
    disks.sort_by(|a, b| {
        a.device_id.parse::<u32>().unwrap_or(u32::MAX).cmp(&b.device_id.parse::<u32>().unwrap_or(u32::MAX))
    });
    let note = match (got_counters, is_admin, disks.is_empty()) {
        (_, _, true) => Some("Windows did not list any disks.".into()),
        (false, false, _) => Some("Restart as administrator to see wear, temperature and error counts.".into()),
        (false, true, _) => Some("These disks do not report wear or temperature.".into()),
        _ => None,
    };
    (disks, note)
}

/// Worst first: unhealthy or failing, then warning / high wear.
pub fn disk_status(d: &PhysicalDisk) -> &'static str {
    let failing = d.problems.iter().any(|p| {
        p.starts_with("Predictive") || p.contains("error") || p == "Error" || p == "Failed media"
    });
    if d.health == "unhealthy" || failing || d.wear_pct.is_some_and(|w| w >= 90) {
        "problem"
    } else if d.health == "warning" || !d.problems.is_empty() || d.wear_pct.is_some_and(|w| w >= 70) {
        "warning"
    } else if d.health == "unknown" {
        "unknown"
    } else {
        "good"
    }
}

pub fn disks_item(disks: &[PhysicalDisk], note: Option<&str>, is_admin: bool) -> HealthItem {
    let title = "Disk health";
    if disks.is_empty() {
        let mut it = item("disks", title, "unknown", note.unwrap_or("Windows did not share disk health."));
        if !is_admin {
            it.action = action("Restart as administrator", "admin", "");
        }
        return it;
    }
    let worst = disks.iter().map(disk_status).max_by_key(|s| rank(s)).unwrap_or("good");
    let named = |pick: &dyn Fn(&PhysicalDisk) -> bool| -> String {
        disks.iter().filter(|d| pick(d)).map(|d| d.name.clone()).collect::<Vec<_>>().join(", ")
    };
    let mut it = match worst {
        "problem" => item(
            "disks",
            title,
            "problem",
            format!("{} reports a problem. Back up your files soon.", named(&|d| disk_status(d) == "problem")),
        ),
        "warning" => item(
            "disks",
            title,
            "warning",
            format!("{} reports a warning. Keep a backup of anything important.", named(&|d| disk_status(d) == "warning")),
        ),
        "unknown" => item("disks", title, "unknown", "Windows does not know the health of some disks."),
        _ => item(
            "disks",
            title,
            "good",
            if disks.len() == 1 {
                "Your disk reports healthy.".to_string()
            } else {
                format!("All {} disks report healthy.", disks.len())
            },
        ),
    };
    it.detail = note.map(str::to_string);
    if !is_admin && note.is_some() {
        it.action = action("Restart as administrator", "admin", "");
    }
    it
}

// ---------------------------------------------------------------------------
// Uptime and pending restart (registry, read only)
// ---------------------------------------------------------------------------

fn uptime_secs() -> u64 {
    (unsafe { GetTickCount64() }) / 1000
}

pub fn uptime_item(secs: u64) -> HealthItem {
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3600;
    let since = if days == 0 {
        plural(hours.max(1), "hour", "hours")
    } else {
        plural(days, "day", "days")
    };
    if days >= 14 {
        item(
            "uptime",
            "Uptime",
            "warning",
            format!("Running for {since} without a restart. A restart now and then finishes updates and frees memory that apps hold on to."),
        )
    } else {
        item("uptime", "Uptime", "good", format!("Restarted {since} ago."))
    }
}

struct RegKey(HKEY);

impl Drop for RegKey {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn open_hklm(path: &str) -> Option<RegKey> {
    let wide = HSTRING::from(path);
    let mut key = HKEY::default();
    let rc = unsafe {
        RegOpenKeyExW(HKEY_LOCAL_MACHINE, &wide, 0, KEY_QUERY_VALUE | KEY_WOW64_64KEY, &mut key)
    };
    (rc == ERROR_SUCCESS).then_some(RegKey(key))
}

/// Size in bytes of a value, None when it does not exist.
fn value_size(key: &RegKey, name: &str) -> Option<u32> {
    let wide = HSTRING::from(name);
    let mut size = 0u32;
    let rc = unsafe { RegQueryValueExW(key.0, &wide, None, None, None, Some(&mut size)) };
    (rc == ERROR_SUCCESS).then_some(size)
}

const CBS_REBOOT: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Component Based Servicing\RebootPending";
const WU_REBOOT: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\WindowsUpdate\Auto Update\RebootRequired";
const SESSION_MANAGER: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager";

fn restart_flags() -> (bool, bool, bool) {
    let cbs = open_hklm(CBS_REBOOT).is_some();
    let wu = open_hklm(WU_REBOOT).is_some();
    // REG_MULTI_SZ: an empty list is one or two NULs.
    let pfro = open_hklm(SESSION_MANAGER)
        .and_then(|k| value_size(&k, "PendingFileRenameOperations"))
        .is_some_and(|size| size > 4);
    (cbs, wu, pfro)
}

pub fn restart_reasons(cbs: bool, wu: bool, pfro: bool) -> Vec<String> {
    let mut out = Vec::new();
    if wu {
        out.push("Windows Update is waiting for a restart to finish installing.".to_string());
    }
    if cbs {
        out.push("Windows components were updated and need a restart.".to_string());
    }
    if pfro {
        out.push("An app or update will replace some files at the next restart.".to_string());
    }
    out
}

pub fn restart_item(reasons: &[String]) -> HealthItem {
    if reasons.is_empty() {
        return item("restart", "Restart", "good", "No restart is waiting.");
    }
    let mut it = item(
        "restart",
        "Restart",
        "warning",
        "A restart is pending. Restart when it suits you; Orbit will not restart the PC.",
    );
    it.detail = Some(reasons.join(" "));
    it
}

// ---------------------------------------------------------------------------
// Windows Update (WUA COM, search only, cached, off the main thread)
// ---------------------------------------------------------------------------

/// A finished search is reused for this long before the report starts a new one.
const UPDATE_FRESH: Duration = Duration::from_secs(30 * 60);
/// Not installed, not hidden, software (drivers are 7e's job), not optional.
const UPDATE_CRITERIA: &str = "IsInstalled=0 and IsHidden=0 and Type='Software' and BrowseOnly=0";
const MAX_TITLES: usize = 10;

struct UpdateCache {
    status: UpdateStatus,
    running: bool,
    finished: Option<Instant>,
}

fn update_cache() -> &'static Mutex<UpdateCache> {
    static CACHE: OnceLock<Mutex<UpdateCache>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(UpdateCache { status: UpdateStatus::idle(), running: false, finished: None })
    })
}

pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) fn wua_error(e: &windows::core::Error) -> String {
    let msg = e.message().to_string();
    let msg = msg.trim().trim_end_matches('.');
    if msg.is_empty() {
        format!("Windows Update did not answer (0x{:08X}).", e.code().0)
    } else {
        format!("Windows Update did not answer: {msg}.")
    }
}

/// (last search, last install) as Windows records them.
fn update_dates() -> (Option<u64>, Option<u64>) {
    let Ok(au) = (unsafe { CoCreateInstance::<_, IAutomaticUpdates2>(&AutomaticUpdates, None, CLSCTX_ALL) })
    else {
        return (None, None);
    };
    let Ok(results) = (unsafe { au.Results() }) else {
        return (None, None);
    };
    let search = unsafe { results.LastSearchSuccessDate() }.ok().and_then(|v| variant_date(&v));
    let install = unsafe { results.LastInstallationSuccessDate() }.ok().and_then(|v| variant_date(&v));
    (search, install)
}

/// The "Upgrades" category (Windows feature updates).
const UPGRADES_CATEGORY: &str = "3689bdc8-b205-4af4-8d4a-a63924c5e9d5";

/// A feature upgrade or another optional offer Windows does not pick by itself.
fn is_upgrade(u: &IUpdate) -> bool {
    if unsafe { u.AutoSelectOnWebSites() }.is_ok_and(|b| !b.as_bool()) {
        return true;
    }
    let Ok(cats) = (unsafe { u.Categories() }) else { return false };
    let n = unsafe { cats.Count() }.unwrap_or(0);
    (0..n).any(|i| {
        unsafe { cats.get_Item(i).and_then(|c| c.CategoryID()) }
            .is_ok_and(|id| id.to_string().eq_ignore_ascii_case(UPGRADES_CATEGORY))
    })
}

fn search_updates(online: bool) -> Result<(u32, Vec<String>), String> {
    let session: IUpdateSession =
        unsafe { CoCreateInstance(&UpdateSession, None, CLSCTX_ALL) }.map_err(|e| wua_error(&e))?;
    unsafe {
        let _ = session.SetClientApplicationID(&BSTR::from("Orbit"));
        let searcher = session.CreateUpdateSearcher().map_err(|e| wua_error(&e))?;
        // Offline: Windows' own last scan, quick and no network. Online asks the server.
        searcher.SetOnline(VARIANT_BOOL::from(online)).map_err(|e| wua_error(&e))?;
        let result = searcher.Search(&BSTR::from(UPDATE_CRITERIA)).map_err(|e| wua_error(&e))?;
        let updates = result.Updates().map_err(|e| wua_error(&e))?;
        let count = updates.Count().map_err(|e| wua_error(&e))?.clamp(0, 500);
        // Feature upgrades ("Windows 11, version 25H2") are offers, not pending
        // updates; the same update can also be listed twice.
        let mut titles: Vec<String> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        let mut n = 0u32;
        for i in 0..count {
            let Ok(u) = updates.get_Item(i) else { continue };
            if is_upgrade(&u) {
                continue;
            }
            let title = u.Title().map(|t| t.to_string()).unwrap_or_default();
            if !title.is_empty() {
                if seen.contains(&title) {
                    continue;
                }
                seen.push(title.clone());
            }
            n += 1;
            if titles.len() < MAX_TITLES && !title.is_empty() {
                titles.push(title);
            }
        }
        Ok((n, titles))
    }
}

fn run_update_check(online: bool) {
    let outcome = {
        let _com = com_init();
        let result = search_updates(online);
        let (search, install) = update_dates();
        (result, search, install)
    };
    let mut cache = lock(update_cache());
    let (result, search, install) = outcome;
    let s = &mut cache.status;
    s.last_search = search.or(s.last_search);
    s.last_install = install.or(s.last_install);
    s.checked_at = Some(now_secs());
    s.online = online;
    match result {
        Ok((count, titles)) => {
            s.state = "ready".into();
            s.pending = Some(count);
            s.pending_titles = titles;
            s.error = None;
        }
        Err(e) => {
            s.state = "error".into();
            s.error = Some(e);
        }
    }
    cache.running = false;
    cache.finished = Some(Instant::now());
}

/// Starts a search when none ran yet, the last one is older than 30 minutes,
/// or `force` (then online). Returns the current status right away.
pub fn ensure_update_check(force: bool) -> UpdateStatus {
    let mut cache = lock(update_cache());
    let stale = cache.finished.map_or(true, |t| t.elapsed() >= UPDATE_FRESH);
    if !cache.running && (force || stale) {
        cache.running = true;
        cache.status.state = "checking".into();
        let online = force;
        let spawned = std::thread::Builder::new()
            .name("orbit-update-check".into())
            .spawn(move || run_update_check(online));
        if spawned.is_err() {
            cache.running = false;
            cache.status.state = "error".into();
            cache.status.error = Some("Could not start the Windows Update check.".into());
        }
    }
    cache.status.clone()
}

pub fn updates_item(u: &UpdateStatus, now: u64) -> HealthItem {
    let title = "Windows Update";
    let settings = || action("Open Windows Update", "settings", "ms-settings:windowsupdate");
    let mut it = match (u.state.as_str(), u.pending) {
        ("checking", None) | ("idle", _) => item("updates", title, "unknown", "Checking Windows Update. This can take a minute."),
        ("error", None) => {
            let mut it = item("updates", title, "unknown", u.error.clone().unwrap_or_else(|| "Windows Update did not answer.".into()));
            it.action = settings();
            return it;
        }
        (_, Some(n)) if n > 0 => {
            let mut it = item(
                "updates",
                title,
                "warning",
                format!("{} waiting to install.", plural(n as u64, "update is", "updates are")),
            );
            if !u.pending_titles.is_empty() {
                it.detail = Some(u.pending_titles.join("; "));
            }
            it.action = settings();
            it
        }
        _ => item("updates", title, "good", "No updates are waiting."),
    };
    // A last search long ago matters more than an empty list.
    if let Some(last) = u.last_search {
        let days = now.saturating_sub(last) / 86_400;
        let when = format!("Windows last checked for updates {}.", days_ago(last, now));
        if days >= 14 && it.status != "unknown" {
            it.status = "warning".into();
            it.text = format!("Windows has not checked for updates in {days} days.");
            it.action = settings();
        } else if it.status == "good" {
            it.text = format!("No updates are waiting. {when}");
        }
    }
    it
}

// ---------------------------------------------------------------------------
// Startup load and suggestions (7b)
// ---------------------------------------------------------------------------

/// Words that mark antivirus / security software, matched in lowercase
/// against the entry name, file path and signer. Never suggested.
const SECURITY_WORDS: &[&str] = &[
    "antivirus", "anti-virus", "antimalware", "anti-malware", "security", "defender",
    "malwarebytes", "mcafee", "norton", "symantec", "nortonlifelock", "gen digital", "avast",
    "avg technologies", "\\avg\\", "kaspersky", "eset", "bitdefender", "sophos", "trend micro",
    "webroot", "f-secure", "withsecure", "avira", "panda security", "comodo", "crowdstrike",
    "sentinelone", "sentinel labs", "zonealarm", "check point", "g data", "gdata", "emsisoft",
    "cylance", "carbon black", "totalav", "vipre", "quick heal", "bullguard", "hitmanpro",
    "zemana", "spybot", "adaware", "360 total", "qihoo", "firewall",
];

pub fn is_security_software(name: &str, path: Option<&str>, publisher: Option<&str>) -> bool {
    let hay = format!(
        "{} {} {}",
        name.to_lowercase(),
        path.unwrap_or("").to_lowercase(),
        publisher.unwrap_or("").to_lowercase()
    );
    SECURITY_WORDS.iter().any(|w| hay.contains(w))
}

/// Signed by Microsoft (not the WHQL driver signature third parties get) or
/// living in the Windows folder.
pub fn is_microsoft(path: Option<&str>, status: Option<&str>, publisher: Option<&str>) -> bool {
    if let Some(p) = path {
        let win = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let win = normalize_path(&win);
        let p = normalize_path(p);
        if p.starts_with(&format!("{win}\\")) {
            return true;
        }
    }
    status == Some("signed")
        && publisher.is_some_and(|p| {
            p.starts_with("Microsoft") && !p.contains("Hardware Compatibility Publisher")
        })
}

/// Startup cost as one number: 1.0 = the "high" threshold (1 s CPU or 3 MB read).
fn cost(cpu_ms: u64, disk_read_bytes: u64) -> f64 {
    cpu_ms as f64 / 1000.0 + disk_read_bytes as f64 / (3 * MB) as f64
}

/// Up to 3 enabled entries with medium or high measured impact, highest first,
/// skipping Microsoft and security software. `sig` gives (status, publisher) per path.
pub fn pick_suggestions(
    entries: &[StartupEntry],
    file: Option<&crate::impact::ImpactFile>,
    sig: &dyn Fn(&str) -> (Option<String>, Option<String>),
) -> Vec<StartupSuggestion> {
    let Some(file) = file else {
        return Vec::new();
    };
    let mut out: Vec<(f64, StartupSuggestion)> = Vec::new();
    for e in entries.iter().filter(|e| e.enabled) {
        let Some(path) = e.path.as_deref() else {
            continue;
        };
        let Some(r) = file.entries.get(&normalize_path(path)) else {
            continue;
        };
        let impact = crate::impact::classify(r.cpu_ms, r.disk_read_bytes);
        if impact == "low" {
            continue;
        }
        let (status, publisher) = sig(path);
        if is_microsoft(Some(path), status.as_deref(), publisher.as_deref())
            || is_security_software(&e.name, Some(path), publisher.as_deref())
        {
            continue;
        }
        // Orbit itself is not a suggestion.
        if e.name.eq_ignore_ascii_case("orbit") {
            continue;
        }
        out.push((
            cost(r.cpu_ms, r.disk_read_bytes),
            StartupSuggestion {
                id: e.id.clone(),
                name: e.name.clone(),
                path: e.path.clone(),
                impact: impact.into(),
                cpu_ms: r.cpu_ms,
                disk_read_bytes: r.disk_read_bytes,
                publisher,
                needs_admin: e.needs_admin,
                measured_at: file.measured_at,
            },
        ));
    }
    out.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.1.name.cmp(&b.1.name)));
    out.into_iter().take(3).map(|(_, s)| s).collect()
}

fn suggestions_blocking() -> (Vec<StartupEntry>, Option<crate::impact::ImpactFile>, Vec<StartupSuggestion>) {
    let mut entries = crate::startup::collect_startup();
    let file = crate::impact::load();
    crate::impact::apply(&mut entries, file.as_ref());
    let paths: Vec<String> = entries
        .iter()
        .filter(|e| e.enabled && e.impact.as_deref().is_some_and(|i| i != "low"))
        .filter_map(|e| e.path.clone())
        .collect();
    let sigs = if paths.is_empty() { HashMap::new() } else { crate::security::signatures_for(&paths) };
    let lookup = |p: &str| {
        sigs.get(p)
            .map(|s| (Some(s.status.clone()), s.publisher.clone()))
            .unwrap_or((None, None))
    };
    let picks = pick_suggestions(&entries, file.as_ref(), &lookup);
    (entries, file, picks)
}

pub fn startup_item(entries: &[StartupEntry], measured: bool, suggestions: usize) -> HealthItem {
    let enabled: Vec<&StartupEntry> = entries.iter().filter(|e| e.enabled).collect();
    let high = enabled.iter().filter(|e| e.impact.as_deref() == Some("high")).count() as u64;
    let n = enabled.len() as u64;
    let title = "Startup load";
    let apps = plural(n, "app starts", "apps start");
    let mut it = if !measured {
        item(
            "startup",
            title,
            "unknown",
            format!("{apps} with Windows. Orbit has not measured how much they slow sign-in yet."),
        )
    } else if high > 0 && suggestions > 0 {
        item(
            "startup",
            title,
            "warning",
            format!("{apps} with Windows. {} a high impact on sign-in.", plural(high, "has", "have")),
        )
    } else if high > 0 {
        item(
            "startup",
            title,
            "good",
            format!("{apps} with Windows. The heaviest are Windows or security apps, which Orbit leaves alone."),
        )
    } else {
        item("startup", title, "good", format!("{apps} with Windows. None of them slows sign-in much."))
    };
    // Something worth turning off: Optimize > Debloat explains each app first.
    it.action = if it.status == "warning" {
        action("Review in Debloat", "optimize", "debloat")
    } else {
        action("Go to Startup", "view", "startup")
    };
    it
}

// ---------------------------------------------------------------------------
// Security scan
// ---------------------------------------------------------------------------

pub fn security_item(findings: Result<&[crate::security::SecurityFinding], &str>) -> HealthItem {
    let title = "Security scan";
    let mut it = match findings {
        Err(e) => item("security", title, "unknown", e.to_string()),
        Ok(list) => {
            let high = list.iter().filter(|f| f.severity == "high").count() as u64;
            let medium = list.iter().filter(|f| f.severity == "medium").count() as u64;
            if high > 0 {
                item(
                    "security",
                    title,
                    "problem",
                    format!("{} need a close look ({high} high, {medium} medium).", plural(high + medium, "program may", "programs may")),
                )
            } else if medium > 0 {
                item(
                    "security",
                    title,
                    "warning",
                    format!("{} worth a look: unsigned or running from an unusual folder.", plural(medium, "program is", "programs are")),
                )
            } else {
                item("security", title, "good", "Nothing suspicious among the running programs.")
            }
        }
    };
    it.action = action("Go to Security", "view", "security");
    it
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

fn join_error(e: impl std::fmt::Display) -> String {
    format!("Could not build the health report: {e}")
}

/// The whole report. Slow parts: WMI (a second or two), the security scan
/// (about 2 s the first time, cached after). Windows Update comes from the
/// cache; a search starts in the background when it is stale.
#[tauri::command]
pub async fn get_health_report(state: State<'_, AppState>) -> Result<HealthReport, String> {
    let is_admin = unsafe { windows::Win32::UI::Shell::IsUserAnAdmin() }.as_bool();
    let update = ensure_update_check(false);
    let local = tauri::async_runtime::spawn_blocking(move || read_local(is_admin));
    let scan = crate::security::security_scan(state).await;
    let local = local.await.map_err(join_error)?;
    Ok(assemble(is_admin, update, local, scan.as_deref().map_err(|e| e.as_str())))
}

/// Everything the report reads on this PC, apart from the security scan and Windows Update.
struct Local {
    drives: Vec<DriveSpace>,
    disks: Vec<PhysicalDisk>,
    disk_note: Option<String>,
    reasons: Vec<String>,
    entries: Vec<StartupEntry>,
    measured: bool,
    picks: usize,
    /// Devices with a problem code (drivers.rs), as (name, reason).
    devices: Result<Vec<(String, String)>, String>,
}

fn read_local(is_admin: bool) -> Local {
    let drives = read_drives();
    let (disks, disk_note) = read_physical_disks(is_admin);
    let (cbs, wu, pfro) = restart_flags();
    let (entries, file, picks) = suggestions_blocking();
    Local {
        drives,
        disks,
        disk_note,
        reasons: restart_reasons(cbs, wu, pfro),
        entries,
        measured: file.is_some(),
        picks: picks.len(),
        devices: crate::drivers::problem_devices(),
    }
}

fn assemble(
    is_admin: bool,
    update: UpdateStatus,
    local: Local,
    scan_ref: Result<&[crate::security::SecurityFinding], &str>,
) -> HealthReport {
    let Local { drives, disks, disk_note, reasons, entries, measured, picks, devices } = local;
    let now = now_secs();
    let uptime = uptime_secs();
    let items = vec![
        disks_item(&disks, disk_note.as_deref(), is_admin),
        space_item(&drives),
        restart_item(&reasons),
        updates_item(&update, now),
        crate::drivers::devices_item(devices.as_deref().map_err(|e| e.as_str())),
        uptime_item(uptime),
        startup_item(&entries, measured, picks),
        security_item(scan_ref),
    ];
    HealthReport {
        generated_at: now,
        is_admin,
        items,
        drives,
        disks,
        disk_note,
        update,
        uptime_secs: uptime,
        restart_reasons: reasons,
    }
}

/// The cached Windows Update status; `refresh: true` starts an online search
/// now (unless one is running). Plain value, never an error.
#[tauri::command]
pub fn get_update_status(refresh: Option<bool>) -> UpdateStatus {
    let mut s = ensure_update_check(refresh.unwrap_or(false));
    // Re-read in case the thread finished between the two locks.
    if s.state == "checking" {
        s = lock(update_cache()).status.clone();
    }
    s
}

/// Up to 3 enabled startup apps worth turning off (7b). Empty when nothing
/// was measured or nothing qualifies.
#[tauri::command]
pub async fn get_startup_suggestions() -> Result<Vec<StartupSuggestion>, String> {
    tauri::async_runtime::spawn_blocking(|| suggestions_blocking().2)
        .await
        .map_err(|e| format!("Could not read startup entries: {e}"))
}

/// Only `ms-settings:` pages with plain names ("ms-settings:windowsupdate").
pub fn valid_settings_page(page: &str) -> bool {
    page.strip_prefix("ms-settings:").is_some_and(|rest| {
        !rest.is_empty()
            && rest.len() <= 64
            && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

/// Opens a Windows Settings page.
#[tauri::command]
pub fn open_settings_page(page: String) -> Result<(), String> {
    let page = page.trim().to_ascii_lowercase();
    if !valid_settings_page(&page) {
        return Err("Orbit can only open Windows Settings pages.".into());
    }
    let wide = HSTRING::from(page.as_str());
    let r = unsafe {
        ShellExecuteW(HWND::default(), &HSTRING::from("open"), &wide, PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL)
    };
    if (r.0 as isize) <= 32 {
        Err("Could not open Windows Settings.".into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::{ImpactFile, ImpactRecord};

    fn drive(mount: &str, total_gb: u64, free_gb_x10: u64, removable: bool) -> DriveSpace {
        let total = total_gb * GB;
        let available = free_gb_x10 * GB / 10;
        DriveSpace {
            mount: mount.into(),
            name: String::new(),
            file_system: "NTFS".into(),
            total,
            available,
            removable,
            status: space_status(total, available).into(),
        }
    }

    #[test]
    fn space_thresholds() {
        assert_eq!(space_status(500 * GB, 200 * GB), "good");
        assert_eq!(space_status(1000 * GB, 60 * GB), "good"); // 6%, but 60 GB
        assert_eq!(space_status(500 * GB, 20 * GB), "warning"); // under 25 GB
        assert_eq!(space_status(500 * GB, 4 * GB), "problem"); // under 5 GB
        assert_eq!(space_status(64 * GB, 6 * GB), "warning"); // under 6.4 GB
        assert_eq!(space_status(64 * GB, GB), "problem"); // under 1.9 GB
        assert_eq!(space_status(32 * GB, 6 * GB), "good");
        assert_eq!(space_status(0, 0), "good");
    }

    #[test]
    fn space_item_names_the_full_drive_and_skips_removable() {
        let drives = vec![drive("C:\\", 500, 30, false), drive("E:\\", 1000, 6000, false), drive("F:\\", 16, 1, true)];
        let it = space_item(&drives);
        assert_eq!(it.status, "problem");
        assert!(it.text.starts_with("C: (3.0 GB free) is almost full"), "{}", it.text);
        assert_eq!(it.action.as_ref().map(|a| a.target.as_str()), Some("cleanup"));
        let ok = space_item(&drives[1..2]);
        assert_eq!(ok.status, "good");
        assert_eq!(ok.text, "E: has 600 GB free.");
        assert!(ok.action.is_none());
        assert_eq!(space_item(&drives[2..]).status, "unknown");
    }

    #[test]
    fn disk_statuses() {
        let mut d = PhysicalDisk { name: "Samsung SSD".into(), health: "healthy".into(), ..Default::default() };
        assert_eq!(disk_status(&d), "good");
        d.wear_pct = Some(75);
        assert_eq!(disk_status(&d), "warning");
        d.wear_pct = Some(95);
        assert_eq!(disk_status(&d), "problem");
        d.wear_pct = None;
        d.problems = vec![operational_problem(5).unwrap()];
        assert_eq!(disk_status(&d), "problem");
        d.problems = vec![operational_problem(3).unwrap()];
        assert_eq!(disk_status(&d), "warning");
        d.problems.clear();
        d.health = "unhealthy".into();
        assert_eq!(disk_status(&d), "problem");
        d.health = "unknown".into();
        assert_eq!(disk_status(&d), "unknown");
        assert_eq!(operational_problem(2), None);
        assert_eq!(operational_problem(99).as_deref(), Some("Status code 99"));
    }

    #[test]
    fn disks_item_texts() {
        let good = PhysicalDisk { name: "A".into(), health: "healthy".into(), ..Default::default() };
        let bad = PhysicalDisk { name: "B".into(), health: "unhealthy".into(), ..Default::default() };
        let it = disks_item(&[good.clone(), bad], None, true);
        assert_eq!(it.status, "problem");
        assert_eq!(it.text, "B reports a problem. Back up your files soon.");
        let it = disks_item(&[good.clone()], Some("Restart as administrator to see wear."), false);
        assert_eq!((it.status.as_str(), it.text.as_str()), ("good", "Your disk reports healthy."));
        assert_eq!(it.action.map(|a| a.kind), Some("admin".to_string()));
        let it = disks_item(&[], None, true);
        assert_eq!(it.status, "unknown");
        assert!(it.action.is_none());
    }

    #[test]
    fn uptime_texts() {
        assert_eq!(uptime_item(600).text, "Restarted 1 hour ago.");
        assert_eq!(uptime_item(5 * 3600).text, "Restarted 5 hours ago.");
        assert_eq!(uptime_item(3 * 86_400).status, "good");
        let long = uptime_item(20 * 86_400);
        assert_eq!(long.status, "warning");
        assert!(long.text.starts_with("Running for 20 days"));
    }

    #[test]
    fn restart_item_lists_reasons_and_never_acts() {
        assert_eq!(restart_item(&restart_reasons(false, false, false)).status, "good");
        let it = restart_item(&restart_reasons(true, true, false));
        assert_eq!(it.status, "warning");
        assert!(it.action.is_none());
        assert!(it.detail.unwrap().starts_with("Windows Update is waiting"));
        assert_eq!(restart_reasons(false, false, true).len(), 1);
    }

    #[test]
    fn reads_restart_flags_without_panicking() {
        // Read-only registry probes; whatever the PC's state, no error.
        let (a, b, c) = restart_flags();
        let _ = restart_reasons(a, b, c);
        assert!(uptime_secs() > 0);
    }

    #[test]
    fn update_item_states() {
        let now = 1_800_000_000;
        let mut u = UpdateStatus::idle();
        assert_eq!(updates_item(&u, now).status, "unknown");
        u.state = "ready".into();
        u.pending = Some(0);
        u.last_search = Some(now - 86_400);
        let it = updates_item(&u, now);
        assert_eq!(it.status, "good");
        assert_eq!(it.text, "No updates are waiting. Windows last checked for updates yesterday.");
        u.pending = Some(2);
        u.pending_titles = vec!["KB1".into(), "KB2".into()];
        let it = updates_item(&u, now);
        assert_eq!((it.status.as_str(), it.text.as_str()), ("warning", "2 updates are waiting to install."));
        assert_eq!(it.action.unwrap().target, "ms-settings:windowsupdate");
        u.pending = Some(0);
        u.last_search = Some(now - 30 * 86_400);
        let it = updates_item(&u, now);
        assert_eq!((it.status.as_str(), it.text.as_str()), ("warning", "Windows has not checked for updates in 30 days."));
        u.state = "error".into();
        u.pending = None;
        u.error = Some("Windows Update did not answer.".into());
        assert_eq!(updates_item(&u, now).status, "unknown");
    }

    #[test]
    fn ole_dates() {
        assert_eq!(ole_date_to_unix(25_569.0), None); // 1970-01-01 exactly
        assert_eq!(ole_date_to_unix(25_570.0), Some(86_400));
        assert_eq!(ole_date_to_unix(0.0), None);
        assert_eq!(ole_date_to_unix(45_658.5), Some(1_735_732_800)); // 2025-01-01 12:00 UTC
    }

    #[test]
    fn settings_pages() {
        assert!(valid_settings_page("ms-settings:windowsupdate"));
        assert!(valid_settings_page("ms-settings:windowsupdate-optionalupdates"));
        assert!(!valid_settings_page("ms-settings:"));
        assert!(!valid_settings_page("https://example.com"));
        assert!(!valid_settings_page("ms-settings:x&calc"));
        assert!(!valid_settings_page("ms-settings:a b"));
        assert!(open_settings_page("file:///c:/windows".into()).is_err());
    }

    #[test]
    fn security_and_microsoft_detection() {
        assert!(is_security_software("Malwarebytes Tray", None, None));
        assert!(is_security_software("Tray", Some(r"C:\Program Files\Bitdefender\x.exe"), None));
        assert!(is_security_software("x", None, Some("NortonLifeLock Inc.")));
        assert!(is_security_software("SecurityHealth", None, None));
        assert!(!is_security_software("Discord", Some(r"C:\Users\a\AppData\Local\Discord\Update.exe"), Some("Discord Inc.")));
        assert!(is_microsoft(Some(r"C:\Windows\System32\SecurityHealthSystray.exe"), None, None));
        assert!(is_microsoft(Some(r"C:\Users\a\OneDrive.exe"), Some("signed"), Some("Microsoft Corporation")));
        assert!(!is_microsoft(Some(r"C:\Drivers\x.exe"), Some("signed"), Some("Microsoft Windows Hardware Compatibility Publisher")));
        assert!(!is_microsoft(Some(r"C:\Apps\x.exe"), Some("unsigned"), Some("Microsoft Corporation")));
    }

    fn entry(name: &str, path: &str, enabled: bool) -> StartupEntry {
        StartupEntry {
            id: format!("HKCU\\Run\\{name}"),
            name: name.into(),
            command: path.into(),
            location: "HKCU Run".into(),
            enabled,
            path: Some(path.into()),
            needs_admin: false,
            impact: None,
            impact_measured_at: None,
            packaged_state: None,
        }
    }

    #[test]
    fn suggestions_pick_the_heaviest_three_safe_apps() {
        let list = vec![
            entry("Discord", r"C:\Apps\Discord.exe", true),
            entry("Spotify", r"C:\Apps\Spotify.exe", true),
            entry("Steam", r"C:\Apps\Steam.exe", false), // disabled
            entry("OneDrive", r"C:\Apps\OneDrive.exe", true), // Microsoft-signed
            entry("Avast", r"C:\Apps\AvastUI.exe", true), // security
            entry("Tiny", r"C:\Apps\Tiny.exe", true), // low
            entry("OBS", r"C:\Apps\obs64.exe", true),
            entry("Adobe", r"C:\Apps\CCX.exe", true),
        ];
        let rec = |cpu, disk| ImpactRecord { cpu_ms: cpu, disk_read_bytes: disk, impact: String::new() };
        let mut map = HashMap::new();
        map.insert(r"c:\apps\discord.exe".to_string(), rec(4000, 0));
        map.insert(r"c:\apps\spotify.exe".to_string(), rec(400, 0));
        map.insert(r"c:\apps\steam.exe".to_string(), rec(9000, 0));
        map.insert(r"c:\apps\onedrive.exe".to_string(), rec(8000, 0));
        map.insert(r"c:\apps\avastui.exe".to_string(), rec(8000, 0));
        map.insert(r"c:\apps\tiny.exe".to_string(), rec(10, 0));
        map.insert(r"c:\apps\obs64.exe".to_string(), rec(500, 6 * MB));
        map.insert(r"c:\apps\ccx.exe".to_string(), rec(1500, 0));
        let file = ImpactFile { measured_at: 7, entries: map };
        let sig = |p: &str| {
            if p.contains("OneDrive") {
                (Some("signed".to_string()), Some("Microsoft Corporation".to_string()))
            } else {
                (Some("signed".to_string()), Some("Some Vendor".to_string()))
            }
        };
        let picks = pick_suggestions(&list, Some(&file), &sig);
        let names: Vec<&str> = picks.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Discord", "OBS", "Adobe"]);
        assert_eq!(picks[0].impact, "high");
        assert_eq!(picks[0].measured_at, 7);
        assert!(pick_suggestions(&list, None, &sig).is_empty());
    }

    #[test]
    fn startup_and_security_items() {
        let mut list = vec![entry("A", r"C:\a.exe", true), entry("B", r"C:\b.exe", true)];
        assert_eq!(startup_item(&list, false, 0).status, "unknown");
        list[0].impact = Some("high".into());
        let it = startup_item(&list, true, 1);
        assert_eq!((it.status.as_str(), it.text.as_str()), ("warning", "2 apps start with Windows. 1 has a high impact on sign-in."));
        assert_eq!(it.action.as_ref().map(|a| (a.kind.as_str(), a.target.as_str())), Some(("optimize", "debloat")));
        assert_eq!(startup_item(&list, true, 0).status, "good");
        assert_eq!(startup_item(&list, true, 0).action.unwrap().target, "startup");
        assert!(startup_item(&list[1..], true, 0).text.starts_with("1 app starts with Windows."));
        let finding = |sev: &str| crate::security::SecurityFinding {
            pid: 1,
            name: "x.exe".into(),
            exe: None,
            severity: sev.into(),
            reasons: vec![],
            signature_status: "unsigned".into(),
            publisher: None,
        };
        let all = [finding("high"), finding("medium"), finding("info")];
        assert_eq!(security_item(Ok(&all)).status, "problem");
        assert_eq!(security_item(Ok(&all[1..])).status, "warning");
        assert_eq!(security_item(Ok(&all[2..])).status, "good");
        assert_eq!(security_item(Err("No process list yet.")).status, "unknown");
    }

    #[test]
    fn reads_drives_and_disks() {
        // Live, read only: free space always; WMI may refuse unelevated.
        let drives = read_drives();
        assert!(!drives.is_empty());
        let (disks, note) = read_physical_disks(false);
        assert!(!disks.is_empty() || note.is_some());
    }

    /// Manual: `cargo test health::tests::live -- --ignored --nocapture`
    /// prints what this PC reports (Windows Update search from Windows' cache).
    #[test]
    #[ignore]
    fn live() {
        println!("drives: {:#?}", read_drives());
        println!("disks: {:#?}", read_physical_disks(false));
        println!("restart flags: {:?}", restart_flags());
        let t = Instant::now();
        run_update_check(false);
        println!("update ({:?}): {:#?}", t.elapsed(), lock(update_cache()).status);
        println!("suggestions: {:#?}", suggestions_blocking().2);
        // The whole report as the UI gets it (the security scan needs the running
        // app's process list, so it shows as not run here).
        let report = assemble(false, lock(update_cache()).status.clone(), read_local(false), Err("Not run in this test."));
        println!("report: {}", serde_json::to_string_pretty(&report).unwrap());
        assert_eq!(report.items.len(), 8);
    }
}
