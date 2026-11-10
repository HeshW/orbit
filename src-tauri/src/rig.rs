//! My Rig: one read-only spec sheet for the PC. No admin.
//!
//! - CPU: WMI `Win32_Processor` (name, cores, threads, MaxClockSpeed = the base clock).
//! - Memory: WMI `Win32_PhysicalMemory` (one row per module: size, configured speed,
//!   SMBIOS type) and `Win32_PhysicalMemoryArray` (slots on the board).
//! - Graphics: `gpu::adapters` (DXGI, same list and twin rule as Performance).
//! - Motherboard and BIOS: the SMBIOS copy Windows keeps under
//!   `HKLM\HARDWARE\DESCRIPTION\System\BIOS` (the same place drivers.rs reads the PC maker).
//! - Drives: `health::read_physical_disks` (Storage WMI `MSFT_PhysicalDisk`).
//! - Displays: `QueryDisplayConfig` (active paths: resolution, refresh rate, the monitor's
//!   own name from its EDID, built-in or not).
//! - Windows: `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion` (ProductName,
//!   DisplayVersion, CurrentBuild, UBR). Windows 11 still says "Windows 10" in ProductName;
//!   a build of 22000 or later fixes the name.
//!
//! Hardware does not change while Orbit runs, so everything but the GPU list (filtered
//! against the latest snapshot's counters) is read once and cached.

use std::sync::Mutex;

use serde::Serialize;
use tauri::State;
use windows::core::HSTRING;
use windows::Win32::Devices::Display::{
    DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig,
    DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_MODE_INFO,
    DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED,
    DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED,
    DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_TARGET_DEVICE_NAME, QDC_ONLY_ACTIVE_PATHS,
};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
};

use crate::gpu::{self, GpuAdapter};
use crate::health::{self, prop, variant_string, variant_u64, Wmi};
use crate::sampler::AppState;

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct RigSpecs {
    pub cpu: CpuSpec,
    pub memory: MemorySpec,
    /// Most video memory first (gpu.rs).
    pub gpus: Vec<GpuAdapter>,
    pub board: BoardSpec,
    pub drives: Vec<DriveSpec>,
    pub displays: Vec<DisplaySpec>,
    pub windows: WindowsSpec,
    /// Unix seconds.
    pub read_at: u64,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CpuSpec {
    /// As the processor reports it: "12th Gen Intel(R) Core(TM) i5-12400F".
    pub name: Option<String>,
    /// Summed over sockets.
    pub cores: Option<u32>,
    pub threads: Option<u32>,
    /// MHz (WMI MaxClockSpeed, the rated base clock).
    pub base_mhz: Option<u32>,
    pub sockets: u32,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MemorySpec {
    /// Bytes installed (sum of the modules), `None` when WMI lists none.
    pub total: Option<u64>,
    /// Bytes per module, in slot order.
    pub modules: Vec<u64>,
    /// MT/s the memory runs at (ConfiguredClockSpeed, else Speed); the slowest module.
    pub speed: Option<u32>,
    /// "DDR4", "DDR5", "LPDDR5", ... from SMBIOSMemoryType.
    pub kind: Option<String>,
    pub slots_used: u32,
    /// Slots on the board, `None` when the firmware does not say.
    pub slots_total: Option<u32>,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BoardSpec {
    pub maker: Option<String>,
    pub model: Option<String>,
    pub bios_vendor: Option<String>,
    pub bios_version: Option<String>,
    /// As the firmware writes it, usually "MM/DD/YYYY".
    pub bios_date: Option<String>,
    /// The PC's own maker and model when it is a brand PC (laptops, prebuilts);
    /// `None` when it repeats the board or is a placeholder.
    pub system_maker: Option<String>,
    pub system_model: Option<String>,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DriveSpec {
    pub name: String,
    /// Bytes.
    pub size: u64,
    /// "SSD" | "HDD" | "SCM" | "Unknown".
    pub media: String,
    /// "NVMe", "SATA", "USB", ... or "".
    pub bus: String,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DisplaySpec {
    /// The monitor's EDID name ("LG ULTRAGEAR"), `None` when it gives none.
    pub name: Option<String>,
    pub width: u32,
    pub height: u32,
    /// Hz, two decimals (59.94 stays 59.94).
    pub refresh_hz: f64,
    /// At the desktop origin: the main display.
    pub primary: bool,
    /// A laptop's own panel.
    pub internal: bool,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WindowsSpec {
    /// "Windows 11 Pro".
    pub edition: Option<String>,
    /// "23H2".
    pub version: Option<String>,
    /// "22631.4460".
    pub build: Option<String>,
}

// ---------------------------------------------------------------------------
// Pure helpers (unit tested)
// ---------------------------------------------------------------------------

/// SMBIOS memory type codes (DSP0134 table 76) people know.
pub fn memory_kind(code: u64) -> Option<&'static str> {
    Some(match code {
        20 => "DDR",
        21 => "DDR2",
        24 => "DDR3",
        26 => "DDR4",
        27 => "LPDDR",
        28 => "LPDDR2",
        29 => "LPDDR3",
        30 => "LPDDR4",
        34 => "DDR5",
        35 => "LPDDR5",
        _ => return None,
    })
}

/// Registry ProductName with the Windows 11 fix: builds from 22000 on are
/// Windows 11 even though the value still says "Windows 10".
pub fn windows_edition(product: &str, build: Option<u32>) -> String {
    let product = product.trim();
    if build.is_some_and(|b| b >= 22_000) && product.starts_with("Windows 10") {
        return format!("Windows 11{}", &product["Windows 10".len()..]);
    }
    product.to_string()
}

/// A refresh rate from its rational, two decimals; `None` for 0/0.
pub fn refresh_hz(numerator: u32, denominator: u32) -> Option<f64> {
    if numerator == 0 || denominator == 0 {
        return None;
    }
    Some((numerator as f64 / denominator as f64 * 100.0).round() / 100.0)
}

fn some_text(s: Option<String>) -> Option<String> {
    s.map(|v| v.trim().to_string()).filter(|v| !v.is_empty() && !crate::drivers::is_placeholder(v))
}

// ---------------------------------------------------------------------------
// Readers
// ---------------------------------------------------------------------------

pub(crate) fn reg_sz(path: &str, name: &str) -> Option<String> {
    let mut buf = [0u16; 512];
    let mut size = (buf.len() * 2) as u32;
    let rc = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            &HSTRING::from(path),
            &HSTRING::from(name),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if rc != ERROR_SUCCESS {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..end]).trim().to_string()).filter(|s| !s.is_empty())
}

fn reg_dword(path: &str, name: &str) -> Option<u32> {
    let mut value = 0u32;
    let mut size = 4u32;
    let rc = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            &HSTRING::from(path),
            &HSTRING::from(name),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut size),
        )
    };
    (rc == ERROR_SUCCESS).then_some(value)
}

/// CPU and memory from one WMI connection.
fn read_wmi() -> (CpuSpec, MemorySpec) {
    let _com = health::com_init();
    let Ok(wmi) = Wmi::connect(r"ROOT\CIMV2") else {
        return (CpuSpec::default(), MemorySpec::default());
    };
    let mut cpu = CpuSpec::default();
    if let Ok(rows) = wmi.query("SELECT Name, NumberOfCores, NumberOfLogicalProcessors, MaxClockSpeed FROM Win32_Processor") {
        cpu.sockets = rows.len() as u32;
        for o in &rows {
            if cpu.name.is_none() {
                cpu.name = variant_string(&prop(o, "Name")).map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "));
            }
            let add = |acc: Option<u32>, v: Option<u64>| match (acc, v) {
                (a, Some(n)) => Some(a.unwrap_or(0) + n as u32),
                (a, None) => a,
            };
            cpu.cores = add(cpu.cores, variant_u64(&prop(o, "NumberOfCores")));
            cpu.threads = add(cpu.threads, variant_u64(&prop(o, "NumberOfLogicalProcessors")));
            if cpu.base_mhz.is_none() {
                cpu.base_mhz = variant_u64(&prop(o, "MaxClockSpeed")).filter(|&m| m > 0).map(|m| m as u32);
            }
        }
    }

    let mut mem = MemorySpec::default();
    if let Ok(rows) = wmi.query("SELECT Capacity, ConfiguredClockSpeed, Speed, SMBIOSMemoryType FROM Win32_PhysicalMemory") {
        for o in &rows {
            let size = variant_u64(&prop(o, "Capacity")).unwrap_or(0);
            if size == 0 {
                continue;
            }
            mem.modules.push(size);
            let speed = variant_u64(&prop(o, "ConfiguredClockSpeed"))
                .filter(|&s| s > 0)
                .or_else(|| variant_u64(&prop(o, "Speed")).filter(|&s| s > 0))
                .map(|s| s as u32);
            if let Some(s) = speed {
                mem.speed = Some(mem.speed.map_or(s, |m| m.min(s)));
            }
            if mem.kind.is_none() {
                mem.kind = variant_u64(&prop(o, "SMBIOSMemoryType")).and_then(memory_kind).map(str::to_string);
            }
        }
        mem.slots_used = mem.modules.len() as u32;
        if !mem.modules.is_empty() {
            mem.total = Some(mem.modules.iter().sum());
        }
    }
    // Use = 3: system memory (not flash or video memory arrays).
    if let Ok(rows) = wmi.query("SELECT MemoryDevices, Use FROM Win32_PhysicalMemoryArray") {
        let slots: u64 = rows
            .iter()
            .filter(|o| variant_u64(&prop(o, "Use")).is_none_or(|u| u == 3))
            .filter_map(|o| variant_u64(&prop(o, "MemoryDevices")))
            .sum();
        if slots > 0 {
            mem.slots_total = Some((slots as u32).max(mem.slots_used));
        }
    }
    (cpu, mem)
}

fn read_board() -> BoardSpec {
    const BIOS: &str = r"HARDWARE\DESCRIPTION\System\BIOS";
    let get = |name| some_text(reg_sz(BIOS, name));
    let maker = get("BaseBoardManufacturer");
    let model = get("BaseBoardProduct");
    // A brand PC names itself apart from its board; a self-built one repeats the board.
    let system_maker = get("SystemManufacturer").filter(|m| Some(m) != maker.as_ref());
    let system_model = get("SystemProductName").filter(|m| Some(m) != model.as_ref());
    BoardSpec {
        maker,
        model,
        bios_vendor: get("BIOSVendor"),
        bios_version: get("BIOSVersion"),
        bios_date: get("BIOSReleaseDate"),
        system_maker,
        system_model,
    }
}

fn read_drives() -> Vec<DriveSpec> {
    let (disks, _) = health::read_physical_disks(false);
    disks
        .into_iter()
        .filter(|d| d.size > 0)
        .map(|d| DriveSpec { name: d.name.trim().to_string(), size: d.size, media: d.media_type, bus: d.bus_type })
        .collect()
}

fn read_displays() -> Vec<DisplaySpec> {
    let mut paths_n = 0u32;
    let mut modes_n = 0u32;
    if unsafe { GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut paths_n, &mut modes_n) } != ERROR_SUCCESS {
        return Vec::new();
    }
    let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); paths_n as usize];
    let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); modes_n as usize];
    let rc = unsafe {
        QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut paths_n,
            paths.as_mut_ptr(),
            &mut modes_n,
            modes.as_mut_ptr(),
            None,
        )
    };
    if rc != ERROR_SUCCESS {
        return Vec::new();
    }
    paths.truncate(paths_n as usize);
    modes.truncate(modes_n as usize);
    let mut out = Vec::new();
    for p in &paths {
        // Without QDC_VIRTUAL_MODE_AWARE the index is a plain u32.
        let idx = unsafe { p.sourceInfo.Anonymous.modeInfoIdx } as usize;
        let Some(mode) = modes.get(idx).filter(|m| m.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE) else {
            continue;
        };
        let src = unsafe { mode.Anonymous.sourceMode };
        let tech = p.targetInfo.outputTechnology;
        let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
        target.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
        target.header.size = std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
        target.header.adapterId = p.targetInfo.adapterId;
        target.header.id = p.targetInfo.id;
        let name = if unsafe { DisplayConfigGetDeviceInfo(&mut target.header) } == 0 {
            let w = &target.monitorFriendlyDeviceName;
            let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
            Some(String::from_utf16_lossy(&w[..end]).trim().to_string()).filter(|s| !s.is_empty())
        } else {
            None
        };
        let rate = p.targetInfo.refreshRate;
        out.push(DisplaySpec {
            name,
            width: src.width,
            height: src.height,
            refresh_hz: refresh_hz(rate.Numerator, rate.Denominator).unwrap_or(0.0),
            primary: src.position.x == 0 && src.position.y == 0,
            internal: tech == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL
                || tech == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED
                || tech == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED,
        });
    }
    // The main display first, then left to right as Windows lists them.
    out.sort_by_key(|d| !d.primary);
    out
}

fn read_windows() -> WindowsSpec {
    const NT: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let build_text = reg_sz(NT, "CurrentBuild").or_else(|| reg_sz(NT, "CurrentBuildNumber"));
    let build_num = build_text.as_deref().and_then(|b| b.parse::<u32>().ok());
    let build = build_text.map(|b| match reg_dword(NT, "UBR") {
        Some(ubr) => format!("{b}.{ubr}"),
        None => b,
    });
    WindowsSpec {
        edition: reg_sz(NT, "ProductName").map(|p| windows_edition(&p, build_num)),
        version: reg_sz(NT, "DisplayVersion").or_else(|| reg_sz(NT, "ReleaseId")),
        build,
    }
}

fn read_specs() -> RigSpecs {
    let (cpu, memory) = read_wmi();
    RigSpecs {
        cpu,
        memory,
        gpus: Vec::new(),
        board: read_board(),
        drives: read_drives(),
        displays: read_displays(),
        windows: read_windows(),
        read_at: health::now_secs(),
    }
}

/// Read once; the lock also makes a second caller wait for the first read.
fn cached_specs() -> RigSpecs {
    static CACHE: Mutex<Option<RigSpecs>> = Mutex::new(None);
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    guard.get_or_insert_with(read_specs).clone()
}

#[tauri::command]
pub async fn get_rig_specs(state: State<'_, AppState>) -> Result<RigSpecs, String> {
    let luids = gpu::counter_luids(&state);
    let mut specs = tauri::async_runtime::spawn_blocking(cached_specs)
        .await
        .map_err(|_| "Could not read this PC's specs.".to_string())?;
    specs.gpus = tauri::async_runtime::spawn_blocking(gpu::adapters).await.unwrap_or_default();
    specs.gpus = gpu::visible(std::mem::take(&mut specs.gpus), &luids);
    Ok(specs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_kinds() {
        assert_eq!(memory_kind(26), Some("DDR4"));
        assert_eq!(memory_kind(34), Some("DDR5"));
        assert_eq!(memory_kind(35), Some("LPDDR5"));
        assert_eq!(memory_kind(0), None);
    }

    #[test]
    fn windows_11_names() {
        assert_eq!(windows_edition("Windows 10 Pro", Some(22631)), "Windows 11 Pro");
        assert_eq!(windows_edition("Windows 10 Home", Some(19045)), "Windows 10 Home");
        assert_eq!(windows_edition("Windows 10 Pro", None), "Windows 10 Pro");
        assert_eq!(windows_edition("Windows Server 2022", Some(20348)), "Windows Server 2022");
    }

    #[test]
    fn refresh_rates() {
        assert_eq!(refresh_hz(165_000, 1000), Some(165.0));
        assert_eq!(refresh_hz(60_000, 1001), Some(59.94));
        assert_eq!(refresh_hz(0, 0), None);
    }

    /// Reads this PC's specs: never panics, prints them for the report.
    #[test]
    fn reads_real_specs() {
        let s = read_specs();
        if let Some(t) = s.memory.total {
            assert_eq!(t, s.memory.modules.iter().sum::<u64>());
            assert!(s.memory.slots_total.is_none_or(|n| n >= s.memory.slots_used));
        }
        assert!(s.displays.iter().filter(|d| d.primary).count() <= 1);
        println!("{s:#?}");
    }
}
