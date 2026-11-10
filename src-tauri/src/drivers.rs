//! Driver check (Optimize > Drivers, 0.5, feature 7e).
//!
//! Lists every present device with its driver (SetupAPI device properties,
//! CfgMgr32 problem codes, SetupVerifyInfFile for the signature) and flags
//! devices with a problem code, old third-party drivers and unsigned drivers.
//!
//! Updates come from two places only:
//! - drivers Windows Update offers for this PC (Windows Update Agent, search
//!   by default; installing one goes through WUA's own downloader and
//!   installer, needs admin and only happens when the user confirms it);
//! - a fixed table of the makers' official https support pages, opened with
//!   `open_url`.
//! Orbit never downloads a driver from anywhere else, never installs one
//! silently and never updates anything in the background.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use windows::core::{Interface, BSTR, GUID, HSTRING, PCWSTR};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_DevNode_Status, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo,
    SetupDiGetClassDescriptionW, SetupDiGetClassDevsW, SetupDiGetDevicePropertyW,
    SetupVerifyInfFileW, CM_DEVNODE_STATUS_FLAGS, CM_PROB, CR_SUCCESS, DIGCF_ALLCLASSES,
    DIGCF_PRESENT, DN_HAS_PROBLEM, DN_NO_SHOW_IN_DM, HDEVINFO, SIGNERSCORE_AUTHENTICODE,
    SIGNERSCORE_INBOX, SIGNERSCORE_LOGO_PREMIUM, SIGNERSCORE_LOGO_STANDARD, SIGNERSCORE_WHQL,
    SP_DEVINFO_DATA, SP_INF_SIGNER_INFO_V2_W,
};
use windows::Win32::Devices::Properties::{
    DEVPKEY_Device_Class, DEVPKEY_Device_CompatibleIds, DEVPKEY_Device_DeviceDesc,
    DEVPKEY_Device_DriverDate, DEVPKEY_Device_DriverInfPath, DEVPKEY_Device_DriverProvider,
    DEVPKEY_Device_DriverVersion, DEVPKEY_Device_FriendlyName, DEVPKEY_Device_HardwareIds,
    DEVPKEY_Device_InstanceId, DEVPKEY_Device_Manufacturer, DEVPROPKEY, DEVPROPTYPE,
    DEVPROP_TYPE_FILETIME, DEVPROP_TYPE_STRING, DEVPROP_TYPE_STRING_LIST,
};
use windows::Win32::Foundation::{GetLastError, HWND, VARIANT_BOOL};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
use windows::Win32::System::UpdateAgent::{
    IUpdate, IUpdate3, IUpdateCollection, IUpdateSession, IWindowsDriverUpdate, UpdateCollection,
    UpdateSession,
};

use crate::health::{com_init, lock, now_secs, ole_date_to_unix, wua_error, HealthItem};

// ---------------------------------------------------------------------------
// Shapes (docs/CONTRACT.md "v0.5", Drivers)
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DriverList {
    /// Unix seconds.
    pub scanned_at: u64,
    pub is_admin: bool,
    /// Third-party drivers at least this old are flagged.
    pub old_years: u32,
    /// Worst first (problem, unsigned, old, off, ok), then by class and name.
    pub devices: Vec<Device>,
    /// Devices Device Manager hides too (no problem, "don't show" flag).
    pub hidden: u32,
    pub counts: Counts,
    pub system: SystemInfo,
    pub updates: DriverUpdateStatus,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Counts {
    pub total: u32,
    pub problem: u32,
    pub unsigned: u32,
    pub old: u32,
    pub off: u32,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    /// Device instance id ("PCI\\VEN_10DE&...").
    pub id: String,
    pub name: String,
    /// Device Manager's class name ("Display adapters"); "Other devices" when none.
    pub class: String,
    pub manufacturer: Option<String>,
    pub provider: Option<String>,
    pub version: Option<String>,
    /// Driver date, Unix seconds.
    pub date: Option<u64>,
    /// INF file name ("oem12.inf"); None when no driver is installed.
    pub inf: Option<String>,
    /// The INF came with Windows (not an oemNN.inf).
    pub inbox: bool,
    /// None when Orbit could not tell.
    pub signed: Option<bool>,
    pub signer: Option<String>,
    /// "windows" | "whql" | "authenticode" | "signed" | "unsigned" | null
    pub signature: Option<String>,
    pub problem: Option<u32>,
    pub problem_text: Option<String>,
    /// "problem" | "unsigned" | "old" | "off" | "ok": the worst flag.
    pub status: String,
    /// Every flag that applies, worst first.
    pub flags: Vec<String>,
    /// The maker's official driver page, from the provider or manufacturer.
    pub vendor: Option<VendorLink>,
    /// Windows Update offers a driver for this device (DriverOffer.id).
    pub update_id: Option<String>,
    #[serde(skip)]
    pub hardware_ids: Vec<String>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VendorLink {
    pub name: String,
    pub url: String,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SystemInfo {
    /// SMBIOS system maker and model (registry copy under HARDWARE\DESCRIPTION).
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    /// Support page of the PC maker (Dell, HP, Lenovo, ASUS, Acer, MSI).
    pub vendor: Option<VendorLink>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DriverOffer {
    /// WUA UpdateID (GUID).
    pub id: String,
    pub title: String,
    pub class: Option<String>,
    pub provider: Option<String>,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub hardware_id: Option<String>,
    /// Driver date, Unix seconds.
    pub date: Option<u64>,
    /// Listed under "Optional updates" in Settings (WUA BrowseOnly).
    pub optional: bool,
    pub downloaded: bool,
    /// The present device it is for, matched by hardware id.
    pub device_id: Option<String>,
    pub device_name: Option<String>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DriverUpdateStatus {
    /// "idle" | "checking" | "ready" | "error"
    pub state: String,
    pub offers: Vec<DriverOffer>,
    pub checked_at: Option<u64>,
    /// The last finished search asked the update server (true) or Windows' cache.
    pub online: bool,
    pub error: Option<String>,
    /// UpdateID being installed right now.
    pub installing: Option<String>,
    pub last_install: Option<InstallResult>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InstallResult {
    pub id: String,
    pub title: String,
    pub ok: bool,
    /// "succeeded" | "succeededWithErrors" | "failed" | "aborted"
    pub result: String,
    pub restart_required: bool,
    pub message: String,
    pub at: u64,
}

impl DriverUpdateStatus {
    fn idle() -> Self {
        Self {
            state: "idle".into(),
            offers: Vec::new(),
            checked_at: None,
            online: false,
            error: None,
            installing: None,
            last_install: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Problem codes (CM_PROB_*) in plain words
// ---------------------------------------------------------------------------

/// What Device Manager's yellow "!" means, in one or two plain sentences.
pub fn problem_text(code: u32) -> String {
    match code {
        1 => "This device is not set up correctly. Its driver may be missing or wrong.".into(),
        10 => "This device cannot start. Updating its driver often fixes this.".into(),
        12 => "This device cannot find enough free resources to work. Another device may be using them.".into(),
        14 => "This device will not work until you restart the PC.".into(),
        18 => "The driver for this device needs to be installed again.".into(),
        19 => "Windows cannot start this device because its settings in the registry are damaged or incomplete.".into(),
        21 => "Windows is removing this device.".into(),
        22 => "This device is turned off in Device Manager.".into(),
        24 => "This device is not present, not working, or not all of its drivers are installed.".into(),
        28 => "No driver is installed for this device.".into(),
        31 => "Windows cannot load the driver this device needs, so it is not working.".into(),
        39 => "Windows cannot load the driver for this device. The driver may be damaged or missing.".into(),
        43 => "Windows stopped this device because it reported problems.".into(),
        45 => "This device is not connected to the PC right now.".into(),
        52 => "Windows cannot check the digital signature of this device's driver, so it was not loaded.".into(),
        n => format!("This device reports a problem (code {n})."),
    }
}

/// Turned off by the user (22) or simply unplugged (45): shown, not counted as a problem.
pub fn is_soft_problem(code: u32) -> bool {
    matches!(code, 22 | 45)
}

// ---------------------------------------------------------------------------
// Makers' official driver pages (fixed, https, official domains only)
// ---------------------------------------------------------------------------

struct Maker {
    name: &'static str,
    url: &'static str,
    /// Lowercase words, matched on word boundaries.
    words: &'static [&'static str],
    /// A PC maker (matched against the system manufacturer too).
    pc: bool,
}

const MAKERS: &[Maker] = &[
    Maker { name: "NVIDIA", url: "https://www.nvidia.com/en-us/drivers/", words: &["nvidia"], pc: false },
    Maker { name: "AMD", url: "https://www.amd.com/en/support/download/drivers.html", words: &["amd", "advanced micro devices", "ati technologies"], pc: false },
    Maker { name: "Intel", url: "https://www.intel.com/content/www/us/en/support/detect.html", words: &["intel"], pc: false },
    Maker { name: "Realtek", url: "https://www.realtek.com/Download/Overview", words: &["realtek"], pc: false },
    Maker { name: "Qualcomm", url: "https://www.qualcomm.com/support", words: &["qualcomm"], pc: false },
    Maker { name: "MediaTek", url: "https://www.mediatek.com/", words: &["mediatek"], pc: false },
    Maker { name: "Broadcom", url: "https://www.broadcom.com/support/download-search", words: &["broadcom"], pc: false },
    Maker { name: "Logitech", url: "https://support.logi.com/", words: &["logitech"], pc: false },
    Maker { name: "Dell", url: "https://www.dell.com/support/home/", words: &["dell"], pc: true },
    Maker { name: "HP", url: "https://support.hp.com/drivers", words: &["hp", "hewlett packard"], pc: true },
    Maker { name: "Lenovo", url: "https://pcsupport.lenovo.com/", words: &["lenovo"], pc: true },
    Maker { name: "ASUS", url: "https://www.asus.com/support/download-center/", words: &["asus", "asustek"], pc: true },
    Maker { name: "Acer", url: "https://www.acer.com/us-en/support", words: &["acer"], pc: true },
    Maker { name: "MSI", url: "https://www.msi.com/support/download", words: &["msi", "micro star"], pc: true },
];

/// " intel r corporation " for "Intel(R) Corporation".
fn word_form(s: &str) -> String {
    let mut out = String::from(" ");
    let mut gap = false;
    for c in s.chars() {
        if c.is_alphanumeric() {
            if gap && !out.ends_with(' ') {
                out.push(' ');
            }
            out.extend(c.to_lowercase());
            gap = false;
        } else {
            gap = true;
        }
    }
    out.push(' ');
    out
}

fn find_maker(s: &str, pc_only: bool) -> Option<VendorLink> {
    let hay = word_form(s);
    MAKERS
        .iter()
        .filter(|m| !pc_only || m.pc)
        .find(|m| m.words.iter().any(|w| hay.contains(&format!(" {w} "))))
        .map(|m| VendorLink { name: m.name.into(), url: m.url.into() })
}

/// The maker page for a driver: its provider first, then the device maker.
pub fn vendor_for(provider: Option<&str>, manufacturer: Option<&str>) -> Option<VendorLink> {
    provider
        .and_then(|p| find_maker(p, false))
        .or_else(|| manufacturer.and_then(|m| find_maker(m, false)))
}

/// SMBIOS placeholders that name no one.
pub(crate) fn is_placeholder(s: &str) -> bool {
    let l = s.trim().to_ascii_lowercase();
    l.is_empty()
        || l.contains("system manufacturer")
        || l.contains("to be filled")
        || l.contains("default string")
        || l == "oem"
        || l == "not applicable"
}

/// The PC maker's support page: the system manufacturer, else the board maker
/// (self-built PCs), only for the PC makers in the table.
pub fn pc_vendor(system: Option<&str>, board: Option<&str>) -> Option<VendorLink> {
    [system, board]
        .into_iter()
        .flatten()
        .filter(|s| !is_placeholder(s))
        .find_map(|s| find_maker(s, true))
}

// ---------------------------------------------------------------------------
// Flags
// ---------------------------------------------------------------------------

pub const OLD_YEARS: u32 = 5;
const YEAR_SECS: f64 = 365.25 * 86_400.0;

/// Older than `OLD_YEARS`. Drivers that came with Windows are serviced by
/// Windows itself (and many carry 2006 dates), so age alone never flags them.
pub fn is_old(date: Option<u64>, inbox: bool, now: u64) -> bool {
    match date {
        Some(d) if !inbox && d > 0 => (now.saturating_sub(d) as f64) / YEAR_SECS >= OLD_YEARS as f64,
        _ => false,
    }
}

const RANK: &[&str] = &["problem", "unsigned", "old", "off", "ok"];

fn rank(status: &str) -> usize {
    RANK.iter().position(|s| *s == status).unwrap_or(RANK.len())
}

/// Flags worst first, given what the scan found.
pub fn flags_for(problem: Option<u32>, signed: Option<bool>, old: bool) -> Vec<String> {
    let mut f = Vec::new();
    match problem {
        Some(c) if is_soft_problem(c) => {}
        Some(_) => f.push("problem".to_string()),
        None => {}
    }
    if signed == Some(false) {
        f.push("unsigned".into());
    }
    if old {
        f.push("old".into());
    }
    if problem.is_some_and(is_soft_problem) {
        f.push("off".into());
    }
    f
}

/// Worst first, then class, then name.
pub fn sort_devices(devices: &mut [Device]) {
    devices.sort_by(|a, b| {
        rank(&a.status)
            .cmp(&rank(&b.status))
            .then_with(|| a.class.to_lowercase().cmp(&b.class.to_lowercase()))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

pub fn counts(devices: &[Device]) -> Counts {
    let mut c = Counts { total: devices.len() as u32, ..Default::default() };
    for d in devices {
        for f in &d.flags {
            match f.as_str() {
                "problem" => c.problem += 1,
                "unsigned" => c.unsigned += 1,
                "old" => c.old += 1,
                "off" => c.off += 1,
                _ => {}
            }
        }
    }
    c
}

// ---------------------------------------------------------------------------
// Signatures (SetupVerifyInfFile, cached per INF)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Signature {
    pub signed: Option<bool>,
    pub signer: Option<String>,
    pub kind: Option<String>,
}

const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_PATH_NOT_FOUND: u32 = 3;
/// SetupAPI: signed with Authenticode by a publisher the user trusts.
const ERROR_AUTHENTICODE_TRUSTED_PUBLISHER: u32 = 0xE000_0241;

/// Turns SetupVerifyInfFile's answer into signed / signer / kind.
pub fn classify_signature(ok: bool, err: u32, score: u32, signer: &str) -> Signature {
    let signer = Some(signer.trim().to_string()).filter(|s| !s.is_empty());
    if !ok && (err == ERROR_FILE_NOT_FOUND || err == ERROR_PATH_NOT_FOUND) {
        return Signature { signed: None, signer: None, kind: None };
    }
    if ok || err == ERROR_AUTHENTICODE_TRUSTED_PUBLISHER {
        let kind = match score {
            SIGNERSCORE_INBOX => "windows",
            SIGNERSCORE_WHQL | SIGNERSCORE_LOGO_STANDARD | SIGNERSCORE_LOGO_PREMIUM => "whql",
            SIGNERSCORE_AUTHENTICODE => "authenticode",
            _ if err == ERROR_AUTHENTICODE_TRUSTED_PUBLISHER => "authenticode",
            _ => "signed",
        };
        return Signature { signed: Some(true), signer, kind: Some(kind.into()) };
    }
    Signature { signed: Some(false), signer, kind: Some("unsigned".into()) }
}

fn wide_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

fn inf_dir() -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    format!(r"{root}\INF")
}

fn verify_inf(inf: &str) -> Signature {
    // Only plain file names from the driver store's INF folder.
    if inf.is_empty() || inf.contains(['\\', '/', ':']) {
        return Signature { signed: None, signer: None, kind: None };
    }
    let path = format!(r"{}\{inf}", inf_dir());
    let mut info = SP_INF_SIGNER_INFO_V2_W {
        cbSize: std::mem::size_of::<SP_INF_SIGNER_INFO_V2_W>() as u32,
        ..unsafe { std::mem::zeroed() }
    };
    let ok = unsafe { SetupVerifyInfFileW(&HSTRING::from(path.as_str()), None, &mut info) }.as_bool();
    let err = if ok { 0 } else { unsafe { GetLastError() }.0 };
    classify_signature(ok, err, info.SignerScore, &wide_to_string(&info.DigitalSigner))
}

/// Per INF name; an INF is never changed in place (a new driver gets a new oemNN.inf).
fn signature_cache() -> &'static Mutex<HashMap<String, Signature>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Signature>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn signature_for(inf: &str) -> Signature {
    let key = inf.to_ascii_lowercase();
    if let Some(s) = lock(signature_cache()).get(&key) {
        return s.clone();
    }
    let s = verify_inf(inf);
    lock(signature_cache()).insert(key, s.clone());
    s
}

// ---------------------------------------------------------------------------
// Device scan (SetupAPI + CfgMgr32, read only)
// ---------------------------------------------------------------------------

/// What one device reports, before Orbit judges it.
#[derive(Clone, Debug, Default)]
pub struct RawDevice {
    pub id: String,
    pub name: String,
    pub class: String,
    pub manufacturer: Option<String>,
    pub provider: Option<String>,
    pub version: Option<String>,
    pub date: Option<u64>,
    pub inf: Option<String>,
    pub hardware_ids: Vec<String>,
    pub problem: Option<u32>,
    pub hidden: bool,
}

struct DevInfoSet(HDEVINFO);

impl Drop for DevInfoSet {
    fn drop(&mut self) {
        unsafe {
            let _ = SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}

fn prop_raw(set: HDEVINFO, data: &SP_DEVINFO_DATA, key: &DEVPROPKEY) -> Option<(DEVPROPTYPE, Vec<u8>)> {
    let mut ty = DEVPROPTYPE(0);
    let mut need = 0u32;
    let _ = unsafe { SetupDiGetDevicePropertyW(set, data, key, &mut ty, None, Some(&mut need), 0) };
    if need == 0 || need > 64 * 1024 {
        return None;
    }
    let mut buf = vec![0u8; need as usize];
    unsafe { SetupDiGetDevicePropertyW(set, data, key, &mut ty, Some(&mut buf), None, 0) }.ok()?;
    Some((ty, buf))
}

fn utf16_units(buf: &[u8]) -> Vec<u16> {
    buf.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect()
}

fn prop_string(set: HDEVINFO, data: &SP_DEVINFO_DATA, key: &DEVPROPKEY) -> Option<String> {
    let (ty, buf) = prop_raw(set, data, key)?;
    if ty != DEVPROP_TYPE_STRING {
        return None;
    }
    let s = wide_to_string(&utf16_units(&buf)).trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn prop_strings(set: HDEVINFO, data: &SP_DEVINFO_DATA, key: &DEVPROPKEY) -> Vec<String> {
    let Some((ty, buf)) = prop_raw(set, data, key) else { return Vec::new() };
    if ty != DEVPROP_TYPE_STRING_LIST {
        return Vec::new();
    }
    String::from_utf16_lossy(&utf16_units(&buf))
        .split('\0')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

/// FILETIME (100 ns since 1601) to Unix seconds.
pub fn filetime_to_unix(ft: u64) -> Option<u64> {
    (ft / 10_000_000).checked_sub(11_644_473_600).filter(|&s| s > 0)
}

fn prop_filetime(set: HDEVINFO, data: &SP_DEVINFO_DATA, key: &DEVPROPKEY) -> Option<u64> {
    let (ty, buf) = prop_raw(set, data, key)?;
    if ty != DEVPROP_TYPE_FILETIME || buf.len() < 8 {
        return None;
    }
    filetime_to_unix(u64::from_le_bytes(buf[..8].try_into().ok()?))
}

fn class_description(guid: &GUID, cache: &mut HashMap<u128, Option<String>>) -> Option<String> {
    cache
        .entry(guid.to_u128())
        .or_insert_with(|| {
            let mut buf = [0u16; 256];
            unsafe { SetupDiGetClassDescriptionW(guid, &mut buf, None) }.ok()?;
            Some(wide_to_string(&buf)).filter(|s| !s.trim().is_empty())
        })
        .clone()
}

/// Every present device (DIGCF_PRESENT | DIGCF_ALLCLASSES). Read only.
pub fn scan_devices() -> Result<Vec<RawDevice>, String> {
    let set = unsafe {
        SetupDiGetClassDevsW(None, PCWSTR::null(), HWND::default(), DIGCF_PRESENT | DIGCF_ALLCLASSES)
    }
    .map_err(|e| format!("Windows did not list the devices: {}.", e.message().trim_end_matches('.')))?;
    let set = DevInfoSet(set);
    let mut classes = HashMap::new();
    let mut out = Vec::new();
    for i in 0..10_000u32 {
        let mut data = SP_DEVINFO_DATA { cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
        if unsafe { SetupDiEnumDeviceInfo(set.0, i, &mut data) }.is_err() {
            break; // ERROR_NO_MORE_ITEMS
        }
        let h = set.0;
        let id = prop_string(h, &data, &DEVPKEY_Device_InstanceId).unwrap_or_default();
        let name = prop_string(h, &data, &DEVPKEY_Device_FriendlyName)
            .or_else(|| prop_string(h, &data, &DEVPKEY_Device_DeviceDesc))
            .unwrap_or_else(|| "Unknown device".into());
        let class = class_description(&data.ClassGuid, &mut classes)
            .or_else(|| prop_string(h, &data, &DEVPKEY_Device_Class))
            .unwrap_or_else(|| "Other devices".into());
        let mut hardware_ids = prop_strings(h, &data, &DEVPKEY_Device_HardwareIds);
        hardware_ids.extend(prop_strings(h, &data, &DEVPKEY_Device_CompatibleIds));
        let mut status = CM_DEVNODE_STATUS_FLAGS(0);
        let mut problem = CM_PROB(0);
        let cr = unsafe { CM_Get_DevNode_Status(&mut status, &mut problem, data.DevInst, 0) };
        let has_problem = cr == CR_SUCCESS && status.0 & DN_HAS_PROBLEM.0 != 0;
        out.push(RawDevice {
            id,
            name,
            class,
            manufacturer: prop_string(h, &data, &DEVPKEY_Device_Manufacturer),
            provider: prop_string(h, &data, &DEVPKEY_Device_DriverProvider),
            version: prop_string(h, &data, &DEVPKEY_Device_DriverVersion),
            date: prop_filetime(h, &data, &DEVPKEY_Device_DriverDate),
            inf: prop_string(h, &data, &DEVPKEY_Device_DriverInfPath),
            hardware_ids,
            problem: has_problem.then_some(problem.0),
            hidden: cr == CR_SUCCESS && status.0 & DN_NO_SHOW_IN_DM.0 != 0,
        });
    }
    Ok(out)
}

/// An oemNN.inf was added to the driver store; anything else came with Windows.
pub fn is_inbox(inf: Option<&str>) -> bool {
    inf.is_some_and(|i| !i.to_ascii_lowercase().starts_with("oem"))
}

/// Judges one device. `sig` is the signature of its INF (None: no driver).
pub fn assess(raw: RawDevice, sig: Option<Signature>, now: u64) -> Device {
    let inbox = is_inbox(raw.inf.as_deref());
    let sig = sig.unwrap_or(Signature { signed: None, signer: None, kind: None });
    let old = raw.inf.is_some() && is_old(raw.date, inbox, now);
    let flags = flags_for(raw.problem, sig.signed, old);
    let status = flags.first().cloned().unwrap_or_else(|| "ok".into());
    let vendor = vendor_for(raw.provider.as_deref(), raw.manufacturer.as_deref());
    Device {
        id: raw.id,
        name: raw.name,
        class: raw.class,
        manufacturer: raw.manufacturer,
        provider: raw.provider,
        version: raw.version,
        date: raw.date,
        inf: raw.inf,
        inbox,
        signed: sig.signed,
        signer: sig.signer,
        signature: sig.kind,
        problem: raw.problem,
        problem_text: raw.problem.map(problem_text),
        status,
        flags,
        vendor,
        update_id: None,
        hardware_ids: raw.hardware_ids,
    }
}

/// Real problems (not turned off / unplugged) as (name, plain reason), for Health.
pub fn problem_devices() -> Result<Vec<(String, String)>, String> {
    Ok(scan_devices()?
        .into_iter()
        .filter_map(|d| match d.problem {
            Some(c) if !is_soft_problem(c) => Some((d.name, problem_text(c))),
            _ => None,
        })
        .collect())
}

// ---------------------------------------------------------------------------
// System maker (SMBIOS, from the registry copy Windows keeps)
// ---------------------------------------------------------------------------

fn bios_value(name: &str) -> Option<String> {
    let mut buf = [0u16; 256];
    let mut size = (buf.len() * 2) as u32;
    let rc = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            &HSTRING::from(r"HARDWARE\DESCRIPTION\System\BIOS"),
            &HSTRING::from(name),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if rc.is_err() {
        return None;
    }
    Some(wide_to_string(&buf).trim().to_string()).filter(|s| !s.is_empty())
}

pub fn read_system() -> SystemInfo {
    let manufacturer = bios_value("SystemManufacturer");
    let board = bios_value("BaseBoardManufacturer");
    let vendor = pc_vendor(manufacturer.as_deref(), board.as_deref());
    let manufacturer = manufacturer.filter(|m| !is_placeholder(m)).or(board);
    let model = bios_value("SystemProductName").filter(|m| !is_placeholder(m));
    SystemInfo { manufacturer, model, vendor }
}

// ---------------------------------------------------------------------------
// Windows Update driver offers (WUA COM, search only, cached)
// ---------------------------------------------------------------------------

const SEARCH_FRESH: Duration = Duration::from_secs(30 * 60);
/// Drivers only (Health's Windows Update item is software only).
const DRIVER_CRITERIA: &str = "IsInstalled=0 and IsHidden=0 and Type='Driver'";
const MAX_OFFERS: i32 = 200;

struct SearchCache {
    status: DriverUpdateStatus,
    running: bool,
    finished: Option<Instant>,
}

fn search_cache() -> &'static Mutex<SearchCache> {
    static CACHE: OnceLock<Mutex<SearchCache>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(SearchCache { status: DriverUpdateStatus::idle(), running: false, finished: None })
    })
}

/// (instance id, name, hardware + compatible ids) of the last scan, for matching offers.
fn device_index() -> &'static Mutex<Vec<(String, String, Vec<String>)>> {
    static INDEX: OnceLock<Mutex<Vec<(String, String, Vec<String>)>>> = OnceLock::new();
    INDEX.get_or_init(|| Mutex::new(Vec::new()))
}

fn bstr_opt(r: windows::core::Result<BSTR>) -> Option<String> {
    r.ok().map(|b| b.to_string().trim().to_string()).filter(|s| !s.is_empty())
}

fn search_driver_offers(online: bool) -> Result<Vec<DriverOffer>, String> {
    let session: IUpdateSession =
        unsafe { CoCreateInstance(&UpdateSession, None, CLSCTX_ALL) }.map_err(|e| wua_error(&e))?;
    unsafe {
        let _ = session.SetClientApplicationID(&BSTR::from("Orbit"));
        let searcher = session.CreateUpdateSearcher().map_err(|e| wua_error(&e))?;
        searcher.SetOnline(VARIANT_BOOL::from(online)).map_err(|e| wua_error(&e))?;
        let result = searcher.Search(&BSTR::from(DRIVER_CRITERIA)).map_err(|e| wua_error(&e))?;
        let updates = result.Updates().map_err(|e| wua_error(&e))?;
        let count = updates.Count().map_err(|e| wua_error(&e))?.clamp(0, MAX_OFFERS);
        let mut out: Vec<DriverOffer> = Vec::new();
        for i in 0..count {
            let Ok(u) = updates.get_Item(i) else { continue };
            let Some(id) = u.Identity().ok().and_then(|x| bstr_opt(x.UpdateID())) else { continue };
            if out.iter().any(|o| o.id.eq_ignore_ascii_case(&id)) {
                continue;
            }
            let drv = u.cast::<IWindowsDriverUpdate>().ok();
            let optional = u.cast::<IUpdate3>().ok().and_then(|u3| u3.BrowseOnly().ok()).is_some_and(|b| b.as_bool());
            out.push(DriverOffer {
                id,
                title: bstr_opt(u.Title()).unwrap_or_else(|| "Driver update".into()),
                class: drv.as_ref().and_then(|d| bstr_opt(d.DriverClass())),
                provider: drv.as_ref().and_then(|d| bstr_opt(d.DriverProvider())),
                manufacturer: drv.as_ref().and_then(|d| bstr_opt(d.DriverManufacturer())),
                model: drv.as_ref().and_then(|d| bstr_opt(d.DriverModel())),
                hardware_id: drv.as_ref().and_then(|d| bstr_opt(d.DriverHardwareID())),
                date: drv.as_ref().and_then(|d| d.DriverVerDate().ok()).and_then(ole_date_to_unix),
                optional,
                downloaded: u.IsDownloaded().is_ok_and(|b| b.as_bool()),
                device_id: None,
                device_name: None,
            });
        }
        Ok(out)
    }
}

/// Links each offer to the present device whose hardware or compatible ids
/// include the offer's hardware id (ignoring case).
pub fn match_offers(offers: &mut [DriverOffer], devices: &[(String, String, Vec<String>)]) {
    for o in offers.iter_mut() {
        let Some(hw) = o.hardware_id.as_deref() else { continue };
        if let Some((id, name, _)) = devices
            .iter()
            .find(|(_, _, ids)| ids.iter().any(|x| x.eq_ignore_ascii_case(hw)))
        {
            o.device_id = Some(id.clone());
            o.device_name = Some(name.clone());
        }
    }
}

fn run_search(online: bool) {
    let result = {
        let _com = com_init();
        search_driver_offers(online)
    };
    let index = lock(device_index()).clone();
    let mut cache = lock(search_cache());
    let s = &mut cache.status;
    s.checked_at = Some(now_secs());
    s.online = online;
    match result {
        Ok(mut offers) => {
            match_offers(&mut offers, &index);
            s.state = "ready".into();
            s.offers = offers;
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

/// Starts a search when none ran yet, the last one is 30+ minutes old, or
/// `force` (then online). Returns the current status right away.
pub fn ensure_search(force: bool) -> DriverUpdateStatus {
    let mut cache = lock(search_cache());
    let stale = cache.finished.map_or(true, |t| t.elapsed() >= SEARCH_FRESH);
    if !cache.running && cache.status.installing.is_none() && (force || stale) {
        cache.running = true;
        cache.status.state = "checking".into();
        let spawned = std::thread::Builder::new()
            .name("orbit-driver-updates".into())
            .spawn(move || run_search(force));
        if spawned.is_err() {
            cache.running = false;
            cache.status.state = "error".into();
            cache.status.error = Some("Could not start the Windows Update check.".into());
        }
    }
    cache.status.clone()
}

// ---------------------------------------------------------------------------
// Installing a Windows Update driver (admin, confirmed in the UI)
// ---------------------------------------------------------------------------

/// What WUA reported for one download + install.
#[derive(Clone, Debug, PartialEq)]
pub struct RawInstall {
    /// OperationResultCode of the download (2 succeeded, 3 with errors, 4 failed, 5 aborted).
    pub download: i32,
    /// Of the install; None when the download failed.
    pub install: Option<i32>,
    pub restart: bool,
    pub hresult: i32,
}

/// The step that touches Windows. The real one is `WuaInstaller`; tests use fakes.
pub trait UpdateInstaller {
    fn install(&self, update_id: &str) -> Result<RawInstall, String>;
}

/// A WUA UpdateID: a GUID, hex digits and hyphens only (it goes into a search string).
pub fn valid_update_id(id: &str) -> bool {
    id.len() == 36
        && id.chars().enumerate().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) { c == '-' } else { c.is_ascii_hexdigit() }
        })
}

fn result_name(code: i32) -> &'static str {
    match code {
        2 => "succeeded",
        3 => "succeededWithErrors",
        5 => "aborted",
        _ => "failed",
    }
}

/// Checks, runs `inst` once, and words the outcome. `busy` keeps it to one at a time.
pub fn install_with(
    inst: &dyn UpdateInstaller,
    offers: &[DriverOffer],
    id: &str,
    is_admin: bool,
    busy: &AtomicBool,
    now: u64,
) -> Result<InstallResult, String> {
    let id = id.trim();
    if !valid_update_id(id) {
        return Err("That is not a Windows Update driver.".into());
    }
    let Some(offer) = offers.iter().find(|o| o.id.eq_ignore_ascii_case(id)) else {
        return Err("Windows Update no longer offers this driver. Check again.".into());
    };
    if !is_admin {
        return Err("Installing a driver needs administrator rights. Restart Orbit as administrator first.".into());
    }
    if busy.swap(true, Ordering::SeqCst) {
        return Err("Another driver is being installed. Wait for it to finish.".into());
    }
    let raw = inst.install(&offer.id);
    busy.store(false, Ordering::SeqCst);
    let raw = raw?;
    let title = offer.title.clone();
    let mk = |ok: bool, result: &str, restart: bool, message: String| InstallResult {
        id: offer.id.clone(),
        title: title.clone(),
        ok,
        result: result.into(),
        restart_required: restart,
        message,
        at: now,
    };
    if !matches!(raw.download, 2 | 3) {
        let msg = if raw.download == 5 {
            "The download was cancelled. Nothing was installed.".to_string()
        } else {
            format!("Windows could not download the driver (0x{:08X}). Nothing was installed.", raw.hresult as u32)
        };
        return Ok(mk(false, result_name(raw.download), false, msg));
    }
    let code = raw.install.unwrap_or(4);
    let restart_note = if raw.restart { " Restart the PC to finish." } else { "" };
    Ok(match code {
        2 => mk(true, "succeeded", raw.restart, format!("Installed.{restart_note}")),
        3 => mk(true, "succeededWithErrors", raw.restart, format!("Installed, but Windows reported errors.{restart_note}")),
        5 => mk(false, "aborted", raw.restart, "The install was cancelled.".into()),
        _ => mk(false, "failed", raw.restart, format!("Windows could not install the driver (0x{:08X}).", raw.hresult as u32)),
    })
}

/// Download and install through Windows Update's own downloader / installer.
/// Only called from `install_driver_update` after the user confirmed.
pub struct WuaInstaller;

impl UpdateInstaller for WuaInstaller {
    fn install(&self, update_id: &str) -> Result<RawInstall, String> {
        if !valid_update_id(update_id) {
            return Err("That is not a Windows Update driver.".into());
        }
        let _com = com_init();
        let session: IUpdateSession =
            unsafe { CoCreateInstance(&UpdateSession, None, CLSCTX_ALL) }.map_err(|e| wua_error(&e))?;
        unsafe {
            let _ = session.SetClientApplicationID(&BSTR::from("Orbit"));
            let searcher = session.CreateUpdateSearcher().map_err(|e| wua_error(&e))?;
            searcher.SetOnline(VARIANT_BOOL::from(false)).map_err(|e| wua_error(&e))?;
            let criteria = format!("UpdateID='{update_id}' and IsInstalled=0 and Type='Driver'");
            let found = searcher.Search(&BSTR::from(criteria.as_str())).map_err(|e| wua_error(&e))?;
            let updates = found.Updates().map_err(|e| wua_error(&e))?;
            if updates.Count().unwrap_or(0) < 1 {
                return Err("Windows Update no longer offers this driver. Check again.".into());
            }
            let update: IUpdate = updates.get_Item(0).map_err(|e| wua_error(&e))?;
            // Orbit never accepts a license on the user's behalf.
            if !update.EulaAccepted().is_ok_and(|b| b.as_bool()) {
                return Err("This driver comes with a license to accept. Install it from Windows Update in Settings.".into());
            }
            let list: IUpdateCollection =
                CoCreateInstance(&UpdateCollection, None, CLSCTX_ALL).map_err(|e| wua_error(&e))?;
            list.Add(&update).map_err(|e| wua_error(&e))?;
            let downloader = session.CreateUpdateDownloader().map_err(|e| wua_error(&e))?;
            downloader.SetUpdates(&list).map_err(|e| wua_error(&e))?;
            let dl = downloader.Download().map_err(|e| wua_error(&e))?;
            let download = dl.ResultCode().map(|c| c.0).unwrap_or(4);
            if !matches!(download, 2 | 3) {
                let hresult = dl.GetUpdateResult(0).and_then(|r| r.HResult()).unwrap_or(0);
                return Ok(RawInstall { download, install: None, restart: false, hresult });
            }
            let installer = session.CreateUpdateInstaller().map_err(|e| wua_error(&e))?;
            installer.SetUpdates(&list).map_err(|e| wua_error(&e))?;
            let res = installer.Install().map_err(|e| wua_error(&e))?;
            Ok(RawInstall {
                download,
                install: Some(res.ResultCode().map(|c| c.0).unwrap_or(4)),
                restart: res.RebootRequired().is_ok_and(|b| b.as_bool()),
                hresult: res.GetUpdateResult(0).and_then(|r| r.HResult()).unwrap_or(0),
            })
        }
    }
}

static INSTALLING: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------------------
// Listing
// ---------------------------------------------------------------------------

fn is_admin() -> bool {
    unsafe { windows::Win32::UI::Shell::IsUserAnAdmin() }.as_bool()
}

/// Scan, judge, match against the last Windows Update search, sort worst first.
pub fn build_list(updates: DriverUpdateStatus) -> Result<DriverList, String> {
    let raw = scan_devices()?;
    let now = now_secs();
    let mut hidden = 0u32;
    let mut devices: Vec<Device> = Vec::new();
    for r in raw {
        if r.hidden && r.problem.is_none() {
            hidden += 1;
            continue;
        }
        let sig = r.inf.as_deref().map(signature_for);
        devices.push(assess(r, sig, now));
    }
    let index: Vec<_> = devices.iter().map(|d| (d.id.clone(), d.name.clone(), d.hardware_ids.clone())).collect();
    let mut updates = updates;
    match_offers(&mut updates.offers, &index);
    for o in &updates.offers {
        if let Some(dev) = o.device_id.as_ref().and_then(|id| devices.iter_mut().find(|d| &d.id == id)) {
            dev.update_id.get_or_insert_with(|| o.id.clone());
        }
    }
    *lock(device_index()) = index;
    sort_devices(&mut devices);
    Ok(DriverList {
        scanned_at: now,
        is_admin: is_admin(),
        old_years: OLD_YEARS,
        counts: counts(&devices),
        devices,
        hidden,
        system: read_system(),
        updates,
    })
}

// ---------------------------------------------------------------------------
// Health item
// ---------------------------------------------------------------------------

/// "Devices" in the Health report: devices with a real problem code.
pub fn devices_item(problems: Result<&[(String, String)], &str>) -> HealthItem {
    let mut it = HealthItem {
        key: "devices".into(),
        title: "Devices".into(),
        status: "good".into(),
        text: "No device reports a problem.".into(),
        detail: None,
        action: None,
    };
    match problems {
        Err(e) => {
            it.status = "unknown".into();
            it.text = e.to_string();
        }
        Ok([]) => {}
        Ok(list) => {
            it.status = "warning".into();
            it.text = if list.len() == 1 {
                format!("{} reports a problem.", list[0].0)
            } else {
                format!("{} devices report a problem.", list.len())
            };
            it.detail = Some(
                list.iter().take(5).map(|(n, t)| format!("{n}: {t}")).collect::<Vec<_>>().join(" "),
            );
            it.action = Some(crate::health::HealthAction {
                label: "Go to Drivers".into(),
                kind: "optimize".into(),
                target: "drivers".into(),
            });
        }
    }
    it
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Every present device with its driver, worst first. Starts a Windows Update
/// driver search (from Windows' cache) when the last one is stale.
#[tauri::command]
pub async fn list_drivers() -> Result<DriverList, String> {
    let updates = ensure_search(false);
    tauri::async_runtime::spawn_blocking(move || build_list(updates))
        .await
        .map_err(|e| format!("Could not list the drivers: {e}"))?
}

/// The cached Windows Update driver search; `refresh: true` asks the server now.
#[tauri::command]
pub fn get_driver_updates(refresh: Option<bool>) -> DriverUpdateStatus {
    let mut s = ensure_search(refresh.unwrap_or(false));
    if s.state == "checking" {
        s = lock(search_cache()).status.clone();
    }
    s
}

/// Downloads and installs one driver Windows Update offered (by UpdateID),
/// through Windows Update itself. Needs admin. The UI asks first.
#[tauri::command]
pub async fn install_driver_update(id: String) -> Result<InstallResult, String> {
    let admin = is_admin();
    let offers = {
        let mut cache = lock(search_cache());
        if cache.running {
            return Err("Windows Update is still searching. Try again when it is done.".into());
        }
        if admin && valid_update_id(id.trim()) {
            cache.status.installing = Some(id.trim().to_string());
        }
        cache.status.offers.clone()
    };
    let run_id = id.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        install_with(&WuaInstaller, &offers, &run_id, admin, &INSTALLING, now_secs())
    })
    .await
    .map_err(|e| format!("The install did not finish: {e}"));
    let mut cache = lock(search_cache());
    cache.status.installing = None;
    let result = result?;
    if let Ok(r) = &result {
        cache.status.last_install = Some(r.clone());
        if r.ok {
            cache.status.offers.retain(|o| !o.id.eq_ignore_ascii_case(&r.id));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_793_000_000; // 2026-10
    const YEAR: u64 = 31_557_600;

    fn raw(name: &str, inf: Option<&str>, provider: Option<&str>, date: Option<u64>, problem: Option<u32>) -> RawDevice {
        RawDevice {
            id: format!("PCI\\{name}"),
            name: name.into(),
            class: "Display adapters".into(),
            provider: provider.map(String::from),
            inf: inf.map(String::from),
            date,
            problem,
            hardware_ids: vec![format!("PCI\\VEN_{name}")],
            ..Default::default()
        }
    }

    fn signed() -> Option<Signature> {
        Some(Signature { signed: Some(true), signer: Some("Microsoft Windows Hardware Compatibility Publisher".into()), kind: Some("whql".into()) })
    }

    #[test]
    fn problem_texts_cover_the_common_codes() {
        for code in [1, 10, 12, 14, 18, 19, 21, 22, 24, 28, 31, 39, 43, 45, 52] {
            let t = problem_text(code);
            assert!(!t.contains("code"), "code {code} has no plain text: {t}");
            assert!(t.ends_with('.'));
        }
        assert_eq!(problem_text(99), "This device reports a problem (code 99).");
        assert!(is_soft_problem(22) && is_soft_problem(45) && !is_soft_problem(28));
    }

    #[test]
    fn makers_match_on_words() {
        let m = |p: &str| vendor_for(Some(p), None).map(|v| v.name);
        assert_eq!(m("NVIDIA").as_deref(), Some("NVIDIA"));
        assert_eq!(m("Advanced Micro Devices, Inc.").as_deref(), Some("AMD"));
        assert_eq!(m("Intel(R) Corporation").as_deref(), Some("Intel"));
        assert_eq!(m("Realtek Semiconductor Corp.").as_deref(), Some("Realtek"));
        assert_eq!(m("Logitech").as_deref(), Some("Logitech"));
        assert_eq!(m("Microsoft"), None);
        // No partial words: "Intelligent" is not Intel, "Samdisk" not AMD.
        assert_eq!(m("Intelligent Devices"), None);
        assert_eq!(m("Samdisk"), None);
        // Provider first, then the device maker.
        assert_eq!(vendor_for(Some("Microsoft"), Some("Intel")).map(|v| v.name).as_deref(), Some("Intel"));
        for maker in MAKERS {
            assert!(maker.url.starts_with("https://"), "{}", maker.name);
            assert!(!maker.url.contains(' '));
        }
    }

    #[test]
    fn pc_makers_come_from_smbios() {
        let p = |s: Option<&str>, b: Option<&str>| pc_vendor(s, b).map(|v| v.name);
        assert_eq!(p(Some("Dell Inc."), None).as_deref(), Some("Dell"));
        assert_eq!(p(Some("HP"), None).as_deref(), Some("HP"));
        assert_eq!(p(Some("Hewlett-Packard"), None).as_deref(), Some("HP"));
        assert_eq!(p(Some("LENOVO"), None).as_deref(), Some("Lenovo"));
        assert_eq!(p(Some("Micro-Star International Co., Ltd."), None).as_deref(), Some("MSI"));
        // Self-built PC: placeholder system name, board maker counts.
        assert_eq!(p(Some("System manufacturer"), Some("ASUSTeK COMPUTER INC.")).as_deref(), Some("ASUS"));
        assert_eq!(p(Some("To Be Filled By O.E.M."), Some("Gigabyte Technology Co., Ltd.")), None);
        // Chip makers are not PC makers.
        assert_eq!(p(Some("Intel Corporation"), None), None);
    }

    #[test]
    fn age_never_flags_inbox_drivers() {
        assert!(is_old(Some(NOW - 6 * YEAR), false, NOW));
        assert!(!is_old(Some(NOW - 4 * YEAR), false, NOW));
        assert!(!is_old(Some(NOW - 20 * YEAR), true, NOW));
        assert!(!is_old(None, false, NOW));
        assert!(is_inbox(Some("display.inf")) && !is_inbox(Some("oem42.inf")) && !is_inbox(None));
    }

    #[test]
    fn signatures() {
        let s = classify_signature(true, 0, SIGNERSCORE_WHQL, "Microsoft Windows Hardware Compatibility Publisher");
        assert_eq!((s.signed, s.kind.as_deref()), (Some(true), Some("whql")));
        assert_eq!(classify_signature(true, 0, SIGNERSCORE_INBOX, "Microsoft Windows").kind.as_deref(), Some("windows"));
        let a = classify_signature(false, ERROR_AUTHENTICODE_TRUSTED_PUBLISHER, SIGNERSCORE_AUTHENTICODE, "Contoso");
        assert_eq!((a.signed, a.kind.as_deref(), a.signer.as_deref()), (Some(true), Some("authenticode"), Some("Contoso")));
        let u = classify_signature(false, 0x800B_0100, 0x8000_0000, "");
        assert_eq!((u.signed, u.kind.as_deref(), u.signer), (Some(false), Some("unsigned"), None));
        let missing = classify_signature(false, ERROR_FILE_NOT_FOUND, 0, "");
        assert_eq!(missing.signed, None);
    }

    #[test]
    fn assess_flags_and_status() {
        let d = assess(raw("GPU", Some("oem3.inf"), Some("NVIDIA"), Some(NOW - YEAR), None), signed(), NOW);
        assert_eq!((d.status.as_str(), d.flags.len()), ("ok", 0));
        assert_eq!(d.vendor.as_ref().map(|v| v.name.as_str()), Some("NVIDIA"));

        let bad = assess(raw("Cam", Some("oem9.inf"), Some("Acme"), Some(NOW - 7 * YEAR), Some(10)),
            Some(Signature { signed: Some(false), signer: None, kind: Some("unsigned".into()) }), NOW);
        assert_eq!(bad.flags, vec!["problem", "unsigned", "old"]);
        assert_eq!(bad.status, "problem");
        assert!(bad.problem_text.as_deref().unwrap().starts_with("This device cannot start"));

        let off = assess(raw("Wifi", Some("netwtw.inf"), Some("Intel"), Some(NOW - 9 * YEAR), Some(22)), signed(), NOW);
        assert_eq!(off.flags, vec!["off"]); // inbox: not old; disabled: soft
        assert!(off.inbox);

        let none = assess(raw("Unknown", None, None, None, Some(28)), None, NOW);
        assert_eq!((none.status.as_str(), none.signed), ("problem", None));
    }

    #[test]
    fn sort_and_count_worst_first() {
        let mut list = vec![
            assess(raw("b-ok", Some("oem1.inf"), None, Some(NOW), None), signed(), NOW),
            assess(raw("a-old", Some("oem2.inf"), None, Some(NOW - 8 * YEAR), None), signed(), NOW),
            assess(raw("c-off", Some("oem3.inf"), None, Some(NOW), Some(22)), signed(), NOW),
            assess(raw("d-bad", None, None, None, Some(43)), None, NOW),
        ];
        sort_devices(&mut list);
        let names: Vec<_> = list.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["d-bad", "a-old", "c-off", "b-ok"]);
        assert_eq!(counts(&list), Counts { total: 4, problem: 1, unsigned: 0, old: 1, off: 1 });
    }

    fn offer(id: &str, hw: Option<&str>) -> DriverOffer {
        DriverOffer {
            id: id.into(),
            title: "NVIDIA - Display - 31.0.15.5222".into(),
            class: Some("Display".into()),
            provider: Some("NVIDIA".into()),
            manufacturer: None,
            model: None,
            hardware_id: hw.map(String::from),
            date: None,
            optional: true,
            downloaded: false,
            device_id: None,
            device_name: None,
        }
    }

    #[test]
    fn offers_match_devices_by_hardware_id() {
        let devs = vec![("PCI\\GPU".to_string(), "GeForce".to_string(), vec!["PCI\\VEN_10DE&DEV_2484".to_string()])];
        let mut offers = vec![offer("a", Some("pci\\ven_10de&dev_2484")), offer("b", Some("USB\\X")), offer("c", None)];
        match_offers(&mut offers, &devs);
        assert_eq!(offers[0].device_name.as_deref(), Some("GeForce"));
        assert_eq!(offers[1].device_id, None);
        assert_eq!(offers[2].device_id, None);
    }

    #[test]
    fn update_ids() {
        assert!(valid_update_id("0b5c8a52-4f1c-4c4b-9d3f-6f0d4e1a2b3c"));
        assert!(!valid_update_id("0b5c8a52-4f1c-4c4b-9d3f-6f0d4e1a2b3"));
        assert!(!valid_update_id("0b5c8a52' or IsInstalled=1 or 'x'='x"));
        assert!(!valid_update_id("0b5c8a52x4f1c-4c4b-9d3f-6f0d4e1a2b3c"));
    }

    /// Never touches Windows: records the call and returns a canned answer.
    struct Fake {
        answer: Result<RawInstall, String>,
        calls: std::cell::Cell<u32>,
    }

    impl UpdateInstaller for Fake {
        fn install(&self, _id: &str) -> Result<RawInstall, String> {
            self.calls.set(self.calls.get() + 1);
            self.answer.clone()
        }
    }

    fn fake(download: i32, install: Option<i32>, restart: bool) -> Fake {
        Fake { answer: Ok(RawInstall { download, install, restart, hresult: 0x8024_0022u32 as i32 }), calls: Default::default() }
    }

    const ID: &str = "0b5c8a52-4f1c-4c4b-9d3f-6f0d4e1a2b3c";

    #[test]
    fn install_checks_before_calling_windows() {
        let offers = vec![offer(ID, None)];
        let busy = AtomicBool::new(false);
        let f = fake(2, Some(2), false);
        assert!(install_with(&f, &offers, "nope", true, &busy, NOW).unwrap_err().contains("not a Windows Update"));
        assert!(install_with(&f, &offers, "11111111-1111-1111-1111-111111111111", true, &busy, NOW).unwrap_err().contains("no longer offers"));
        assert!(install_with(&f, &offers, ID, false, &busy, NOW).unwrap_err().contains("administrator"));
        busy.store(true, Ordering::SeqCst);
        assert!(install_with(&f, &offers, ID, true, &busy, NOW).unwrap_err().contains("Another driver"));
        assert_eq!(f.calls.get(), 0);
    }

    #[test]
    fn install_outcomes() {
        let offers = vec![offer(ID, None)];
        let busy = AtomicBool::new(false);
        let run = |f: Fake| {
            let r = install_with(&f, &offers, &ID.to_uppercase(), true, &busy, NOW);
            assert_eq!(f.calls.get(), 1);
            assert!(!busy.load(Ordering::SeqCst), "busy flag released");
            r
        };
        let ok = run(fake(2, Some(2), true)).unwrap();
        assert!(ok.ok && ok.restart_required);
        assert_eq!(ok.message, "Installed. Restart the PC to finish.");
        assert_eq!(ok.id, ID);
        let plain = run(fake(2, Some(2), false)).unwrap();
        assert_eq!(plain.message, "Installed.");
        let partial = run(fake(3, Some(3), false)).unwrap();
        assert_eq!((partial.ok, partial.result.as_str()), (true, "succeededWithErrors"));
        let dl = run(fake(4, None, false)).unwrap();
        assert!(!dl.ok && dl.message.contains("0x80240022") && dl.message.contains("Nothing was installed"));
        let failed = run(fake(2, Some(4), false)).unwrap();
        assert_eq!((failed.ok, failed.result.as_str()), (false, "failed"));
        let aborted = run(fake(2, Some(5), false)).unwrap();
        assert_eq!(aborted.result, "aborted");
        let err = run(Fake { answer: Err("Windows Update did not answer.".into()), calls: Default::default() });
        assert_eq!(err.unwrap_err(), "Windows Update did not answer.");
    }

    #[test]
    fn health_item_texts() {
        let good = devices_item(Ok(&[]));
        assert_eq!((good.status.as_str(), good.action), ("good", None));
        let one = devices_item(Ok(&[("Webcam".into(), problem_text(10))]));
        assert_eq!(one.text, "Webcam reports a problem.");
        assert_eq!(one.action.unwrap().target, "drivers");
        let two = devices_item(Ok(&[("A".into(), "x.".into()), ("B".into(), "y.".into())]));
        assert_eq!((two.status.as_str(), two.text.as_str()), ("warning", "2 devices report a problem."));
        assert_eq!(devices_item(Err("No list.")).status, "unknown");
    }

    #[test]
    fn filetimes() {
        assert_eq!(filetime_to_unix(116_444_736_000_000_000 + 10_000_000), Some(1));
        assert_eq!(filetime_to_unix(0), None);
    }

    #[test]
    fn scans_devices_read_only() {
        // Live, read only: every PC has some present devices.
        let devs = scan_devices().expect("device list");
        assert!(devs.len() > 5, "{} devices", devs.len());
        assert!(devs.iter().any(|d| d.inf.is_some() && d.version.is_some()));
    }

    /// Manual: `cargo test drivers::tests::live -- --ignored --nocapture`
    /// prints the device list and an offline Windows Update driver search.
    #[test]
    #[ignore]
    fn live() {
        let t = Instant::now();
        let raw = scan_devices().unwrap();
        let infs: std::collections::HashSet<_> = raw.iter().filter_map(|d| d.inf.clone()).collect();
        println!("devices only: {:?}, {} devices, {} INFs", t.elapsed(), raw.len(), infs.len());
        let t = Instant::now();
        let list = build_list(DriverUpdateStatus::idle()).unwrap();
        println!("scan: {:?}, {} shown, {} hidden, counts {:?}", t.elapsed(), list.devices.len(), list.hidden, list.counts);
        println!("system: {:?}", list.system);
        for d in list.devices.iter().filter(|d| d.status != "ok") {
            println!("{} [{}] {} / {:?} {:?} {:?} {:?}", d.status, d.class, d.name, d.provider, d.version, d.inf, d.problem_text);
        }
        let t = Instant::now();
        run_search(false);
        let s = lock(search_cache()).status.clone();
        println!("driver search ({:?}): {} {:?}", t.elapsed(), s.state, s.error);
        for o in &s.offers {
            println!("  offer {} | {} | {:?} | optional {} | device {:?}", o.id, o.title, o.hardware_id, o.optional, o.device_name);
        }
    }
}
