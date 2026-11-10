//! CPU and GPU temperatures, fan speed, power and clocks (Performance, 0.7,
//! roadmap 9b). Read on a thread of its own, at most once a second, and only
//! while the sampler asks for it (`SensorHub::poll` every tick), so a slow
//! source never holds up a snapshot. The latest reading rides along in
//! `Snapshot.sensors`.
//!
//! Every value says where it came from (`source`) and nothing is guessed: a
//! sensor a source does not report is `None`, and the UI hides it.
//!
//! - GPU: NVIDIA through NVML (`nvml.dll`, installed with the driver), loaded
//!   at run time from System32 or the NVSMI folder, so a PC without it just
//!   has no GPU sensors. Each NVML device is matched to its DXGI adapter
//!   (gpu.rs LUID) by PCI bus and device number (D3DKMT), else by name.
//!   AMD and Intel cards have no reader yet (no vendor library Orbit can load
//!   and test here), so they show no temperature.
//! - CPU, in this order: LibreHardwareMonitor or OpenHardwareMonitor (their
//!   WMI `Sensor` class) or HWiNFO (shared memory `Global\HWiNFO_SENS_SM2`),
//!   when one of them is running; then, only when Orbit is elevated, the ACPI
//!   thermal zone (`MSAcpi_ThermalZoneTemperature`), marked approximate and
//!   only once its value has moved (many boards report a constant).
//!   Orbit never installs, bundles or loads a kernel driver.

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use windows::core::{HSTRING, PCSTR, VARIANT};
use windows::Wdk::Graphics::Direct3D::{
    D3DKMTCloseAdapter, D3DKMTOpenAdapterFromLuid, D3DKMTQueryAdapterInfo, D3DKMT_ADAPTERADDRESS,
    D3DKMT_CLOSEADAPTER, D3DKMT_OPENADAPTERFROMLUID, D3DKMT_QUERYADAPTERINFO,
    KMTQAITYPE_ADAPTERADDRESS,
};
use windows::Win32::Foundation::{CloseHandle, FreeLibrary, HMODULE, LUID};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
    LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
};
use windows::Win32::System::Memory::{
    MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, VirtualQuery, FILE_MAP_READ,
    MEMORY_BASIC_INFORMATION,
};
use windows::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows::Win32::System::Variant::{VT_EMPTY, VT_NULL};
use windows::Win32::UI::Shell::FOLDERID_ProgramFiles;

use crate::health::{prop, variant_string, variant_u64, Wmi};

pub const SRC_NVML: &str = "NVML";
pub const SRC_LHM: &str = "LibreHardwareMonitor";
pub const SRC_OHM: &str = "OpenHardwareMonitor";
pub const SRC_HWINFO: &str = "HWiNFO";
pub const SRC_ACPI: &str = "ACPI";

/// At most one reading a second, however fast the sampler ticks.
const MIN_GAP: Duration = Duration::from_millis(1000);
/// A read still going after this long (a source hung): the last reading is
/// dropped, not shown. Measured from the request, not the reading, so a slow
/// update interval or a long pause never hides a good one.
const STALE: Duration = Duration::from_secs(5);
/// How long `on_exit` waits for the sensor thread to release NVML.
const EXIT_WAIT: Duration = Duration::from_millis(1000);
/// How often to look for a monitoring tool that was not running.
const PROBE_EVERY: Duration = Duration::from_secs(15);
/// WMI wait per row; the tools answer in a few milliseconds.
const WMI_ROW_MS: i32 = 2000;
/// HWiNFO stops updating when its sensor window is paused or closed.
const HWINFO_STALE_SECS: i64 = 60;

// ---------------------------------------------------------------------------
// Shapes (docs/CONTRACT.md "v0.7")
// ---------------------------------------------------------------------------

/// `Snapshot.sensors`: `cpu` is `None` when no source reports a CPU
/// temperature; `gpus` has one entry per GPU a library reads.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Sensors {
    pub cpu: Option<CpuSensors>,
    #[serde(default)]
    pub gpus: Vec<GpuSensors>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CpuSensors {
    /// "LibreHardwareMonitor" | "OpenHardwareMonitor" | "HWiNFO" | "ACPI".
    pub source: String,
    /// True for the ACPI thermal zone: the motherboard's zone, not the die.
    pub approximate: bool,
    /// °C: the package (or Tctl/Tdie), else the hottest core.
    pub package: Option<f64>,
    /// °C per core ("Core 1", "P-core 0") or per CCD ("CCD 1"), source order by index.
    pub cores: Vec<Reading>,
    /// Package power, watts.
    pub power: Option<f64>,
    /// MHz per core, when the source reports them.
    pub clocks: Vec<Reading>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Reading {
    pub name: String,
    pub value: f64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GpuSensors {
    /// The DXGI adapter it belongs to (gpu.rs `luid_key`), `None` if unmatched.
    pub luid: Option<String>,
    /// "NVIDIA GeForce RTX 4060", as NVML names it.
    pub name: String,
    /// "NVML".
    pub source: String,
    /// °C, the GPU core sensor.
    pub temp: Option<f64>,
    /// °C, video memory (NVML field 82; most GeForce cards do not report it).
    pub memory_temp: Option<f64>,
    /// °C at which the card starts slowing itself down.
    pub slowdown_temp: Option<f64>,
    /// Percent of the fan's maximum (`None` on most laptops).
    pub fan_pct: Option<f64>,
    pub power_w: Option<f64>,
    pub power_limit_w: Option<f64>,
    pub core_mhz: Option<f64>,
    pub memory_mhz: Option<f64>,
}

// ---------------------------------------------------------------------------
// Hub: the sampler's side
// ---------------------------------------------------------------------------

#[derive(Default)]
struct HubState {
    requested: bool,
    stop: bool,
    /// The sensor thread has ended (NVML shut down, COM released).
    done: bool,
    /// When the read under way started; `None` between reads.
    busy_since: Option<Instant>,
    latest: Option<Sensors>,
}

impl HubState {
    /// The latest reading, unless the read after it has hung.
    fn current(&self) -> Sensors {
        let hung = self.busy_since.is_some_and(|t| t.elapsed() > STALE);
        match &self.latest {
            Some(s) if !hung => s.clone(),
            _ => Sensors::default(),
        }
    }
}

#[derive(Default)]
struct Shared {
    state: Mutex<HubState>,
    cv: Condvar,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The hub whose thread `on_exit` stops (the sampler's; there is one).
static HUB: OnceLock<Arc<Shared>> = OnceLock::new();

/// Owned by the sampler. The reading thread starts on the first `poll` and
/// ends when the hub is dropped or on `on_exit`.
pub struct SensorHub {
    shared: Arc<Shared>,
    started: bool,
}

impl Default for SensorHub {
    fn default() -> Self {
        Self { shared: Arc::new(Shared::default()), started: false }
    }
}

impl SensorHub {
    /// Asks for a fresh reading (taken at most once a second, on the sensor
    /// thread) and returns the latest one. Never waits for a reading.
    pub fn poll(&mut self) -> Sensors {
        if !self.started {
            self.started = true;
            let shared = Arc::clone(&self.shared);
            let _ = HUB.set(Arc::clone(&self.shared));
            if let Err(err) = std::thread::Builder::new()
                .name("orbit-sensors".into())
                .spawn(move || run(shared))
            {
                eprintln!("Orbit could not start the sensor thread: {err}");
            }
        }
        let out = {
            let mut g = lock(&self.shared.state);
            g.requested = true;
            g.current()
        };
        self.shared.cv.notify_all();
        out
    }
}

impl Drop for SensorHub {
    fn drop(&mut self) {
        lock(&self.shared.state).stop = true;
        self.shared.cv.notify_all();
    }
}

/// Stops the sensor thread and waits (briefly) for it to shut NVML down on
/// its own thread, so no NVML call is cut off. Safe to repeat (lib.rs
/// `exit_cleanup`); the sampler itself never ends.
pub fn on_exit() {
    let Some(shared) = HUB.get() else { return };
    let deadline = Instant::now() + EXIT_WAIT;
    let mut g = lock(&shared.state);
    g.stop = true;
    shared.cv.notify_all();
    while !g.done {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        g = match shared.cv.wait_timeout(g, left) {
            Ok((g, _)) => g,
            Err(e) => e.into_inner().0,
        };
    }
}

fn run(shared: Arc<Shared>) {
    {
        let _com = crate::health::com_init();
        // Dropped before COM: WMI released, NVML shut down.
        let mut reader = Reader::default();
        read_loop(&shared, &mut reader);
    }
    lock(&shared.state).done = true;
    shared.cv.notify_all();
}

fn read_loop(shared: &Shared, reader: &mut Reader) {
    let mut last: Option<Instant> = None;
    loop {
        let mut g = lock(&shared.state);
        while !g.requested && !g.stop {
            g = shared.cv.wait(g).unwrap_or_else(|e| e.into_inner());
        }
        // At most one reading a second; a stop still ends the wait.
        while !g.stop {
            let left = last.map_or(Duration::ZERO, |at| MIN_GAP.saturating_sub(at.elapsed()));
            if left.is_zero() {
                break;
            }
            g = match shared.cv.wait_timeout(g, left) {
                Ok((g, _)) => g,
                Err(e) => e.into_inner().0,
            };
        }
        if g.stop {
            return;
        }
        g.requested = false;
        g.busy_since = Some(Instant::now());
        drop(g);
        let sensors = reader.read();
        last = Some(Instant::now());
        let mut g = lock(&shared.state);
        g.latest = Some(sensors);
        g.busy_since = None;
    }
}

// ---------------------------------------------------------------------------
// Reader: the sensor thread's side
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Reader {
    nvml: Option<Nvml>,
    nvml_tried: bool,
    cpu: CpuReader,
}

impl Reader {
    fn read(&mut self) -> Sensors {
        if !self.nvml_tried {
            // Once per run: a missing nvml.dll stays missing until a driver
            // install, which restarts the display stack anyway.
            self.nvml_tried = true;
            self.nvml = Nvml::load();
        }
        Sensors {
            cpu: self.cpu.read(),
            gpus: self.nvml.as_ref().map(Nvml::read).unwrap_or_default(),
        }
    }
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// A temperature worth showing: a finite number between 0 and 150 °C
/// (tools write 0, -1 or NaN for a sensor that is not there).
fn plausible_temp(c: f64) -> Option<f64> {
    (c.is_finite() && c > 0.0 && c < 150.0).then(|| round1(c))
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn is_admin() -> bool {
    unsafe { windows::Win32::UI::Shell::IsUserAnAdmin() }.as_bool()
}

// ---------------------------------------------------------------------------
// CPU: shared parsing for the monitoring tools
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Temp,
    Power,
    Clock,
}

/// One CPU reading from a tool, before it is sorted into `CpuSensors`.
#[derive(Clone, Debug, PartialEq)]
struct Item {
    kind: Kind,
    label: String,
    value: f64,
}

/// Package-level temperature names, best first (LHM, OHM and HWiNFO spellings).
const PACKAGE_NAMES: &[&str] = &[
    "cpu package",
    "core (tctl/tdie)",
    "cpu (tctl/tdie)",
    "core (tdie)",
    "cpu (tdie)",
    "core (tctl)",
    "cpu (tctl)",
    "package",
    "cpu die (average)",
    "core max",
    "cpu core max",
];

const POWER_NAMES: &[&str] = &["cpu package", "package", "cpu package power", "package power"];

/// "Core #3", "CPU Core #3", "Core 3", "P-core 0", "E-core 4" -> (order, "Core 3").
/// Anything after the number ("Core #1 Distance to TjMax") is not a core.
fn core_label(raw: &str) -> Option<(u32, String)> {
    let s = raw.trim();
    let s = s.strip_prefix("CPU ").unwrap_or(s);
    let lower = s.to_ascii_lowercase();
    let (prefix, order, rest) = if let Some(r) = lower.strip_prefix("p-core") {
        ("P-core", 0, r)
    } else if let Some(r) = lower.strip_prefix("e-core") {
        ("E-core", 1, r)
    } else if let Some(r) = lower.strip_prefix("core") {
        ("Core", 0, r)
    } else {
        return None;
    };
    let digits = rest.trim().trim_start_matches('#').trim();
    if digits.is_empty() || digits.len() > 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u32 = digits.parse().ok()?;
    Some((order * 10_000 + n, format!("{prefix} {n}")))
}

/// "CCD1 (Tdie)", "CPU CCD2 (Tdie)", "CCD 1" -> (index, "CCD 1"). Not "CCDs Max".
fn ccd_label(raw: &str) -> Option<(u32, String)> {
    let upper = raw.to_ascii_uppercase();
    let at = upper.find("CCD")?;
    let rest = upper[at + 3..].trim_start();
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let n: u32 = digits.parse().ok()?;
    Some((n, format!("CCD {n}")))
}

/// HWiNFO clock labels: "Core #0 Clock", "P-core 0 Clock (perf #1/1)".
fn clock_core_label(raw: &str) -> Option<(u32, String)> {
    let at = raw.find(" Clock")?;
    core_label(&raw[..at])
}

fn sorted(mut v: Vec<(u32, String, f64)>) -> Vec<Reading> {
    v.sort_by_key(|(k, _, _)| *k);
    let mut seen = HashSet::new();
    v.into_iter()
        .filter(|(_, name, _)| seen.insert(name.clone()))
        .map(|(_, name, value)| Reading { name, value })
        .collect()
}

/// Sorts a tool's CPU readings into package, per-core temps, power and clocks.
/// `None` without any temperature: then the tool tells nothing Orbit shows.
fn cpu_from_items(items: &[Item], source: &str) -> Option<CpuSensors> {
    let named = |kind: Kind, names: &[&str]| -> Option<f64> {
        names.iter().find_map(|want| {
            items
                .iter()
                .find(|i| i.kind == kind && i.label.trim().eq_ignore_ascii_case(want))
                .map(|i| i.value)
        })
    };
    let temps: Vec<&Item> = items.iter().filter(|i| i.kind == Kind::Temp).collect();
    let mut cores: Vec<(u32, String, f64)> = Vec::new();
    for t in &temps {
        if let (Some((k, name)), Some(v)) = (core_label(&t.label), plausible_temp(t.value)) {
            cores.push((k, name, v));
        }
    }
    if cores.is_empty() {
        // AMD: per-CCD temperatures stand in for cores.
        for t in &temps {
            let l = t.label.to_ascii_lowercase();
            if l.contains("ccds") || l.contains("max") || l.contains("average") {
                continue;
            }
            if let (Some((k, name)), Some(v)) = (ccd_label(&t.label), plausible_temp(t.value)) {
                cores.push((k, name, v));
            }
        }
    }
    let cores = sorted(cores);
    let package = named(Kind::Temp, PACKAGE_NAMES)
        .and_then(plausible_temp)
        .or_else(|| cores.iter().map(|c| c.value).reduce(f64::max));
    if package.is_none() {
        return None;
    }
    let power = named(Kind::Power, POWER_NAMES).filter(|w| w.is_finite() && *w >= 0.0).map(round1);
    let mut clocks: Vec<(u32, String, f64)> = Vec::new();
    for c in items.iter().filter(|i| i.kind == Kind::Clock) {
        let label = core_label(&c.label).or_else(|| clock_core_label(&c.label));
        if let Some((k, name)) = label {
            if c.value.is_finite() && c.value > 0.0 {
                clocks.push((k, name, c.value.round()));
            }
        }
    }
    Some(CpuSensors {
        source: source.to_string(),
        approximate: false,
        package,
        cores,
        power,
        clocks: sorted(clocks),
    })
}

// ---------------------------------------------------------------------------
// CPU: LibreHardwareMonitor / OpenHardwareMonitor (WMI)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
enum WmiTool {
    Libre,
    Open,
}

impl WmiTool {
    fn namespace(self) -> &'static str {
        match self {
            WmiTool::Libre => r"root\LibreHardwareMonitor",
            WmiTool::Open => r"root\OpenHardwareMonitor",
        }
    }

    fn source(self) -> &'static str {
        match self {
            WmiTool::Libre => SRC_LHM,
            WmiTool::Open => SRC_OHM,
        }
    }
}

/// One row of the tools' `Sensor` class.
#[derive(Clone, Debug, PartialEq)]
struct ToolRow {
    name: String,
    /// "Temperature" | "Clock" | "Power" | ...
    sensor_type: String,
    value: Option<f64>,
    /// Hardware identifier: "/intelcpu/0", "/amdcpu/0", "/gpu-nvidia/0", ...
    parent: String,
}

/// "/intelcpu/0", "/amdcpu/0", "/cpu/0": the tools' CPU hardware.
fn is_cpu_hardware(parent: &str) -> bool {
    parent.split('/').nth(1).is_some_and(|seg| seg.to_ascii_lowercase().ends_with("cpu"))
}

/// The first CPU's readings (a second socket would repeat the core names).
fn cpu_from_tool_rows(rows: &[ToolRow], source: &str) -> Option<CpuSensors> {
    let first = rows.iter().filter(|r| is_cpu_hardware(&r.parent)).map(|r| r.parent.as_str()).min()?;
    let items: Vec<Item> = rows
        .iter()
        .filter(|r| r.parent == first)
        .filter_map(|r| {
            let kind = match r.sensor_type.as_str() {
                "Temperature" => Kind::Temp,
                "Power" => Kind::Power,
                "Clock" => Kind::Clock,
                _ => return None,
            };
            Some(Item { kind, label: r.name.clone(), value: r.value? })
        })
        .collect();
    cpu_from_items(&items, source)
}

fn variant_f64(v: &VARIANT) -> Option<f64> {
    let vt = unsafe { v.as_raw().Anonymous.Anonymous.vt };
    if vt == VT_EMPTY.0 || vt == VT_NULL.0 {
        return None;
    }
    f64::try_from(v).ok().filter(|x| x.is_finite())
}

const TOOL_QUERY: &str = "SELECT Name, SensorType, Value, Parent FROM Sensor WHERE \
    (SensorType='Temperature' OR SensorType='Clock' OR SensorType='Power')";

fn read_tool(wmi: &Wmi, tool: WmiTool) -> Option<CpuSensors> {
    // CPU rows only where WQL's LIKE works (keeps a big PC under the row cap);
    // the plain query otherwise. Rust filters by Parent either way.
    let narrow = format!("{TOOL_QUERY} AND Parent LIKE '%cpu/%'");
    [narrow.as_str(), TOOL_QUERY]
        .iter()
        .find_map(|wql| cpu_from_tool_rows(&tool_rows(&wmi.query_timeout(wql, WMI_ROW_MS).ok()?), tool.source()))
}

fn tool_rows(rows: &[windows::Win32::System::Wmi::IWbemClassObject]) -> Vec<ToolRow> {
    rows
        .iter()
        .map(|o| ToolRow {
            name: variant_string(&prop(o, "Name")).unwrap_or_default(),
            sensor_type: variant_string(&prop(o, "SensorType")).unwrap_or_default(),
            value: variant_f64(&prop(o, "Value")),
            parent: variant_string(&prop(o, "Parent")).unwrap_or_default(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// CPU: HWiNFO shared memory
// ---------------------------------------------------------------------------

const HWINFO_MAP: &str = r"Global\HWiNFO_SENS_SM2";
const HWINFO_HEADER: usize = 44;
const HWINFO_SENSOR_MIN: usize = 264; // ids (8) + two 128-byte names
const HWINFO_READING_MIN: usize = 316; // 3 dwords + labels (256) + unit (16) + 4 doubles
const HWINFO_MAX_BYTES: usize = 16 << 20;
const HWINFO_TEMP: u32 = 1;
const HWINFO_POWER: u32 = 5;
const HWINFO_CLOCK: u32 = 6;

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn i64_at(b: &[u8], at: usize) -> Option<i64> {
    Some(i64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

fn f64_at(b: &[u8], at: usize) -> Option<f64> {
    Some(f64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// A NUL-terminated ANSI string (HWiNFO writes the °C sign as 0xB0).
fn ansi(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    b[..end].iter().map(|&c| c as char).collect::<String>().trim().to_string()
}

/// Bytes the header says the sections need (header included).
fn hwinfo_len(b: &[u8]) -> Option<usize> {
    let span = |off: usize, size: usize, n: usize| off.checked_add(size.checked_mul(n)?);
    let s = span(u32_at(b, 20)? as usize, u32_at(b, 24)? as usize, u32_at(b, 28)? as usize)?;
    let r = span(u32_at(b, 32)? as usize, u32_at(b, 36)? as usize, u32_at(b, 40)? as usize)?;
    Some(s.max(r).max(HWINFO_HEADER))
}

/// Parses HWiNFO's sensor shared memory (SDK layout, packed). `now`: Unix
/// seconds, to drop a reading HWiNFO stopped updating.
fn parse_hwinfo(b: &[u8], now: i64) -> Option<CpuSensors> {
    if b.get(0..4)? != b"HWiS" {
        return None; // "DEAD" once HWiNFO stops sharing
    }
    let poll = i64_at(b, 12)?;
    if poll > 0 && (now - poll).abs() > HWINFO_STALE_SECS {
        return None;
    }
    let (s_off, s_size, s_num) = (u32_at(b, 20)? as usize, u32_at(b, 24)? as usize, u32_at(b, 28)? as usize);
    let (r_off, r_size, r_num) = (u32_at(b, 32)? as usize, u32_at(b, 36)? as usize, u32_at(b, 40)? as usize);
    if s_size < HWINFO_SENSOR_MIN || r_size < HWINFO_READING_MIN || s_num > 4096 || r_num > 65_536 {
        return None;
    }
    if hwinfo_len(b)? > b.len() {
        return None;
    }
    // Sensors of the first CPU: "CPU [#0]: Intel Core i5-12400F: DTS", "...: Enhanced".
    let cpu_sensors: HashSet<u32> = (0..s_num)
        .filter(|i| {
            let base = s_off + i * s_size;
            ansi(&b[base + 8..base + 136]).to_ascii_uppercase().starts_with("CPU [#0]")
        })
        .map(|i| i as u32)
        .collect();
    let mut items = Vec::new();
    for i in 0..r_num {
        let base = r_off + i * r_size;
        let kind = match u32_at(b, base)? {
            HWINFO_TEMP => Kind::Temp,
            HWINFO_POWER => Kind::Power,
            HWINFO_CLOCK => Kind::Clock,
            _ => continue,
        };
        if !cpu_sensors.contains(&u32_at(b, base + 4)?) {
            continue;
        }
        let label = ansi(&b[base + 12..base + 140]);
        let unit = ansi(&b[base + 268..base + 284]);
        let mut value = f64_at(b, base + 284)?;
        if kind == Kind::Temp && unit.ends_with('F') {
            value = (value - 32.0) * 5.0 / 9.0;
        }
        items.push(Item { kind, label, value });
    }
    cpu_from_items(&items, SRC_HWINFO)
}

/// A copy of HWiNFO's shared memory, `None` when HWiNFO is not sharing.
fn map_hwinfo() -> Option<Vec<u8>> {
    unsafe {
        let handle = OpenFileMappingW(FILE_MAP_READ.0, false, &HSTRING::from(HWINFO_MAP)).ok()?;
        let view = MapViewOfFile(handle, FILE_MAP_READ, 0, 0, 0);
        let mut out = None;
        if !view.Value.is_null() {
            let mut mbi = MEMORY_BASIC_INFORMATION::default();
            let got = VirtualQuery(Some(view.Value), &mut mbi, std::mem::size_of::<MEMORY_BASIC_INFORMATION>());
            let region = if got == 0 { 0 } else { mbi.RegionSize };
            if region >= HWINFO_HEADER {
                let base = view.Value as *const u8;
                let header = std::slice::from_raw_parts(base, HWINFO_HEADER);
                if let Some(len) = hwinfo_len(header) {
                    if len <= region && len <= HWINFO_MAX_BYTES {
                        out = Some(std::slice::from_raw_parts(base, len).to_vec());
                    }
                }
            }
            let _ = UnmapViewOfFile(view);
        }
        let _ = CloseHandle(handle);
        out
    }
}

fn read_hwinfo() -> Option<CpuSensors> {
    parse_hwinfo(&map_hwinfo()?, now_secs())
}

// ---------------------------------------------------------------------------
// CPU: ACPI thermal zone (elevated only)
// ---------------------------------------------------------------------------

/// Tenths of a kelvin (ACPI `_TMP`) to °C, `None` when out of range.
fn acpi_celsius(tenths_k: u64) -> Option<f64> {
    plausible_temp(tenths_k as f64 / 10.0 - 273.15)
}

/// Remembers each zone's first value; a zone counts once it has moved.
#[derive(Default, Debug)]
struct AcpiZones {
    first: HashMap<String, u64>,
    moved: HashSet<String>,
}

impl AcpiZones {
    /// The hottest zone that has changed at least once, in °C.
    fn update(&mut self, zones: &[(String, u64)]) -> Option<f64> {
        for (name, raw) in zones {
            let first = *self.first.entry(name.clone()).or_insert(*raw);
            if first != *raw {
                self.moved.insert(name.clone());
            }
        }
        zones
            .iter()
            .filter(|(name, _)| self.moved.contains(name))
            .filter_map(|(_, raw)| acpi_celsius(*raw))
            .reduce(f64::max)
    }
}

#[derive(Default)]
struct AcpiReader {
    wmi: Option<Wmi>,
    /// Connect or query failed: not tried again this run.
    failed: bool,
    zones: AcpiZones,
}

impl AcpiReader {
    fn read(&mut self) -> Option<CpuSensors> {
        if self.failed {
            return None;
        }
        if self.wmi.is_none() {
            match Wmi::connect(r"root\WMI") {
                Ok(w) => self.wmi = Some(w),
                Err(_) => {
                    self.failed = true;
                    return None;
                }
            }
        }
        let rows = match self.wmi.as_ref()?.query_timeout(
            "SELECT InstanceName, CurrentTemperature FROM MSAcpi_ThermalZoneTemperature",
            WMI_ROW_MS,
        ) {
            Ok(r) if !r.is_empty() => r,
            _ => {
                self.failed = true;
                self.wmi = None;
                return None;
            }
        };
        let zones: Vec<(String, u64)> = rows
            .iter()
            .filter_map(|o| {
                Some((
                    variant_string(&prop(o, "InstanceName")).unwrap_or_default(),
                    variant_u64(&prop(o, "CurrentTemperature"))?,
                ))
            })
            .collect();
        let c = self.zones.update(&zones)?;
        Some(CpuSensors {
            source: SRC_ACPI.into(),
            approximate: true,
            package: Some(c),
            cores: Vec::new(),
            power: None,
            clocks: Vec::new(),
        })
    }
}

// ---------------------------------------------------------------------------
// CPU: picking a source
// ---------------------------------------------------------------------------

#[derive(Default)]
struct CpuReader {
    tool: Option<(WmiTool, Wmi)>,
    next_probe: Option<Instant>,
    acpi: Option<AcpiReader>,
    admin: Option<bool>,
}

impl CpuReader {
    fn read(&mut self) -> Option<CpuSensors> {
        if let Some((tool, wmi)) = &self.tool {
            if let Some(c) = read_tool(wmi, *tool) {
                return Some(c);
            }
            // The tool closed: look again on the next probe.
            self.tool = None;
        }
        if let Some(c) = read_hwinfo() {
            return Some(c);
        }
        let now = Instant::now();
        if self.next_probe.map_or(true, |t| now >= t) {
            self.next_probe = Some(now + PROBE_EVERY);
            for tool in [WmiTool::Libre, WmiTool::Open] {
                if let Ok(wmi) = Wmi::connect(tool.namespace()) {
                    if let Some(c) = read_tool(&wmi, tool) {
                        self.tool = Some((tool, wmi));
                        return Some(c);
                    }
                }
            }
        }
        if *self.admin.get_or_insert_with(is_admin) {
            return self.acpi.get_or_insert_with(AcpiReader::default).read();
        }
        None
    }
}

// ---------------------------------------------------------------------------
// GPU: NVML
// ---------------------------------------------------------------------------

type NvmlDevice = *mut c_void;
type FnInit = unsafe extern "C" fn() -> i32;
type FnCount = unsafe extern "C" fn(*mut u32) -> i32;
type FnHandle = unsafe extern "C" fn(u32, *mut NvmlDevice) -> i32;
type FnName = unsafe extern "C" fn(NvmlDevice, *mut u8, u32) -> i32;
type FnPci = unsafe extern "C" fn(NvmlDevice, *mut NvmlPciInfo) -> i32;
type FnU32 = unsafe extern "C" fn(NvmlDevice, *mut u32) -> i32;
type FnKindU32 = unsafe extern "C" fn(NvmlDevice, u32, *mut u32) -> i32;
type FnFields = unsafe extern "C" fn(NvmlDevice, i32, *mut NvmlFieldValue) -> i32;

const NVML_SUCCESS: i32 = 0;
const NVML_TEMPERATURE_GPU: u32 = 0;
const NVML_TEMPERATURE_THRESHOLD_SLOWDOWN: u32 = 1;
const NVML_CLOCK_GRAPHICS: u32 = 0;
const NVML_CLOCK_MEM: u32 = 2;
const NVML_FI_DEV_MEMORY_TEMP: u32 = 82;
const NVML_NAME_LEN: usize = 96;
const NVML_MAX_DEVICES: u32 = 16;

/// `nvmlPciInfo_t` (v2 and v3 share this layout).
#[repr(C)]
#[derive(Clone, Copy)]
struct NvmlPciInfo {
    bus_id_legacy: [u8; 16],
    domain: u32,
    bus: u32,
    device: u32,
    pci_device_id: u32,
    pci_sub_system_id: u32,
    bus_id: [u8; 32],
}

/// `nvmlFieldValue_t`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct NvmlFieldValue {
    field_id: u32,
    scope_id: u32,
    timestamp: i64,
    latency_usec: i64,
    value_type: i32,
    nvml_return: i32,
    value: u64,
}

/// An NVML return code and value: the value only on success.
fn nv_value(ret: i32, raw: u32, scale: f64) -> Option<f64> {
    (ret == NVML_SUCCESS).then(|| raw as f64 * scale)
}

/// `nvmlValue_t` by `nvmlValueType_t` (0 double, 1 uint, 2 ulong, 3 ulonglong,
/// 4 slonglong, 5 sint, 6 ushort).
fn field_number(value_type: i32, raw: u64) -> Option<f64> {
    let v = match value_type {
        0 => f64::from_bits(raw),
        1 | 2 => (raw as u32) as f64,
        3 => raw as f64,
        4 => raw as i64 as f64,
        5 => (raw as u32) as i32 as f64,
        6 => (raw as u16) as f64,
        _ => return None,
    };
    v.is_finite().then_some(v)
}

/// "0x00000000_0x0000d1a5" (gpu::luid_key) -> (high, low).
fn parse_luid(key: &str) -> Option<(u32, u32)> {
    let (h, l) = key.split_once('_')?;
    let hex = |s: &str| u32::from_str_radix(s.strip_prefix("0x")?, 16).ok();
    Some((hex(h)?, hex(l)?))
}

/// PCI bus and device number of a DXGI adapter, through D3DKMT (no admin).
fn adapter_address(luid_key: &str) -> Option<(u32, u32)> {
    let (high, low) = parse_luid(luid_key)?;
    let mut open = D3DKMT_OPENADAPTERFROMLUID {
        AdapterLuid: LUID { LowPart: low, HighPart: high as i32 },
        hAdapter: 0,
    };
    if unsafe { D3DKMTOpenAdapterFromLuid(&mut open) }.is_err() {
        return None;
    }
    let mut addr = D3DKMT_ADAPTERADDRESS::default();
    let mut query = D3DKMT_QUERYADAPTERINFO {
        hAdapter: open.hAdapter,
        Type: KMTQAITYPE_ADAPTERADDRESS,
        pPrivateDriverData: &mut addr as *mut _ as *mut c_void,
        PrivateDriverDataSize: std::mem::size_of::<D3DKMT_ADAPTERADDRESS>() as u32,
    };
    let ok = unsafe { D3DKMTQueryAdapterInfo(&mut query) }.is_ok();
    let close = D3DKMT_CLOSEADAPTER { hAdapter: open.hAdapter };
    let _ = unsafe { D3DKMTCloseAdapter(&close) };
    ok.then_some((addr.BusNumber, addr.DeviceNumber))
}

/// What matching needs to know about one NVML device or DXGI adapter.
#[derive(Clone, Debug, PartialEq)]
struct GpuKey {
    name: String,
    /// (PCI bus, device), `None` when unknown.
    addr: Option<(u32, u32)>,
    /// DXGI side only.
    luid: String,
}

/// The DXGI LUID for each NVML device: same PCI bus and device first; else a
/// name only one NVML device has. Two unaddressed cards of one name stay
/// unmatched rather than risk swapping their readings.
fn match_gpus(nv: &[GpuKey], dx: &[GpuKey]) -> Vec<Option<String>> {
    let mut used: HashSet<&str> = HashSet::new();
    let mut out: Vec<Option<String>> = vec![None; nv.len()];
    for (i, n) in nv.iter().enumerate() {
        if let Some(addr) = n.addr {
            if let Some(d) = dx.iter().find(|d| d.addr == Some(addr) && !used.contains(d.luid.as_str())) {
                used.insert(&d.luid);
                out[i] = Some(d.luid.clone());
            }
        }
    }
    let same = |a: &str, b: &str| a.trim().eq_ignore_ascii_case(b.trim());
    for (i, n) in nv.iter().enumerate() {
        if out[i].is_some() || nv.iter().filter(|m| same(&m.name, &n.name)).count() != 1 {
            continue;
        }
        if let Some(d) = dx.iter().find(|d| same(&d.name, &n.name) && !used.contains(d.luid.as_str())) {
            used.insert(&d.luid);
            out[i] = Some(d.luid.clone());
        }
    }
    out
}

struct NvDevice {
    handle: NvmlDevice,
    name: String,
    luid: Option<String>,
    addr: Option<(u32, u32)>,
    slowdown: Option<f64>,
}

struct Nvml {
    lib: HMODULE,
    /// Where nvml.dll was loaded from (printed by the `real_sensors` test).
    #[allow(dead_code)]
    path: PathBuf,
    shutdown: FnInit,
    temperature: FnKindU32,
    fan: Option<FnU32>,
    power: Option<FnU32>,
    limit: Option<FnU32>,
    clock: Option<FnKindU32>,
    fields: Option<FnFields>,
    devices: Vec<NvDevice>,
}

impl Drop for Nvml {
    fn drop(&mut self) {
        unsafe {
            (self.shutdown)();
            let _ = FreeLibrary(self.lib);
        }
    }
}

/// System32 first (current drivers), then the NVSMI folder (older ones).
/// Both from Windows itself (no environment variable, no search path), so
/// nothing a user-level process sets can point Orbit at another nvml.dll.
fn nvml_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut buf = [0u16; 260];
    let n = unsafe { GetSystemDirectoryW(Some(&mut buf)) } as usize;
    if n > 0 && n < buf.len() {
        out.push(PathBuf::from(String::from_utf16_lossy(&buf[..n])).join("nvml.dll"));
    }
    if let Some(pf) = crate::cleanup::known_folder(&FOLDERID_ProgramFiles) {
        out.push(pf.join(r"NVIDIA Corporation\NVSMI\nvml.dll"));
    }
    out
}

fn c_name(buf: &[u8]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).trim().to_string()
}

impl Nvml {
    fn load() -> Option<Nvml> {
        for path in nvml_paths() {
            if !path.is_file() {
                continue;
            }
            let flags = LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS;
            let Ok(lib) = (unsafe { LoadLibraryExW(&HSTRING::from(path.as_os_str()), None, flags) }) else {
                continue;
            };
            match unsafe { Self::init(lib, path.clone()) } {
                Some(n) => return Some(n),
                None => unsafe {
                    let _ = FreeLibrary(lib);
                },
            }
        }
        None
    }

    unsafe fn sym<T: Copy>(lib: HMODULE, name: &[u8]) -> Option<T> {
        debug_assert_eq!(name.last(), Some(&0));
        let f = GetProcAddress(lib, PCSTR(name.as_ptr()))?;
        // SAFETY: T is one of the fn pointer types above, matching nvml.h.
        Some(std::mem::transmute_copy(&f))
    }

    unsafe fn init(lib: HMODULE, path: PathBuf) -> Option<Nvml> {
        let init: FnInit = Self::sym(lib, b"nvmlInit_v2\0")?;
        let shutdown: FnInit = Self::sym(lib, b"nvmlShutdown\0")?;
        let count: FnCount = Self::sym(lib, b"nvmlDeviceGetCount_v2\0")?;
        let handle: FnHandle = Self::sym(lib, b"nvmlDeviceGetHandleByIndex_v2\0")?;
        let temperature: FnKindU32 = Self::sym(lib, b"nvmlDeviceGetTemperature\0")?;
        if init() != NVML_SUCCESS {
            return None;
        }
        let name_fn: Option<FnName> = Self::sym(lib, b"nvmlDeviceGetName\0");
        let pci: Option<FnPci> =
            Self::sym(lib, b"nvmlDeviceGetPciInfo_v3\0").or_else(|| Self::sym(lib, b"nvmlDeviceGetPciInfo_v2\0"));
        let threshold: Option<FnKindU32> = Self::sym(lib, b"nvmlDeviceGetTemperatureThreshold\0");
        let mut n = 0u32;
        let mut devices = Vec::new();
        if count(&mut n) == NVML_SUCCESS {
            for i in 0..n.min(NVML_MAX_DEVICES) {
                let mut h: NvmlDevice = std::ptr::null_mut();
                if handle(i, &mut h) != NVML_SUCCESS || h.is_null() {
                    continue;
                }
                let mut buf = [0u8; NVML_NAME_LEN];
                let name = match name_fn {
                    Some(f) if f(h, buf.as_mut_ptr(), buf.len() as u32) == NVML_SUCCESS => c_name(&buf),
                    _ => String::new(),
                };
                let addr = pci.and_then(|f| {
                    let mut info: NvmlPciInfo = std::mem::zeroed();
                    (f(h, &mut info) == NVML_SUCCESS).then_some((info.bus, info.device))
                });
                let slowdown = threshold.and_then(|f| {
                    let mut t = 0u32;
                    nv_value(f(h, NVML_TEMPERATURE_THRESHOLD_SLOWDOWN, &mut t), t, 1.0).and_then(plausible_temp)
                });
                devices.push(NvDevice { handle: h, name, luid: None, addr, slowdown });
            }
        }
        let nv: Vec<GpuKey> = devices
            .iter()
            .map(|d| GpuKey { name: d.name.clone(), addr: d.addr, luid: String::new() })
            .collect();
        let dx: Vec<GpuKey> = crate::gpu::adapters()
            .into_iter()
            .filter(|a| a.vendor.as_deref() == Some("NVIDIA"))
            .map(|a| GpuKey { addr: adapter_address(&a.luid), name: a.name, luid: a.luid })
            .collect();
        for (d, luid) in devices.iter_mut().zip(match_gpus(&nv, &dx)) {
            d.luid = luid;
        }
        Some(Nvml {
            lib,
            path,
            shutdown,
            temperature,
            fan: Self::sym(lib, b"nvmlDeviceGetFanSpeed\0"),
            power: Self::sym(lib, b"nvmlDeviceGetPowerUsage\0"),
            limit: Self::sym(lib, b"nvmlDeviceGetEnforcedPowerLimit\0"),
            clock: Self::sym(lib, b"nvmlDeviceGetClockInfo\0"),
            fields: Self::sym(lib, b"nvmlDeviceGetFieldValues\0"),
            devices,
        })
    }

    fn read(&self) -> Vec<GpuSensors> {
        self.devices.iter().map(|d| self.read_device(d)).collect()
    }

    fn read_device(&self, d: &NvDevice) -> GpuSensors {
        let h = d.handle;
        let u32_of = |f: Option<FnU32>, scale: f64| {
            f.and_then(|f| {
                let mut v = 0u32;
                nv_value(unsafe { f(h, &mut v) }, v, scale)
            })
        };
        let kind_of = |f: Option<FnKindU32>, kind: u32| {
            f.and_then(|f| {
                let mut v = 0u32;
                nv_value(unsafe { f(h, kind, &mut v) }, v, 1.0)
            })
        };
        let memory_temp = self.fields.and_then(|f| {
            let mut fv = NvmlFieldValue { field_id: NVML_FI_DEV_MEMORY_TEMP, ..Default::default() };
            let ok = unsafe { f(h, 1, &mut fv) } == NVML_SUCCESS && fv.nvml_return == NVML_SUCCESS;
            ok.then(|| field_number(fv.value_type, fv.value)).flatten().and_then(plausible_temp)
        });
        GpuSensors {
            luid: d.luid.clone(),
            name: d.name.clone(),
            source: SRC_NVML.into(),
            temp: kind_of(Some(self.temperature), NVML_TEMPERATURE_GPU).and_then(plausible_temp),
            memory_temp,
            slowdown_temp: d.slowdown,
            fan_pct: u32_of(self.fan, 1.0),
            power_w: u32_of(self.power, 0.001).map(round1),
            power_limit_w: u32_of(self.limit, 0.001).map(round1).filter(|w| *w > 0.0),
            core_mhz: kind_of(self.clock, NVML_CLOCK_GRAPHICS),
            memory_mhz: kind_of(self.clock, NVML_CLOCK_MEM),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, kind: &str, value: f64, parent: &str) -> ToolRow {
        ToolRow { name: name.into(), sensor_type: kind.into(), value: Some(value), parent: parent.into() }
    }

    #[test]
    fn nvml_values() {
        assert_eq!(nv_value(0, 62, 1.0), Some(62.0));
        assert_eq!(nv_value(0, 115_250, 0.001).map(round1), Some(115.3)); // mW -> W
        assert_eq!(nv_value(3, 40, 1.0), None); // NVML_ERROR_NOT_SUPPORTED
        assert_eq!(nv_value(999, 40, 1.0), None);
        assert_eq!(field_number(1, 68), Some(68.0));
        assert_eq!(field_number(0, 71.5f64.to_bits()), Some(71.5));
        assert_eq!(field_number(5, (-3i32) as u32 as u64), Some(-3.0));
        assert_eq!(field_number(9, 1), None);
        assert_eq!(c_name(b"NVIDIA GeForce RTX 4060\0\0junk"), "NVIDIA GeForce RTX 4060");
        // nvml.h struct sizes.
        assert_eq!(std::mem::size_of::<NvmlPciInfo>(), 68);
        assert_eq!(std::mem::size_of::<NvmlFieldValue>(), 40);
        assert_eq!(parse_luid("0x00000000_0x0000d1a5"), Some((0, 0xd1a5)));
        assert_eq!(parse_luid("0x00000001_0xffffffff"), Some((1, 0xffff_ffff)));
        assert_eq!(parse_luid("nope"), None);
    }

    #[test]
    fn matches_nvml_to_dxgi() {
        let k = |name: &str, addr: Option<(u32, u32)>, luid: &str| GpuKey { name: name.into(), addr, luid: luid.into() };
        // By PCI address, even when names differ in spelling.
        let nv = vec![k("NVIDIA GeForce RTX 4060", Some((1, 0)), "")];
        let dx = vec![k("NVIDIA GeForce RTX 4060 ", Some((1, 0)), "A"), k("Virtual", Some((7, 0)), "B")];
        assert_eq!(match_gpus(&nv, &dx), [Some("A".to_string())]);
        // No address: a unique name still matches.
        let nv = vec![k("RTX 4060", None, "")];
        let dx = vec![k("rtx 4060", None, "A")];
        assert_eq!(match_gpus(&nv, &dx), [Some("A".to_string())]);
        // Two identical cards: addresses keep them apart...
        let nv = vec![k("RTX", Some((2, 0)), ""), k("RTX", Some((1, 0)), "")];
        let dx = vec![k("RTX", Some((1, 0)), "A"), k("RTX", Some((2, 0)), "B")];
        assert_eq!(match_gpus(&nv, &dx), [Some("B".to_string()), Some("A".to_string())]);
        // ...and without them neither is guessed.
        let nv = vec![k("RTX", None, ""), k("RTX", None, "")];
        let dx = vec![k("RTX", None, "A"), k("RTX", None, "B")];
        assert_eq!(match_gpus(&nv, &dx), [None, None]);
        // No DXGI adapter at all.
        assert_eq!(match_gpus(&[k("RTX", Some((1, 0)), "")], &[]), [None]);
    }

    #[test]
    fn core_labels() {
        assert_eq!(core_label("Core #3"), Some((3, "Core 3".into())));
        assert_eq!(core_label("CPU Core #3"), Some((3, "Core 3".into())));
        assert_eq!(core_label("Core 12"), Some((12, "Core 12".into())));
        assert_eq!(core_label("P-core 0"), Some((0, "P-core 0".into())));
        assert_eq!(core_label("E-core 4"), Some((10_004, "E-core 4".into())));
        assert_eq!(core_label("Core #1 Distance to TjMax"), None);
        assert_eq!(core_label("Core Max"), None);
        assert_eq!(core_label("Core (Tctl/Tdie)"), None);
        assert_eq!(clock_core_label("Core 0 Clock (perf #1/1)"), Some((0, "Core 0".into())));
        assert_eq!(clock_core_label("Core 0 T0 Effective Clock"), None);
        assert_eq!(ccd_label("CCD1 (Tdie)"), Some((1, "CCD 1".into())));
    }

    /// LibreHardwareMonitor on a 6-core Intel CPU, plus a GPU and a drive it
    /// also reports (those are not the CPU's).
    #[test]
    fn lhm_rows() {
        let cpu = "/intelcpu/0";
        let mut rows = vec![
            row("CPU Package", "Temperature", 61.0, cpu),
            row("Core Max", "Temperature", 66.0, cpu),
            row("Core Average", "Temperature", 58.2, cpu),
            row("CPU Package", "Power", 47.25, cpu),
            row("CPU Cores", "Power", 40.0, cpu),
            row("Bus Speed", "Clock", 100.0, cpu),
            row("GPU Core", "Temperature", 55.0, "/gpu-nvidia/0"),
            row("Temperature", "Temperature", 38.0, "/nvme/0"),
        ];
        for i in (1..=6).rev() {
            rows.push(row(&format!("Core #{i}"), "Temperature", 50.0 + i as f64, cpu));
            rows.push(row(&format!("Core #{i} Distance to TjMax"), "Temperature", 40.0, cpu));
            rows.push(row(&format!("Core #{i}"), "Clock", 4390.4 + i as f64, cpu));
        }
        let c = cpu_from_tool_rows(&rows, SRC_LHM).unwrap();
        assert_eq!(c.source, "LibreHardwareMonitor");
        assert!(!c.approximate);
        assert_eq!(c.package, Some(61.0));
        assert_eq!(c.cores.len(), 6);
        assert_eq!(c.cores[0], Reading { name: "Core 1".into(), value: 51.0 });
        assert_eq!(c.cores[5].name, "Core 6");
        assert_eq!(c.power, Some(47.3));
        assert_eq!(c.clocks.len(), 6);
        assert_eq!(c.clocks[0], Reading { name: "Core 1".into(), value: 4391.0 });
    }

    /// OpenHardwareMonitor spells cores "CPU Core #n"; AMD Zen has Tctl/Tdie and CCDs only.
    #[test]
    fn ohm_and_amd_rows() {
        let rows = vec![
            row("CPU Core #2", "Temperature", 47.0, "/intelcpu/0"),
            row("CPU Core #1", "Temperature", 45.0, "/intelcpu/0"),
        ];
        let c = cpu_from_tool_rows(&rows, SRC_OHM).unwrap();
        assert_eq!(c.package, Some(47.0)); // no package sensor: the hottest core
        assert_eq!(c.cores.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["Core 1", "Core 2"]);

        let amd = "/amdcpu/0";
        let rows = vec![
            row("Core (Tctl/Tdie)", "Temperature", 72.4, amd),
            row("CCD1 (Tdie)", "Temperature", 70.1, amd),
            row("CCD2 (Tdie)", "Temperature", 66.0, amd),
            row("CCDs Max (Tdie)", "Temperature", 70.1, amd),
            row("Package", "Power", 88.0, amd),
        ];
        let c = cpu_from_tool_rows(&rows, SRC_LHM).unwrap();
        assert_eq!(c.package, Some(72.4));
        assert_eq!(c.cores.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["CCD 1", "CCD 2"]);
        assert_eq!(c.power, Some(88.0));

        // Only a GPU, or a sensor the tool has not read yet (NaN): no CPU reading.
        assert!(cpu_from_tool_rows(&[row("GPU Core", "Temperature", 55.0, "/gpu-amd/0")], SRC_LHM).is_none());
        assert!(cpu_from_tool_rows(&[row("CPU Package", "Temperature", f64::NAN, "/intelcpu/0")], SRC_LHM).is_none());
        // Two sockets: the first one only.
        let rows = vec![row("CPU Package", "Temperature", 50.0, "/intelcpu/1"), row("CPU Package", "Temperature", 40.0, "/intelcpu/0")];
        assert_eq!(cpu_from_tool_rows(&rows, SRC_LHM).unwrap().package, Some(40.0));
    }

    /// Builds HWiNFO shared memory the way the SDK lays it out (packed,
    /// little endian), with `extra` bytes after each element as newer versions add.
    fn hwinfo_fixture(poll: i64, sensors: &[&str], readings: &[(u32, u32, &str, &[u8], f64)], extra: usize) -> Vec<u8> {
        let s_size = HWINFO_SENSOR_MIN + extra;
        let r_size = HWINFO_READING_MIN + extra;
        let s_off = HWINFO_HEADER;
        let r_off = s_off + s_size * sensors.len();
        let mut b = vec![0u8; r_off + r_size * readings.len()];
        b[0..4].copy_from_slice(b"HWiS");
        b[4..8].copy_from_slice(&2u32.to_le_bytes());
        b[12..20].copy_from_slice(&poll.to_le_bytes());
        for (at, v) in [(20, s_off), (24, s_size), (28, sensors.len()), (32, r_off), (36, r_size), (40, readings.len())] {
            b[at..at + 4].copy_from_slice(&(v as u32).to_le_bytes());
        }
        let put = |b: &mut Vec<u8>, at: usize, s: &[u8]| b[at..at + s.len()].copy_from_slice(s);
        for (i, name) in sensors.iter().enumerate() {
            let base = s_off + i * s_size;
            put(&mut b, base + 8, name.as_bytes());
            put(&mut b, base + 136, b"renamed by user");
        }
        for (i, (kind, sensor, label, unit, value)) in readings.iter().enumerate() {
            let base = r_off + i * r_size;
            put(&mut b, base, &kind.to_le_bytes());
            put(&mut b, base + 4, &sensor.to_le_bytes());
            put(&mut b, base + 12, label.as_bytes());
            put(&mut b, base + 140, b"user label");
            put(&mut b, base + 268, unit);
            put(&mut b, base + 284, &value.to_le_bytes());
        }
        b
    }

    #[test]
    fn hwinfo_layout() {
        let now = 1_800_000_000;
        let sensors = ["System: ASUS", "CPU [#0]: Intel Core i5-12400F", "CPU [#0]: Intel Core i5-12400F: DTS", "GPU [#0]: NVIDIA"];
        let c = b"\xB0C";
        let readings: Vec<(u32, u32, &str, &[u8], f64)> = vec![
            (HWINFO_TEMP, 0, "Motherboard", c, 33.0),
            (HWINFO_CLOCK, 1, "Core 0 Clock (perf #1/1)", b"MHz", 4389.6),
            (HWINFO_CLOCK, 1, "Core 1 Clock (perf #2/2)", b"MHz", 4400.0),
            (HWINFO_CLOCK, 1, "Bus Clock", b"MHz", 100.0),
            (HWINFO_POWER, 1, "CPU Package Power", b"W", 38.44),
            (HWINFO_TEMP, 2, "Core #1", c, 55.0),
            (HWINFO_TEMP, 2, "Core #0", c, 57.0),
            (HWINFO_TEMP, 2, "Core #0 Distance to TjMAX", c, 43.0),
            (HWINFO_TEMP, 2, "CPU Package", c, 59.0),
            (HWINFO_TEMP, 2, "Core Max", c, 57.0),
            (HWINFO_TEMP, 3, "GPU Temperature", c, 48.0),
        ];
        for extra in [0, 144] {
            let b = hwinfo_fixture(now - 1, &sensors, &readings, extra);
            let s = parse_hwinfo(&b, now).unwrap();
            assert_eq!(s.source, "HWiNFO");
            assert_eq!(s.package, Some(59.0));
            assert_eq!(s.cores, [Reading { name: "Core 0".into(), value: 57.0 }, Reading { name: "Core 1".into(), value: 55.0 }]);
            assert_eq!(s.power, Some(38.4));
            assert_eq!(s.clocks, [Reading { name: "Core 0".into(), value: 4390.0 }, Reading { name: "Core 1".into(), value: 4400.0 }]);
        }
        // Fahrenheit in HWiNFO's settings: converted back.
        let b = hwinfo_fixture(now, &sensors[1..2], &[(HWINFO_TEMP, 0, "CPU Package", b"\xB0F", 140.0)], 0);
        assert_eq!(parse_hwinfo(&b, now).unwrap().package, Some(60.0));
        // Not sharing any more, stale, cut short, or no CPU sensor.
        let mut dead = hwinfo_fixture(now, &sensors, &readings, 0);
        dead[0..4].copy_from_slice(b"DEAD");
        assert!(parse_hwinfo(&dead, now).is_none());
        assert!(parse_hwinfo(&hwinfo_fixture(now - 600, &sensors, &readings, 0), now).is_none());
        let full = hwinfo_fixture(now, &sensors, &readings, 0);
        assert!(parse_hwinfo(&full[..full.len() - 1], now).is_none());
        assert!(parse_hwinfo(&full[..10], now).is_none());
        assert!(parse_hwinfo(&hwinfo_fixture(now, &sensors[3..], &readings[10..], 0), now).is_none());
        assert_eq!(hwinfo_len(&full), Some(full.len()));
    }

    #[test]
    fn acpi_tenths_of_kelvin() {
        // 323.2 K = 50.05 °C, kept to one decimal.
        let near = |v: Option<f64>, want: f64| v.is_some_and(|c| (c - want).abs() <= 0.051);
        assert!(near(acpi_celsius(3232), 50.05));
        assert!(near(acpi_celsius(2982), 25.05));
        assert_eq!(acpi_celsius(2730), None); // below 0 °C: not a reading
        assert_eq!(acpi_celsius(0), None);
        assert_eq!(acpi_celsius(5000), None); // 226.9 °C
        let z = |n: &str, v: u64| (n.to_string(), v);
        let mut zones = AcpiZones::default();
        // A constant zone is never shown.
        assert_eq!(zones.update(&[z("TZ00", 3010), z("TZ01", 3132)]), None);
        assert_eq!(zones.update(&[z("TZ00", 3010), z("TZ01", 3132)]), None);
        // Once TZ01 moves it is shown, and stays shown when it settles back.
        assert!(near(zones.update(&[z("TZ00", 3010), z("TZ01", 3162)]), 43.05));
        assert!(near(zones.update(&[z("TZ00", 3010), z("TZ01", 3132)]), 40.05));
    }

    #[test]
    fn stale_only_when_a_read_hangs() {
        let reading = Sensors { cpu: None, gpus: vec![GpuSensors {
            luid: None, name: "RTX".into(), source: SRC_NVML.into(), temp: Some(50.0), memory_temp: None,
            slowdown_temp: None, fan_pct: None, power_w: None, power_limit_w: None, core_mhz: None, memory_mhz: None,
        }] };
        // However old the reading (a 5 s interval, a long pause), it is shown between reads...
        let mut st = HubState { latest: Some(reading.clone()), ..Default::default() };
        assert_eq!(st.current(), reading);
        // ...and while a read is under way...
        st.busy_since = Some(Instant::now());
        assert_eq!(st.current(), reading);
        // ...until that read has hung past STALE.
        if let Some(t) = Instant::now().checked_sub(STALE + Duration::from_secs(1)) {
            st.busy_since = Some(t);
            assert_eq!(st.current(), Sensors::default());
        }
    }

    #[test]
    fn exit_stops_the_thread() {
        let shared = Arc::new(Shared::default());
        let t = { let s = Arc::clone(&shared); std::thread::spawn(move || run(s)) };
        lock(&shared.state).stop = true;
        shared.cv.notify_all();
        t.join().unwrap();
        assert!(lock(&shared.state).done);
    }

    #[test]
    fn hub_drops_nothing_until_read() {
        // Before the thread has read anything, the snapshot carries no sensors.
        let mut hub = SensorHub::default();
        assert_eq!(hub.poll(), Sensors::default());
        let json = serde_json::to_value(Sensors::default()).unwrap();
        assert_eq!(json, serde_json::json!({ "cpu": null, "gpus": [] }));
    }

    /// What this PC's sensors return: `cargo test real_sensors -- --ignored --nocapture`.
    #[test]
    #[ignore = "reads this PC's sensors"]
    fn real_sensors() {
        let _com = crate::health::com_init();
        println!("elevated: {}", is_admin());
        println!("nvml candidates: {:?}", nvml_paths().iter().map(|p| (p.display().to_string(), p.is_file())).collect::<Vec<_>>());
        for a in crate::gpu::adapters() {
            println!("dxgi: {} {} {:?} addr {:?}", a.luid, a.name, a.vendor, adapter_address(&a.luid));
        }
        match Nvml::load() {
            Some(n) => {
                println!("nvml loaded from {}", n.path.display());
                for d in &n.devices {
                    println!("nvml device: {:?} pci {:?} -> luid {:?}, slowdown {:?}", d.name, d.addr, d.luid, d.slowdown);
                }
            }
            None => println!("nvml: not loaded"),
        }
        println!("hwinfo shared memory: {}", if map_hwinfo().is_some() { "present" } else { "absent" });
        for tool in [WmiTool::Libre, WmiTool::Open] {
            println!("{}: {}", tool.namespace(), if Wmi::connect(tool.namespace()).is_ok() { "namespace found" } else { "no namespace" });
        }
        let mut reader = Reader::default();
        let started = Instant::now();
        let first = reader.read();
        println!("first read took {:.1} ms", started.elapsed().as_secs_f64() * 1000.0);
        let mut last = first;
        for _ in 0..3 {
            std::thread::sleep(Duration::from_millis(1000));
            let t = Instant::now();
            last = reader.read();
            println!("read took {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
        }
        println!("{}", serde_json::to_string_pretty(&last).unwrap());
        for g in &last.gpus {
            assert_eq!(g.source, "NVML");
            if let Some(t) = g.temp {
                assert!(t > 0.0 && t < 150.0);
            }
        }
    }
}
