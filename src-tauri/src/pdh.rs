//! Windows Performance Data Helper (PDH) counters: GPU per process, per
//! engine type and total, physical disk activity (all disks and each disk),
//! thread and handle counts, commit charge and the memory page lists
//! (standby, modified, free).
//!
//! The PDH entry points are declared directly against `pdh.dll` so the ABI is
//! exactly the documented C one. Every counter is optional: a counter that
//! cannot be added (for example a machine without GPU counters) reads as
//! `None`, never a panic.
//!
//! The query handles are raw pointers, so `Pdh` is not `Send`; it is created
//! and used only on the sampler thread.

use std::collections::{BTreeMap, HashMap};
use std::ffi::c_void;
use std::ptr::null_mut;
use std::time::{Duration, Instant};

use crate::model::{DiskActivity, GpuAdapterLoad, GpuEngine};

type PdhHandle = *mut c_void;
type PdhStatus = i32;

const ERROR_SUCCESS: PdhStatus = 0;
const PDH_MORE_DATA: PdhStatus = 0x8000_07D2_u32 as i32;
const PDH_CSTATUS_VALID_DATA: u32 = 0x0000_0000;
const PDH_CSTATUS_NEW_DATA: u32 = 0x0000_0001;
const PDH_FMT_DOUBLE: u32 = 0x0000_0200;
const PDH_FMT_NOCAP100: u32 = 0x0000_8000;
const FORMAT: u32 = PDH_FMT_DOUBLE | PDH_FMT_NOCAP100;

/// GPU process instances come and go; rebuild the wildcard query this often.
const GPU_REBUILD_EVERY: Duration = Duration::from_secs(10);
/// When GPU counters are missing entirely, try again this rarely.
const GPU_RETRY_EVERY: Duration = Duration::from_secs(60);

/// `PDH_FMT_COUNTERVALUE`: a status word followed by an 8-byte union. We
/// always request `PDH_FMT_DOUBLE`, so the union is read as `f64`.
#[repr(C)]
#[derive(Clone, Copy)]
struct FmtCounterValue {
    c_status: u32,
    value: f64,
}

/// `PDH_FMT_COUNTERVALUE_ITEM_W`.
#[repr(C)]
#[derive(Clone, Copy)]
struct FmtCounterValueItem {
    name: *const u16,
    value: FmtCounterValue,
}

#[link(name = "pdh")]
extern "system" {
    fn PdhOpenQueryW(data_source: *const u16, user_data: usize, query: *mut PdhHandle)
        -> PdhStatus;
    fn PdhAddEnglishCounterW(
        query: PdhHandle,
        full_counter_path: *const u16,
        user_data: usize,
        counter: *mut PdhHandle,
    ) -> PdhStatus;
    fn PdhCollectQueryData(query: PdhHandle) -> PdhStatus;
    fn PdhGetFormattedCounterValue(
        counter: PdhHandle,
        format: u32,
        counter_type: *mut u32,
        value: *mut FmtCounterValue,
    ) -> PdhStatus;
    fn PdhGetFormattedCounterArrayW(
        counter: PdhHandle,
        format: u32,
        buffer_size: *mut u32,
        item_count: *mut u32,
        item_buffer: *mut FmtCounterValueItem,
    ) -> PdhStatus;
    fn PdhCloseQuery(query: PdhHandle) -> PdhStatus;
}

/// Everything PDH could read this tick. `None` means the counter is missing
/// or produced no valid data.
#[derive(Clone, Debug, Default)]
pub struct PdhSample {
    /// Total GPU usage 0-100.
    pub gpu_total: Option<f64>,
    /// GPU usage per pid, 0-100.
    pub gpu_per_pid: HashMap<u32, f64>,
    /// Adapter-level dedicated GPU memory in use on the busiest adapter,
    /// bytes (`\GPU Adapter Memory(*)\Dedicated Usage`; not a sum over
    /// adapters). Falls back to the per-process sum when that counter is missing.
    pub gpu_dedicated: Option<u64>,
    /// 100 - "% Idle Time" of all physical disks, 0-100.
    pub disk_active_pct: Option<f64>,
    pub disk_read_bps: Option<f64>,
    pub disk_write_bps: Option<f64>,
    /// One entry per physical disk ("_Total" left out), sorted by name.
    pub disks: Vec<DiskActivity>,
    /// GPU usage per engine type, busiest engine of each type, 0-100.
    pub gpu_engines: Vec<GpuEngine>,
    /// Usage and dedicated memory per adapter LUID, sorted by LUID.
    pub gpu_adapters: Vec<GpuAdapterLoad>,
    pub threads: Option<u64>,
    pub handles: Option<u64>,
    pub committed: Option<u64>,
    pub commit_limit: Option<u64>,
    /// Standby list, by priority band.
    pub standby_core: Option<u64>,
    pub standby_normal: Option<u64>,
    pub standby_reserve: Option<u64>,
    /// Modified page list.
    pub modified: Option<u64>,
    /// Free and zeroed page lists.
    pub free_zero: Option<u64>,
}

/// Owner of all PDH queries used by the sampler.
pub struct Pdh {
    system: Option<SystemCounters>,
    gpu: Option<GpuCounters>,
    gpu_built_at: Instant,
}

impl Default for Pdh {
    fn default() -> Self {
        Self::new()
    }
}

impl Pdh {
    /// Opens the queries and collects a first baseline, so rate counters are
    /// valid on the next `sample` call.
    pub fn new() -> Self {
        Self {
            system: SystemCounters::create(),
            gpu: GpuCounters::create(),
            gpu_built_at: Instant::now(),
        }
    }

    /// Collects every query once and returns the formatted values.
    pub fn sample(&mut self) -> PdhSample {
        let mut out = PdhSample::default();
        if let Some(system) = &self.system {
            system.read(&mut out);
        }
        if let Some(gpu) = &self.gpu {
            gpu.read(&mut out);
        }

        // Rebuild the GPU query after reading the old one, so this tick keeps
        // valid numbers and the new query has a baseline for the next tick.
        let rebuild_after = if self.gpu.is_some() {
            GPU_REBUILD_EVERY
        } else {
            GPU_RETRY_EVERY
        };
        if self.gpu_built_at.elapsed() >= rebuild_after {
            self.gpu = GpuCounters::create();
            self.gpu_built_at = Instant::now();
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Query wrapper
// ---------------------------------------------------------------------------

struct Query {
    handle: PdhHandle,
}

impl Query {
    fn open() -> Option<Self> {
        let mut handle: PdhHandle = null_mut();
        // SAFETY: a null data source selects real-time data; `handle` is a
        // valid out pointer for the duration of the call.
        let status = unsafe { PdhOpenQueryW(std::ptr::null(), 0, &mut handle) };
        if status == ERROR_SUCCESS && !handle.is_null() {
            Some(Self { handle })
        } else {
            None
        }
    }

    /// Adds an English counter path; `None` if the object or counter does not
    /// exist on this machine.
    fn add(&self, path: &str) -> Option<PdhHandle> {
        let wide = to_wide(path);
        let mut counter: PdhHandle = null_mut();
        // SAFETY: `wide` is NUL-terminated and outlives the call; `counter`
        // is a valid out pointer; `self.handle` is an open query.
        let status = unsafe { PdhAddEnglishCounterW(self.handle, wide.as_ptr(), 0, &mut counter) };
        if status == ERROR_SUCCESS && !counter.is_null() {
            Some(counter)
        } else {
            None
        }
    }

    fn collect(&self) -> bool {
        // SAFETY: `self.handle` is an open query owned by this struct.
        unsafe { PdhCollectQueryData(self.handle) == ERROR_SUCCESS }
    }
}

impl Drop for Query {
    fn drop(&mut self) {
        // SAFETY: the handle came from PdhOpenQueryW and is closed exactly
        // once; closing a query also frees its counters.
        unsafe {
            PdhCloseQuery(self.handle);
        }
    }
}

// ---------------------------------------------------------------------------
// System counters (stable instance names, one query for the app lifetime)
// ---------------------------------------------------------------------------

struct SystemCounters {
    query: Query,
    disk_idle: Option<PdhHandle>,
    disk_read: Option<PdhHandle>,
    disk_write: Option<PdhHandle>,
    /// Wildcard `PhysicalDisk(*)` counters, one instance per physical disk.
    disks_idle: Option<PdhHandle>,
    disks_read: Option<PdhHandle>,
    disks_write: Option<PdhHandle>,
    threads: Option<PdhHandle>,
    handles: Option<PdhHandle>,
    committed: Option<PdhHandle>,
    commit_limit: Option<PdhHandle>,
    standby_core: Option<PdhHandle>,
    standby_normal: Option<PdhHandle>,
    standby_reserve: Option<PdhHandle>,
    modified: Option<PdhHandle>,
    free_zero: Option<PdhHandle>,
}

impl SystemCounters {
    fn create() -> Option<Self> {
        let query = Query::open()?;
        let counters = Self {
            disk_idle: query.add(r"\PhysicalDisk(_Total)\% Idle Time"),
            disk_read: query.add(r"\PhysicalDisk(_Total)\Disk Read Bytes/sec"),
            disk_write: query.add(r"\PhysicalDisk(_Total)\Disk Write Bytes/sec"),
            disks_idle: query.add(r"\PhysicalDisk(*)\% Idle Time"),
            disks_read: query.add(r"\PhysicalDisk(*)\Disk Read Bytes/sec"),
            disks_write: query.add(r"\PhysicalDisk(*)\Disk Write Bytes/sec"),
            threads: query.add(r"\System\Threads"),
            handles: query.add(r"\Process(_Total)\Handle Count"),
            committed: query.add(r"\Memory\Committed Bytes"),
            commit_limit: query.add(r"\Memory\Commit Limit"),
            // Task Manager's "Cached" is standby + modified, not Cache Bytes
            // (the file-cache working set, which is already part of "in use").
            standby_core: query.add(r"\Memory\Standby Cache Core Bytes"),
            standby_normal: query.add(r"\Memory\Standby Cache Normal Priority Bytes"),
            standby_reserve: query.add(r"\Memory\Standby Cache Reserve Bytes"),
            modified: query.add(r"\Memory\Modified Page List Bytes"),
            free_zero: query.add(r"\Memory\Free & Zero Page List Bytes"),
            query,
        };
        let any = [
            counters.disk_idle,
            counters.disk_read,
            counters.disk_write,
            counters.disks_idle,
            counters.disks_read,
            counters.disks_write,
            counters.threads,
            counters.handles,
            counters.committed,
            counters.commit_limit,
            counters.standby_core,
            counters.standby_normal,
            counters.standby_reserve,
            counters.modified,
            counters.free_zero,
        ]
        .iter()
        .any(Option::is_some);
        if !any {
            return None;
        }
        // Baseline for the rate counters (% Idle Time, bytes/sec).
        counters.query.collect();
        Some(counters)
    }

    fn read(&self, out: &mut PdhSample) {
        if !self.query.collect() {
            return;
        }
        out.disk_active_pct = self
            .disk_idle
            .and_then(read_double)
            .map(|idle| (100.0 - idle).clamp(0.0, 100.0));
        out.disk_read_bps = self.disk_read.and_then(read_double).map(|v| v.max(0.0));
        out.disk_write_bps = self.disk_write.and_then(read_double).map(|v| v.max(0.0));
        out.disks = merge_disks(
            self.disks_idle.and_then(read_array).unwrap_or_default(),
            self.disks_read.and_then(read_array).unwrap_or_default(),
            self.disks_write.and_then(read_array).unwrap_or_default(),
        );
        out.threads = self.threads.and_then(read_double).map(to_u64);
        out.handles = self.handles.and_then(read_double).map(to_u64);
        out.committed = self.committed.and_then(read_double).map(to_u64);
        out.commit_limit = self.commit_limit.and_then(read_double).map(to_u64);
        out.standby_core = self.standby_core.and_then(read_double).map(to_u64);
        out.standby_normal = self.standby_normal.and_then(read_double).map(to_u64);
        out.standby_reserve = self.standby_reserve.and_then(read_double).map(to_u64);
        out.modified = self.modified.and_then(read_double).map(to_u64);
        out.free_zero = self.free_zero.and_then(read_double).map(to_u64);
    }
}

// ---------------------------------------------------------------------------
// GPU counters (wildcard instances, rebuilt periodically)
// ---------------------------------------------------------------------------

struct GpuCounters {
    query: Query,
    engine: Option<PdhHandle>,
    memory: Option<PdhHandle>,
    adapter_memory: Option<PdhHandle>,
}

impl GpuCounters {
    fn create() -> Option<Self> {
        let query = Query::open()?;
        let engine = query.add(r"\GPU Engine(*)\Utilization Percentage");
        let memory = query.add(r"\GPU Process Memory(*)\Dedicated Usage");
        let adapter_memory = query.add(r"\GPU Adapter Memory(*)\Dedicated Usage");
        if engine.is_none() && memory.is_none() && adapter_memory.is_none() {
            return None;
        }
        // Baseline: Utilization Percentage is a rate counter.
        query.collect();
        Some(Self {
            query,
            engine,
            memory,
            adapter_memory,
        })
    }

    fn read(&self, out: &mut PdhSample) {
        if !self.query.collect() {
            return;
        }

        if let Some(items) = self.engine.and_then(read_array) {
            // Like Task Manager, usage is that of the busiest single engine
            // (adapter luid + physical index + engine index), never a sum of
            // several engines of one type or of several adapters.
            // Per pid: sum that pid's instances per engine, then the busiest.
            let mut per_pid: HashMap<u32, HashMap<&str, f64>> = HashMap::new();
            // Total: sum per engine across all pids, then the busiest engine.
            // The engine type ("3D", "VideoDecode") rides along for
            // `gpu_engines`.
            let mut per_engine: HashMap<&str, (&str, f64)> = HashMap::new();
            for (name, value) in &items {
                if let Some((pid, engine)) = parse_engine_instance(name) {
                    let v = value.max(0.0);
                    *per_pid.entry(pid).or_default().entry(engine).or_default() += v;
                    let slot = per_engine
                        .entry(engine)
                        .or_insert((engine_type(name).unwrap_or(""), 0.0));
                    slot.1 += v;
                }
            }
            out.gpu_per_pid = per_pid
                .into_iter()
                .map(|(pid, engines)| {
                    let busiest = engines.values().copied().fold(0.0_f64, f64::max);
                    (pid, busiest.clamp(0.0, 100.0))
                })
                .collect();
            let total = per_engine.values().map(|e| e.1).fold(0.0_f64, f64::max);
            out.gpu_total = Some(total.clamp(0.0, 100.0));
            for (engine, (_, usage)) in &per_engine {
                if let Some(luid) = parse_adapter(engine).and_then(adapter_luid) {
                    let slot = adapter_slot(&mut out.gpu_adapters, luid);
                    let u = usage.clamp(0.0, 100.0);
                    slot.usage = Some(slot.usage.map_or(u, |v| v.max(u)));
                }
            }
            out.gpu_engines = engine_types(per_engine.into_values());
        }

        let adapter_items = self.adapter_memory.and_then(read_array).filter(|items| !items.is_empty());
        if let Some(items) = adapter_items {
            // What each adapter has in use, counted once. The largest one.
            let largest = items
                .iter()
                .filter(|(name, _)| parse_adapter(name).is_some())
                .map(|(_, value)| value.max(0.0))
                .fold(0.0_f64, f64::max);
            out.gpu_dedicated = Some(to_u64(largest));
            for (name, value) in &items {
                if let Some(luid) = parse_adapter(name).and_then(adapter_luid) {
                    let slot = adapter_slot(&mut out.gpu_adapters, luid);
                    slot.dedicated_used = Some(slot.dedicated_used.unwrap_or(0) + to_u64(value.max(0.0)));
                }
            }
        } else if let Some(items) = self.memory.and_then(read_array) {
            // Fallback: sum per adapter, then report the adapter using the most.
            // Per-process values can count shared memory more than once.
            let mut per_adapter: HashMap<&str, f64> = HashMap::new();
            for (name, value) in &items {
                if let Some(adapter) = parse_adapter(name) {
                    *per_adapter.entry(adapter).or_default() += value.max(0.0);
                }
            }
            let largest = per_adapter.values().copied().fold(0.0_f64, f64::max);
            out.gpu_dedicated = Some(to_u64(largest));
            for (adapter, value) in per_adapter {
                if let Some(luid) = adapter_luid(adapter) {
                    adapter_slot(&mut out.gpu_adapters, luid).dedicated_used = Some(to_u64(value));
                }
            }
        }
        out.gpu_adapters.sort_by(|a, b| a.luid.cmp(&b.luid));
    }
}

/// "luid_0x00000000_0x0000D1A5" -> "0x00000000_0x0000d1a5" (gpu::luid_key),
/// so PDH instances match DXGI adapters.
fn adapter_luid(key: &str) -> Option<String> {
    let rest = key.strip_prefix("luid_")?;
    let (high, low) = rest.split_once('_')?;
    let hex = |s: &str| u32::from_str_radix(s.trim_start_matches("0x").trim_start_matches("0X"), 16).ok();
    Some(crate::gpu::luid_key(hex(high)?, hex(low)?))
}

fn adapter_slot(list: &mut Vec<GpuAdapterLoad>, luid: String) -> &mut GpuAdapterLoad {
    let i = match list.iter().position(|a| a.luid == luid) {
        Some(i) => i,
        None => {
            list.push(GpuAdapterLoad { luid, usage: None, dedicated_used: None });
            list.len() - 1
        }
    };
    &mut list[i]
}

/// Parses "pid_1234_luid_0x00000000_0x0000D1A5_phys_0_eng_3_engtype_3D" into
/// `(1234, "luid_0x00000000_0x0000D1A5_phys_0_eng_3")`: the pid plus a key
/// naming one engine of one adapter.
fn parse_engine_instance(name: &str) -> Option<(u32, &str)> {
    let rest = name.strip_prefix("pid_")?;
    let end = rest.find('_')?;
    let pid = rest[..end].parse::<u32>().ok()?;
    let start = name.find("luid_")?;
    let stop = name.rfind("_engtype_")?;
    if stop <= start {
        return None;
    }
    let key = &name[start..stop];
    if !key.contains("_eng_") {
        return None;
    }
    Some((pid, key))
}

/// Parses "pid_1234_luid_0x00000000_0x0000D1A5_phys_0" (a GPU Process Memory
/// instance) into the adapter key "luid_0x00000000_0x0000D1A5".
fn parse_adapter(name: &str) -> Option<&str> {
    let start = name.find("luid_")?;
    let rest = &name[start..];
    let end = rest.find("_phys").unwrap_or(rest.len());
    let key = &rest[..end];
    if key.len() > "luid_".len() {
        Some(key)
    } else {
        None
    }
}

/// The engine type at the end of a GPU Engine instance name:
/// "..._eng_3_engtype_VideoDecode" -> "VideoDecode".
fn engine_type(name: &str) -> Option<&str> {
    let at = name.rfind("_engtype_")?;
    let kind = &name[at + "_engtype_".len()..];
    (!kind.is_empty()).then_some(kind)
}

/// Usage per engine type from (raw type, usage summed over pids) per single
/// engine: the busiest engine of each type, like Task Manager's graphs.
/// Sorted 3D, Copy, Video Encode, Video Decode, then by name.
fn engine_types<'a>(engines: impl Iterator<Item = (&'a str, f64)>) -> Vec<GpuEngine> {
    let mut by_type: HashMap<&str, f64> = HashMap::new();
    for (kind, usage) in engines {
        if kind.is_empty() {
            continue;
        }
        let slot = by_type.entry(kind).or_insert(0.0);
        *slot = slot.max(usage);
    }
    let mut out: Vec<GpuEngine> = by_type
        .into_iter()
        .map(|(kind, usage)| GpuEngine {
            name: friendly_engine_name(kind),
            usage: (usage.clamp(0.0, 100.0) * 100.0).round() / 100.0,
        })
        .collect();
    out.sort_by(|a, b| {
        engine_rank(&a.name)
            .cmp(&engine_rank(&b.name))
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

fn engine_rank(name: &str) -> u8 {
    match name {
        "3D" => 0,
        "Copy" => 1,
        "Video Encode" => 2,
        "Video Decode" => 3,
        "Video Processing" => 4,
        n if n.starts_with("Compute") => 5,
        _ => 6,
    }
}

/// "VideoDecode" -> "Video Decode", "Compute_0" -> "Compute 0",
/// "LegacyOverlay" -> "Legacy Overlay"; "3D" and "VR" stay as they are.
fn friendly_engine_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 4);
    let mut prev: Option<char> = None;
    for c in raw.chars() {
        let c = if c == '_' { ' ' } else { c };
        if c == ' ' && matches!(prev, None | Some(' ')) {
            continue;
        }
        if c.is_ascii_uppercase() && prev.is_some_and(|p| p.is_ascii_lowercase()) {
            out.push(' ');
        }
        out.push(c);
        prev = Some(c);
    }
    out.trim_end().to_string()
}

/// Joins the three `PhysicalDisk(*)` arrays by instance name, leaving out
/// "_Total". A disk missing from the idle array has no active time; one
/// missing from a byte array reads 0 for it.
fn merge_disks(
    idle: Vec<(String, f64)>,
    read: Vec<(String, f64)>,
    write: Vec<(String, f64)>,
) -> Vec<DiskActivity> {
    fn entry(
        disks: &mut BTreeMap<String, DiskActivity>,
        name: String,
    ) -> Option<&mut DiskActivity> {
        if name.eq_ignore_ascii_case("_Total") {
            return None;
        }
        Some(disks.entry(name).or_insert_with_key(|name| DiskActivity {
            name: name.clone(),
            active_pct: None,
            read_bps: 0.0,
            write_bps: 0.0,
        }))
    }
    let mut disks: BTreeMap<String, DiskActivity> = BTreeMap::new();
    for (name, v) in idle {
        if let Some(d) = entry(&mut disks, name) {
            d.active_pct = Some(((100.0 - v).clamp(0.0, 100.0) * 100.0).round() / 100.0);
        }
    }
    for (name, v) in read {
        if let Some(d) = entry(&mut disks, name) {
            d.read_bps = v.max(0.0).round();
        }
    }
    for (name, v) in write {
        if let Some(d) = entry(&mut disks, name) {
            d.write_bps = v.max(0.0).round();
        }
    }
    disks.into_values().collect()
}

// ---------------------------------------------------------------------------
// Value readers
// ---------------------------------------------------------------------------

fn read_double(counter: PdhHandle) -> Option<f64> {
    let mut value = FmtCounterValue {
        c_status: 0,
        value: 0.0,
    };
    // SAFETY: `counter` belongs to a live query; the type out pointer is
    // optional (null); `value` is a valid, correctly laid out out pointer.
    let status = unsafe { PdhGetFormattedCounterValue(counter, FORMAT, null_mut(), &mut value) };
    if status == ERROR_SUCCESS && is_valid(value.c_status) && value.value.is_finite() {
        Some(value.value)
    } else {
        None
    }
}

/// Reads every instance of a wildcard counter as `(instance name, value)`.
/// Instances without valid data are skipped. `None` if the read failed.
fn read_array(counter: PdhHandle) -> Option<Vec<(String, f64)>> {
    // The instance set can grow between the sizing call and the read, so
    // retry a few times when PDH asks for a larger buffer.
    for _ in 0..4 {
        let mut size: u32 = 0;
        let mut count: u32 = 0;
        // SAFETY: sizing call; a null buffer with size 0 asks PDH for the
        // required byte count.
        let status = unsafe {
            PdhGetFormattedCounterArrayW(counter, FORMAT, &mut size, &mut count, null_mut())
        };
        if status == ERROR_SUCCESS {
            return Some(Vec::new());
        }
        if status != PDH_MORE_DATA || size == 0 {
            return None;
        }

        // u64 storage keeps the item array 8-byte aligned.
        let words = (size as usize).div_ceil(8);
        let mut buffer: Vec<u64> = vec![0; words];
        let items_ptr = buffer.as_mut_ptr().cast::<FmtCounterValueItem>();
        // SAFETY: `buffer` holds at least `size` bytes, aligned for the item
        // struct, and stays alive while PDH writes into it.
        let status = unsafe {
            PdhGetFormattedCounterArrayW(counter, FORMAT, &mut size, &mut count, items_ptr)
        };
        if status == PDH_MORE_DATA {
            continue;
        }
        if status != ERROR_SUCCESS {
            return None;
        }

        let available = (words * 8) / std::mem::size_of::<FmtCounterValueItem>();
        let count = (count as usize).min(available);
        // SAFETY: PDH wrote `count` items at the start of `buffer`; the
        // instance-name pointers point into the same buffer, which is alive
        // until this function returns, and are copied into owned Strings.
        let out = unsafe {
            std::slice::from_raw_parts(items_ptr.cast_const(), count)
                .iter()
                .filter(|item| is_valid(item.value.c_status) && item.value.value.is_finite())
                .map(|item| (wide_ptr_to_string(item.name), item.value.value))
                .collect()
        };
        return Some(out);
    }
    None
}

fn is_valid(c_status: u32) -> bool {
    c_status == PDH_CSTATUS_VALID_DATA || c_status == PDH_CSTATUS_NEW_DATA
}

fn to_u64(v: f64) -> u64 {
    if v.is_finite() && v > 0.0 {
        v.round() as u64
    } else {
        0
    }
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Copies a NUL-terminated UTF-16 string.
///
/// # Safety
/// `ptr` must be null or point to a NUL-terminated UTF-16 string.
unsafe fn wide_ptr_to_string(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    while *ptr.add(len) != 0 {
        len += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
}

#[cfg(test)]
mod tests {
    use super::{
        adapter_luid, engine_type, engine_types, friendly_engine_name, merge_disks, parse_adapter,
        parse_engine_instance,
    };

    #[test]
    fn names_engine_types() {
        assert_eq!(
            engine_type("pid_8_luid_0x0_0x1_phys_0_eng_0_engtype_VideoDecode"),
            Some("VideoDecode")
        );
        assert_eq!(
            engine_type("pid_8_luid_0x0_0x1_phys_0_eng_0_engtype_"),
            None
        );
        assert_eq!(engine_type("_Total"), None);
        assert_eq!(friendly_engine_name("3D"), "3D");
        assert_eq!(friendly_engine_name("VideoDecode"), "Video Decode");
        assert_eq!(friendly_engine_name("VideoEncode"), "Video Encode");
        assert_eq!(friendly_engine_name("LegacyOverlay"), "Legacy Overlay");
        assert_eq!(friendly_engine_name("Compute_0"), "Compute 0");
        assert_eq!(friendly_engine_name("OFA_0"), "OFA 0");
        assert_eq!(friendly_engine_name("VR"), "VR");
        assert_eq!(friendly_engine_name("High Priority 3D"), "High Priority 3D");
    }

    #[test]
    fn totals_engine_types_by_busiest_engine() {
        // Two Copy engines (each already summed over pids): the busier counts.
        let engines = engine_types(
            [
                ("Copy", 12.0),
                ("Copy", 30.0),
                ("VideoDecode", 4.5),
                ("3D", 140.0),
                ("Security", 0.0),
                ("", 50.0),
            ]
            .into_iter(),
        );
        let names: Vec<&str> = engines.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["3D", "Copy", "Video Decode", "Security"]);
        assert_eq!(engines[0].usage, 100.0);
        assert_eq!(engines[1].usage, 30.0);
        assert_eq!(engines[2].usage, 4.5);
    }

    #[test]
    fn merges_physical_disks() {
        let disks = merge_disks(
            vec![
                ("1 D: E:".into(), 87.5),
                ("0 F:".into(), 100.4),
                ("_Total".into(), 90.0),
            ],
            vec![("0 F:".into(), 2048.4), ("1 D: E:".into(), -1.0)],
            vec![("1 D: E:".into(), 512.0), ("2 C:".into(), 64.0)],
        );
        let names: Vec<&str> = disks.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["0 F:", "1 D: E:", "2 C:"]);
        assert_eq!(disks[0].active_pct, Some(0.0));
        assert_eq!(disks[0].read_bps, 2048.0);
        assert_eq!(disks[1].active_pct, Some(12.5));
        assert_eq!(disks[1].read_bps, 0.0);
        assert_eq!(disks[1].write_bps, 512.0);
        assert_eq!(disks[2].active_pct, None);
    }

    #[test]
    fn parses_engine_instance_names() {
        assert_eq!(
            parse_engine_instance("pid_1234_luid_0x00000000_0x0000D1A5_phys_0_eng_3_engtype_3D"),
            Some((1234, "luid_0x00000000_0x0000D1A5_phys_0_eng_3"))
        );
        assert_eq!(
            parse_engine_instance(
                "pid_8_luid_0x00000000_0x0000D1A5_phys_0_eng_0_engtype_VideoDecode"
            ),
            Some((8, "luid_0x00000000_0x0000D1A5_phys_0_eng_0"))
        );
        // Two engines of the same type stay two separate keys.
        assert_ne!(
            parse_engine_instance("pid_8_luid_0x0_0x1_phys_0_eng_3_engtype_Copy"),
            parse_engine_instance("pid_8_luid_0x0_0x1_phys_0_eng_4_engtype_Copy")
        );
        assert_eq!(parse_engine_instance("_Total"), None);
        assert_eq!(parse_engine_instance("pid_x_luid_engtype_3D"), None);
        assert_eq!(parse_engine_instance("pid_5_luid_0x0_0x1_engtype_3D"), None);
    }

    #[test]
    fn parses_adapter_keys() {
        assert_eq!(
            parse_adapter("pid_1234_luid_0x00000000_0x00016B80_phys_0"),
            Some("luid_0x00000000_0x00016B80")
        );
        assert_eq!(parse_adapter("_Total"), None);
    }

    #[test]
    fn adapter_keys_match_dxgi_luids() {
        assert_eq!(adapter_luid("luid_0x00000000_0x0000D1A5").as_deref(), Some("0x00000000_0x0000d1a5"));
        assert_eq!(adapter_luid("luid_0x1_0xA").as_deref(), Some("0x00000001_0x0000000a"));
        assert_eq!(adapter_luid("luid_0x00000000"), None);
        assert_eq!(adapter_luid("luid_0xZZ_0x1"), None);
    }
}
