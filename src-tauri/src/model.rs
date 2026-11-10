//! Data shapes shared between the backend modules and the UI.
//!
//! Every struct serializes with camelCase field names, and `Option` fields
//! serialize as `null`, matching `docs/CONTRACT.md`. Other modules use these
//! through `use crate::model::*;`.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Snapshot (event "orbit://snapshot" and command get_snapshot)
// ---------------------------------------------------------------------------

/// One sample of the whole system, pushed to the UI every tick.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    /// Milliseconds since the Unix epoch.
    pub ts: u64,
    pub interval_ms: u64,
    pub cpu: CpuStats,
    pub memory: MemoryStats,
    pub disk: DiskStats,
    pub network: NetworkStats,
    pub gpu: GpuStats,
    pub system: SystemStats,
    /// Per physical disk activity (PDH PhysicalDisk instances, "_Total" left
    /// out). Empty when the counters are unavailable.
    pub disks: Vec<DiskActivity>,
    /// GPU usage per engine type ("3D", "Copy", "Video Decode", ...), 0-100.
    /// Empty when GPU counters are unavailable.
    pub gpu_engines: Vec<GpuEngine>,
    /// `None` on a PC without a battery.
    pub battery: Option<BatteryStats>,
    /// CPU and GPU temperatures, fan, power and clocks (sensors.rs, 0.7).
    /// `{ cpu: null, gpus: [] }` when no source reports them.
    #[serde(default)]
    pub sensors: crate::sensors::Sensors,
    pub processes: Vec<ProcessRow>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CpuStats {
    /// Total usage, 0-100.
    pub usage: f64,
    /// Usage per logical core, 0-100 each.
    pub per_core: Vec<f64>,
    pub freq_mhz: u64,
}

/// All values in bytes.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct MemoryStats {
    pub total: u64,
    pub used: u64,
    pub available: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    pub committed: Option<u64>,
    pub commit_limit: Option<u64>,
    /// Task Manager's "Cached": standby lists plus the modified page list.
    /// Mostly part of `available`.
    pub cached: Option<u64>,
    /// Free and zeroed pages ("Free" in Task Manager's composition bar).
    pub free: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DiskStats {
    /// Physical disk active time, 0-100, `None` when the counter is unavailable.
    pub active_pct: Option<f64>,
    /// Physical disk bytes/sec, `None` when the PDH counter is unavailable.
    pub read_bps: Option<f64>,
    pub write_bps: Option<f64>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct NetworkStats {
    pub rx_bps: f64,
    pub tx_bps: f64,
    pub interfaces: Vec<InterfaceStats>,
    /// Whether per-process network rates (`ProcessRow::net_rx_bps`) are filled.
    pub per_app: PerAppStatus,
}

/// Whether per-app network data is available, and why not when it isn't.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct PerAppStatus {
    pub available: bool,
    /// Short human reason when `available` is false, e.g. "Needs administrator rights.".
    pub reason: Option<String>,
}

/// One physical disk.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DiskActivity {
    /// PDH instance name, e.g. "0 C: E:".
    pub name: String,
    /// Active time 0-100, `None` when unavailable.
    pub active_pct: Option<f64>,
    pub read_bps: f64,
    pub write_bps: f64,
}

/// Usage of one GPU engine type, summed over processes.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct GpuEngine {
    /// "3D", "Copy", "Video Decode", "Video Encode", "Compute", ...
    pub name: String,
    /// 0-100.
    pub usage: f64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct BatteryStats {
    /// Charge 0-100.
    pub percent: f64,
    pub charging: bool,
    /// Plugged in.
    pub ac_online: bool,
    /// Estimated seconds of battery left, `None` while charging or unknown.
    pub seconds_left: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct InterfaceStats {
    pub name: String,
    pub rx_bps: f64,
    pub tx_bps: f64,
    pub total_rx: u64,
    pub total_tx: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct GpuStats {
    /// 0-100, `None` when GPU counters are unavailable.
    pub usage: Option<f64>,
    /// Bytes of dedicated GPU memory in use.
    pub dedicated_used: Option<u64>,
    /// The same per adapter (PDH LUID), sorted by `luid`. Empty without
    /// counters. Names come from `get_gpu_info` (gpu.rs), matched by `luid`.
    #[serde(default)]
    pub adapters: Vec<GpuAdapterLoad>,
}

/// Live load of one GPU adapter.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GpuAdapterLoad {
    /// "0x00000000_0x0000d1a5": LUID high part, then low part (gpu::luid_key).
    pub luid: String,
    /// Busiest engine of this adapter, 0-100.
    pub usage: Option<f64>,
    /// Bytes of this adapter's dedicated memory in use.
    pub dedicated_used: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SystemStats {
    pub uptime_secs: u64,
    pub process_count: u32,
    pub thread_count: Option<u64>,
    pub handle_count: Option<u64>,
}

/// How Task Manager would group a process.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum ProcessKind {
    /// Owns a visible, unowned, titled top-level window.
    App,
    /// Anything that is neither an app nor part of Windows.
    Background,
    /// Core Windows process (pid 0/4, under %SystemRoot%, or a known core name).
    Windows,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ProcessRow {
    pub pid: u32,
    pub parent_pid: Option<u32>,
    /// Image name, e.g. "chrome.exe".
    pub name: String,
    /// Full path, `None` if access is denied.
    pub exe: Option<String>,
    /// Command line joined by spaces, "" if unknown.
    pub cmd: String,
    pub user: Option<String>,
    /// 0-100, normalised by logical core count.
    pub cpu: f64,
    /// Working set in bytes.
    pub memory: u64,
    /// All I/O of the process (file, network and device), from IO_COUNTERS
    /// transfer counts. Not physical disk bytes.
    pub disk_read_bps: f64,
    pub disk_write_bps: f64,
    /// 0-100, `None` when GPU counters are unavailable.
    pub gpu: Option<f64>,
    /// "running" | "sleeping" | "stopped" | "zombie" | "dead" | "unknown".
    pub status: String,
    /// Seconds since the Unix epoch.
    pub start_time: u64,
    pub kind: ProcessKind,
    pub window_title: Option<String>,
    pub not_responding: bool,
    /// True if Orbit suspended this process.
    pub suspended: bool,
    /// Windows session (0 = services), `None` if unreadable.
    pub session_id: Option<u32>,
    /// Network bytes/sec received and sent by this process. `None` unless
    /// per-app network data is available (see `NetworkStats::per_app`).
    pub net_rx_bps: Option<f64>,
    pub net_tx_bps: Option<f64>,
}

// ---------------------------------------------------------------------------
// Per-tick event payload (event "orbit://snapshot")
// ---------------------------------------------------------------------------

/// The fields of a process row that change from tick to tick.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DynRow<'a> {
    pub pid: u32,
    pub cpu: f64,
    pub memory: u64,
    pub disk_read_bps: f64,
    pub disk_write_bps: f64,
    pub gpu: Option<f64>,
    pub status: &'a str,
    /// Can change when a process opens or closes its window.
    pub kind: ProcessKind,
    pub window_title: Option<&'a str>,
    pub not_responding: bool,
    pub suspended: bool,
    pub session_id: Option<u32>,
    pub net_rx_bps: Option<f64>,
    pub net_tx_bps: Option<f64>,
}

impl<'a> From<&'a ProcessRow> for DynRow<'a> {
    fn from(r: &'a ProcessRow) -> Self {
        Self {
            pid: r.pid,
            cpu: r.cpu,
            memory: r.memory,
            disk_read_bps: r.disk_read_bps,
            disk_write_bps: r.disk_write_bps,
            gpu: r.gpu,
            status: &r.status,
            kind: r.kind,
            window_title: r.window_title.as_deref(),
            not_responding: r.not_responding,
            suspended: r.suspended,
            session_id: r.session_id,
            net_rx_bps: r.net_rx_bps,
            net_tx_bps: r.net_tx_bps,
        }
    }
}

/// What the sampler emits every tick: the system stats of a `Snapshot`, plus
/// the process list as a delta against the previous tick. `added` holds full
/// rows for (pid, startTime) pairs that were not in the previous tick,
/// `removed` the pids that are gone (a reused pid appears in both, apply
/// `removed` first), and `rows` the dynamic fields of every process.
/// `get_snapshot` still returns the full `Snapshot`, for first load and resync.
#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotDelta<'a> {
    pub ts: u64,
    pub interval_ms: u64,
    pub cpu: &'a CpuStats,
    pub memory: &'a MemoryStats,
    pub disk: &'a DiskStats,
    pub network: &'a NetworkStats,
    pub gpu: &'a GpuStats,
    pub system: &'a SystemStats,
    pub disks: &'a [DiskActivity],
    pub gpu_engines: &'a [GpuEngine],
    pub battery: Option<&'a BatteryStats>,
    pub sensors: &'a crate::sensors::Sensors,
    pub added: Vec<&'a ProcessRow>,
    pub removed: Vec<u32>,
    pub rows: Vec<DynRow<'a>>,
}

// ---------------------------------------------------------------------------
// get_static_info
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct StaticInfo {
    pub cpu_brand: String,
    pub logical_cores: u32,
    pub physical_cores: Option<u32>,
    pub total_memory: u64,
    pub host_name: Option<String>,
    pub os_name: Option<String>,
    pub os_version: Option<String>,
    pub kernel_version: Option<String>,
    pub is_admin: bool,
    /// Seconds since the Unix epoch.
    pub boot_time: u64,
    pub disks: Vec<DiskInfo>,
    /// Orbit's own pid.
    pub pid: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DiskInfo {
    pub name: String,
    pub mount: String,
    pub file_system: String,
    pub total: u64,
    pub available: u64,
    /// "SSD" | "HDD" | "Unknown".
    pub kind: String,
    pub removable: bool,
}

// ---------------------------------------------------------------------------
// get_process_details (filled by actions.rs)
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ProcessDetails {
    pub pid: u32,
    pub parent_pid: Option<u32>,
    pub parent_name: Option<String>,
    pub name: String,
    pub exe: Option<String>,
    pub cmd: String,
    pub cwd: Option<String>,
    pub user: Option<String>,
    /// Seconds since the Unix epoch.
    pub start_time: u64,
    /// One of the priority strings in the contract, `None` if unreadable.
    pub priority: Option<String>,
    pub threads: Option<u32>,
    pub handles: Option<u32>,
    /// Working set in bytes.
    pub memory: u64,
    /// Virtual memory in bytes.
    pub virtual_memory: u64,
    pub children: Vec<ChildRef>,
    pub disk_total_read: u64,
    pub disk_total_written: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ChildRef {
    pub pid: u32,
    pub name: String,
}

// ---------------------------------------------------------------------------
// list_startup (filled by startup.rs)
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct StartupEntry {
    /// Stable id, e.g. "HKCU\\Run\\Discord" or "UserFolder\\X.lnk".
    pub id: String,
    pub name: String,
    pub command: String,
    /// "HKCU Run" | "HKLM Run" | "HKLM Run (32-bit)" | "Startup folder" |
    /// "Startup folder (all users)".
    pub location: String,
    pub enabled: bool,
    /// Resolved executable path, if the command could be parsed.
    pub path: Option<String>,
    pub needs_admin: bool,
    /// "high" | "medium" | "low" from Orbit's own startup measurement
    /// (impact.rs), `None` when this entry has not been measured.
    pub impact: Option<String>,
    /// Seconds since the Unix epoch of that measurement, `None` with `impact`.
    pub impact_measured_at: Option<u64>,
}

// ---------------------------------------------------------------------------
// get_affinity (filled by actions.rs)
// ---------------------------------------------------------------------------

/// CPU affinity of a process. Masks are decimal strings because a 64-bit
/// mask does not fit a JavaScript number.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AffinityInfo {
    /// Processors the process may run on (bit n = CPU n).
    pub process_mask: String,
    /// Processors the system has (in the process's processor group).
    pub system_mask: String,
    /// Number of bits set in `system_mask`.
    pub logical_cores: u32,
}

// ---------------------------------------------------------------------------
// list_services (filled by services.rs)
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ServiceEntry {
    pub name: String,
    pub display_name: String,
    /// "running" | "stopped" | "startPending" | "stopPending" | "paused" |
    /// "pausePending" | "continuePending" | "unknown".
    pub status: String,
    pub pid: Option<u32>,
    /// "auto" | "autoDelayed" | "manual" | "disabled" | "boot" | "system" | "unknown".
    pub start_type: String,
    pub description: Option<String>,
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// Processes that Orbit has suspended: pid -> start time (seconds since the
/// Unix epoch, as in `ProcessRow::start_time`). The start time guards against
/// pid reuse. `actions` inserts and removes; the sampler reads it to fill
/// `ProcessRow::suspended` and drops entries whose process has exited; on
/// exit `actions::resume_all` resumes whatever is left.
#[derive(Default, Debug)]
pub struct SuspendedSet(pub Mutex<HashMap<u32, u64>>);

/// True when two start times (Unix seconds) describe the same process. The
/// two sources can round differently, so one second of slack is allowed.
pub fn same_start(a: u64, b: u64) -> bool {
    a.abs_diff(b) <= 1
}
