//! Background sampling thread and the snapshot commands.
//!
//! One thread owns a sysinfo `System`, the PDH queries and the caches. Each
//! tick it builds a `Snapshot` and stores it in `AppState`, then emits
//! `"orbit://snapshot"`: the full `Snapshot`, or, once the UI opts in through
//! `set_sampling(delta: true)`, a `SnapshotDelta` (static process fields such
//! as the command line are then sent once per process). While paused
//! the thread stays alive and idle until `set_sampling` wakes it.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sysinfo::{
    CpuRefreshKind, DiskKind, Disks, MemoryRefreshKind, Networks, Pid, ProcessRefreshKind,
    ProcessStatus, ProcessesToUpdate, System, Uid, UpdateKind, Users,
    MINIMUM_CPU_UPDATE_INTERVAL,
};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::model::*;
use crate::pdh::{Pdh, PdhSample};
use crate::winenum::{self, ImagePaths, UserResolver, WindowInfo};

pub const SNAPSHOT_EVENT: &str = "orbit://snapshot";
pub const DEFAULT_INTERVAL_MS: u64 = 1000;
pub const MIN_INTERVAL_MS: u64 = 250;
pub const MAX_INTERVAL_MS: u64 = 10_000;

/// Rebuild the sysinfo user list this often (in ticks).
const USERS_REFRESH_TICKS: u64 = 30;
/// Refresh CPU frequency this often (in ticks).
const FREQ_REFRESH_TICKS: u64 = 5;
/// How long `get_snapshot` waits for the very first sample.
const FIRST_SAMPLE_TIMEOUT: Duration = Duration::from_secs(5);

/// Core Windows processes that may not live under %SystemRoot% or whose path
/// is unreadable without elevation. Lowercase.
const CORE_NAMES: &[&str] = &[
    "system",
    "system idle process",
    "registry",
    "smss.exe",
    "csrss.exe",
    "wininit.exe",
    "services.exe",
    "lsass.exe",
    "svchost.exe",
    "winlogon.exe",
    "dwm.exe",
    "fontdrvhost.exe",
    "memory compression",
    "secure system",
];

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// Managed Tauri state: the latest snapshot and the sampling controls.
pub struct AppState {
    /// Latest snapshot, `None` until the first sample completes.
    pub latest: Arc<Mutex<Option<Snapshot>>>,
    control: Arc<Control>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            latest: Arc::new(Mutex::new(None)),
            control: Arc::new(Control {
                interval_ms: AtomicU64::new(DEFAULT_INTERVAL_MS),
                paused: AtomicBool::new(false),
                delta: AtomicBool::new(false),
                wake: Mutex::new(false),
                wake_cv: Condvar::new(),
                ready_cv: Condvar::new(),
            }),
        }
    }
}

impl AppState {
    /// A copy of the latest snapshot, if one exists.
    pub fn latest_snapshot(&self) -> Option<Snapshot> {
        lock(&self.latest).clone()
    }
}

struct Control {
    interval_ms: AtomicU64,
    paused: AtomicBool,
    /// Emit `SnapshotDelta` instead of the full `Snapshot` (UI opt-in).
    delta: AtomicBool,
    /// Set by `set_sampling` so a sleeping sampler re-reads its settings.
    wake: Mutex<bool>,
    wake_cv: Condvar,
    /// Signalled (with `AppState::latest`) after every new snapshot.
    ready_cv: Condvar,
}

impl Control {
    fn interval(&self) -> Duration {
        Duration::from_millis(self.interval_ms.load(Ordering::Relaxed))
    }

    fn paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    /// Sleeps up to `duration`, returning early if `wake` is called.
    fn sleep(&self, duration: Duration) {
        let guard = lock(&self.wake);
        let (mut guard, _) = self
            .wake_cv
            .wait_timeout_while(guard, duration, |woken| !*woken)
            .unwrap_or_else(|e| e.into_inner());
        *guard = false;
    }

    fn wake(&self) {
        *lock(&self.wake) = true;
        self.wake_cv.notify_all();
    }
}

/// Locks a mutex, recovering the data if another thread panicked with it.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Latest snapshot. If the sampler has not produced one yet, waits for it.
#[tauri::command]
pub async fn get_snapshot(state: State<'_, AppState>) -> Result<Snapshot, String> {
    if let Some(snapshot) = state.latest_snapshot() {
        return Ok(snapshot);
    }
    let latest = Arc::clone(&state.latest);
    let control = Arc::clone(&state.control);
    let waited = tauri::async_runtime::spawn_blocking(move || {
        let guard = lock(&latest);
        let (guard, _) = control
            .ready_cv
            .wait_timeout_while(guard, FIRST_SAMPLE_TIMEOUT, |s| s.is_none())
            .unwrap_or_else(|e| e.into_inner());
        guard.clone()
    })
    .await
    .map_err(|_| "Could not read the system snapshot.".to_string())?;
    waited.ok_or_else(|| "Orbit is still reading the system. Try again in a moment.".to_string())
}

/// Facts that do not change while Orbit runs.
#[tauri::command]
pub async fn get_static_info() -> StaticInfo {
    let mut sys = System::new();
    sys.refresh_cpu_usage();
    sys.refresh_memory();

    let cpu_brand = sys
        .cpus()
        .first()
        .map(|c| c.brand().trim().to_string())
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| "Unknown processor".to_string());

    let disks = Disks::new_with_refreshed_list();
    let disks = disks
        .list()
        .iter()
        .map(|d| DiskInfo {
            name: d.name().to_string_lossy().into_owned(),
            mount: d.mount_point().to_string_lossy().into_owned(),
            file_system: d.file_system().to_string_lossy().into_owned(),
            total: d.total_space(),
            available: d.available_space(),
            kind: match d.kind() {
                DiskKind::SSD => "SSD",
                DiskKind::HDD => "HDD",
                _ => "Unknown",
            }
            .to_string(),
            removable: d.is_removable(),
        })
        .collect();

    StaticInfo {
        cpu_brand,
        logical_cores: sys.cpus().len() as u32,
        physical_cores: winenum::physical_core_count(),
        total_memory: sys.total_memory(),
        host_name: System::host_name(),
        os_name: System::long_os_version().or_else(System::name),
        os_version: System::os_version(),
        kernel_version: System::kernel_version(),
        is_admin: is_admin(),
        boot_time: System::boot_time(),
        disks,
        pid: std::process::id(),
    }
}

/// Changes the tick interval (clamped to 250..=10000 ms) and pause state.
/// `delta: true` switches the event payload to `SnapshotDelta` (the next
/// event then lists every process in `added`); `false` switches back to the
/// full `Snapshot`; omitted keeps the current mode. The default is the full
/// `Snapshot`, so a UI that does not know about deltas keeps working.
#[tauri::command]
pub fn set_sampling(
    state: State<'_, AppState>,
    interval_ms: u64,
    paused: bool,
    delta: Option<bool>,
) {
    let interval = interval_ms.clamp(MIN_INTERVAL_MS, MAX_INTERVAL_MS);
    state.control.interval_ms.store(interval, Ordering::Relaxed);
    state.control.paused.store(paused, Ordering::Relaxed);
    if let Some(delta) = delta {
        state.control.delta.store(delta, Ordering::Relaxed);
    }
    state.control.wake();
}

/// True if Orbit runs elevated.
fn is_admin() -> bool {
    // SAFETY: IsUserAnAdmin takes no arguments and only inspects the
    // current process token.
    unsafe { windows::Win32::UI::Shell::IsUserAnAdmin() }.as_bool()
}

// ---------------------------------------------------------------------------
// Thread
// ---------------------------------------------------------------------------

/// Starts the sampler thread. Called once from the Tauri setup hook.
pub fn start(app: AppHandle) {
    let (latest, control) = {
        let state = app.state::<AppState>();
        (Arc::clone(&state.latest), Arc::clone(&state.control))
    };
    let spawned = std::thread::Builder::new()
        .name("orbit-sampler".into())
        .spawn(move || run_loop(app, latest, control));
    if let Err(err) = spawned {
        eprintln!("Orbit could not start the sampler thread: {err}");
    }
}

fn run_loop(app: AppHandle, latest: Arc<Mutex<Option<Snapshot>>>, control: Arc<Control>) {
    // Created on this thread: the PDH handles inside are not Send.
    let mut sampler = Sampler::new();
    let mut last_sample: Option<Instant> = None;
    // pid -> start time of the rows sent in the previous event.
    let mut emitted: HashMap<u32, u64> = HashMap::new();
    let mut delta_mode = false;

    loop {
        let interval = control.interval().max(MINIMUM_CPU_UPDATE_INTERVAL);
        let paused = control.paused();
        let due = match last_sample {
            // The first snapshot is always taken, even when started paused.
            None => true,
            Some(at) => !paused && at.elapsed() >= interval,
        };

        if !due {
            let wait = if paused {
                Duration::from_secs(3600)
            } else {
                interval.saturating_sub(last_sample.map(|at| at.elapsed()).unwrap_or_default())
            };
            control.sleep(wait);
            continue;
        }

        let started = Instant::now();
        let suspended = read_suspended(&app);
        let snapshot = sampler.sample(&suspended, interval.as_millis() as u64);
        prune_suspended(&app, &snapshot);
        // Seconds this sample stands for in App history: the gap since the
        // last one, capped at two intervals so a long pause adds nothing odd.
        let covered = last_sample.map_or(0.0, |at| {
            started
                .duration_since(at)
                .as_secs_f64()
                .min(interval.as_secs_f64() * 2.0)
        });
        crate::history::record(&snapshot, covered);

        if control.delta.load(Ordering::Relaxed) {
            if !delta_mode {
                // Just switched on: the first delta carries every row.
                emitted.clear();
                delta_mode = true;
            }
            let delta = build_delta(&snapshot, &emitted);
            let _ = app.emit(SNAPSHOT_EVENT, &delta);
        } else {
            delta_mode = false;
            let _ = app.emit(SNAPSHOT_EVENT, &snapshot);
        }
        emitted = snapshot
            .processes
            .iter()
            .map(|r| (r.pid, r.start_time))
            .collect();
        *lock(&latest) = Some(snapshot);
        control.ready_cv.notify_all();
        last_sample = Some(started);
    }
}

/// The event payload for `snapshot`, relative to the rows sent last time.
fn build_delta<'a>(snapshot: &'a Snapshot, emitted: &HashMap<u32, u64>) -> SnapshotDelta<'a> {
    let current: HashMap<u32, u64> = snapshot
        .processes
        .iter()
        .map(|r| (r.pid, r.start_time))
        .collect();
    let mut removed: Vec<u32> = emitted
        .iter()
        .filter(|(pid, start)| current.get(pid) != Some(start))
        .map(|(pid, _)| *pid)
        .collect();
    removed.sort_unstable();
    SnapshotDelta {
        ts: snapshot.ts,
        interval_ms: snapshot.interval_ms,
        cpu: &snapshot.cpu,
        memory: &snapshot.memory,
        disk: &snapshot.disk,
        network: &snapshot.network,
        gpu: &snapshot.gpu,
        system: &snapshot.system,
        disks: &snapshot.disks,
        gpu_engines: &snapshot.gpu_engines,
        battery: snapshot.battery.as_ref(),
        added: snapshot
            .processes
            .iter()
            .filter(|r| emitted.get(&r.pid) != Some(&r.start_time))
            .collect(),
        removed,
        rows: snapshot.processes.iter().map(DynRow::from).collect(),
    }
}

fn read_suspended(app: &AppHandle) -> HashMap<u32, u64> {
    match app.try_state::<SuspendedSet>() {
        Some(set) => lock(&set.0).clone(),
        None => HashMap::new(),
    }
}

/// True when a suspended-set entry (recorded start time) belongs to the
/// process now shown with `row_start`. sysinfo reports 0 for processes it
/// cannot open; that is treated as "unknown", not as a mismatch.
fn suspended_matches(recorded: u64, row_start: u64) -> bool {
    row_start == 0 || same_start(recorded, row_start)
}

/// Forgets suspended processes that no longer exist (pid gone, or reused by
/// a process with another start time), so a reused pid is not shown as
/// suspended.
fn prune_suspended(app: &AppHandle, snapshot: &Snapshot) {
    if let Some(set) = app.try_state::<SuspendedSet>() {
        let mut set = lock(&set.0);
        if set.is_empty() {
            return;
        }
        let alive: HashMap<u32, u64> = snapshot
            .processes
            .iter()
            .map(|p| (p.pid, p.start_time))
            .collect();
        set.retain(|pid, recorded| {
            alive
                .get(pid)
                .is_some_and(|&start| suspended_matches(*recorded, start))
        });
    }
}

// ---------------------------------------------------------------------------
// Sampler
// ---------------------------------------------------------------------------

struct Sampler {
    sys: System,
    users: Users,
    pdh: Pdh,
    resolver: UserResolver,
    /// (pid, start time) -> owner name, resolved once per process.
    user_cache: HashMap<(u32, u64), Option<String>>,
    /// Image paths for processes sysinfo could not read (no access).
    image_paths: ImagePaths,
    /// (pid, start time) -> (name, image path) from `image_paths`, looked up
    /// once per process. The name guards against a reused pid whose start
    /// time is unknown (0).
    exe_cache: HashMap<(u32, u64), (String, Option<String>)>,
    /// (pid, start time) -> (name, Windows session), read once per process.
    /// The name guards a reused pid whose start time is unknown (0), as in
    /// `exe_cache`.
    session_cache: HashMap<(u32, u64), (String, Option<u32>)>,
    /// pid -> start time of processes seen last tick (for disk rate deltas).
    previous: HashMap<u32, u64>,
    /// Interface name -> (total rx, total tx) at the last tick.
    previous_net: HashMap<String, (u64, u64)>,
    last_refresh: Instant,
    tick: u64,
    /// Lowercase "%SystemRoot%\" prefix.
    system_root: String,
}

/// Per-tick refresh. exe, cmd and user are never re-read here: values already
/// read stay set, and processes that refused them once (no access) would
/// refuse again every tick. New processes get one try in `refresh_new`.
fn process_refresh_kind() -> ProcessRefreshKind {
    ProcessRefreshKind::everything()
        .without_environ()
        .without_cwd()
        .without_root()
        .with_exe(UpdateKind::Never)
        .with_cmd(UpdateKind::Never)
        .with_user(UpdateKind::Never)
}

/// One read of the static fields, for processes seen for the first time.
fn static_refresh_kind() -> ProcessRefreshKind {
    ProcessRefreshKind::nothing()
        .with_exe(UpdateKind::OnlyIfNotSet)
        .with_cmd(UpdateKind::OnlyIfNotSet)
        .with_user(UpdateKind::OnlyIfNotSet)
}

impl Sampler {
    fn new() -> Self {
        let mut sys = System::new();
        sys.refresh_cpu_usage();
        sys.refresh_cpu_frequency();
        sys.refresh_memory();
        // First full pass also reads paths, command lines and users.
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            process_refresh_kind()
                .with_exe(UpdateKind::OnlyIfNotSet)
                .with_cmd(UpdateKind::OnlyIfNotSet)
                .with_user(UpdateKind::OnlyIfNotSet),
        );
        let last_refresh = Instant::now();

        let previous = sys
            .processes()
            .iter()
            .map(|(pid, p)| (pid.as_u32(), p.start_time()))
            .collect();

        let previous_net = Networks::new_with_refreshed_list()
            .list()
            .iter()
            .map(|(name, data)| {
                (
                    name.clone(),
                    (data.total_received(), data.total_transmitted()),
                )
            })
            .collect();

        let pdh = Pdh::new();

        let mut system_root = std::env::var("SystemRoot")
            .unwrap_or_else(|_| r"C:\Windows".to_string())
            .to_lowercase();
        if !system_root.ends_with('\\') {
            system_root.push('\\');
        }

        // CPU usage needs two refreshes at least this far apart.
        std::thread::sleep(MINIMUM_CPU_UPDATE_INTERVAL);

        Self {
            sys,
            users: Users::new_with_refreshed_list(),
            pdh,
            resolver: UserResolver::new(),
            user_cache: HashMap::new(),
            image_paths: ImagePaths::new(),
            exe_cache: HashMap::new(),
            session_cache: HashMap::new(),
            previous,
            previous_net,
            last_refresh,
            tick: 0,
            system_root,
        }
    }

    fn sample(&mut self, suspended: &HashMap<u32, u64>, interval_ms: u64) -> Snapshot {
        self.tick += 1;
        let now = Instant::now();
        let elapsed = now
            .duration_since(self.last_refresh)
            .as_secs_f64()
            .max(0.05);
        self.last_refresh = now;

        // One refresh call: on Windows every CPU refresh re-collects the PDH
        // query and rewrites usage, so a separate frequency refresh right
        // after the usage refresh would read a near-zero interval (100%).
        let mut cpu_kind = CpuRefreshKind::nothing().with_cpu_usage();
        if self.tick % FREQ_REFRESH_TICKS == 0 {
            cpu_kind = cpu_kind.with_frequency();
        }
        self.sys.refresh_cpu_specifics(cpu_kind);
        // RAM only: sysinfo's swap figures come from K32GetPerformanceInfo,
        // which is slow; swap is derived from PDH commit counters below.
        self.sys
            .refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());
        self.sys
            .refresh_processes_specifics(ProcessesToUpdate::All, true, process_refresh_kind());
        self.refresh_new();
        if self.tick % USERS_REFRESH_TICKS == 0 {
            self.users = Users::new_with_refreshed_list();
        }

        let mut pdh = self.pdh.sample();
        let windows = winenum::top_level_windows();
        let network = self.sample_network(elapsed);

        // CPU
        let per_core: Vec<f64> = self
            .sys
            .cpus()
            .iter()
            .map(|c| round2(c.cpu_usage() as f64))
            .collect();
        let usage = if per_core.is_empty() {
            0.0
        } else {
            round2(per_core.iter().sum::<f64>() / per_core.len() as f64)
        };
        let freq_mhz = self.sys.cpus().first().map(|c| c.frequency()).unwrap_or(0);
        let cores = self.sys.cpus().len().max(1) as f64;

        // Processes
        let processes = self.build_rows(&pdh, &windows, suspended, elapsed, cores);

        // Swap the way sysinfo computes it (commit minus physical RAM), from
        // the PDH commit counters already read this tick.
        let phys = self.sys.total_memory();
        let (swap_total, swap_used) = match (pdh.commit_limit, pdh.committed) {
            (Some(limit), Some(committed)) => {
                let total = limit.saturating_sub(phys);
                (total, committed.saturating_sub(phys).min(total))
            }
            _ => {
                self.sys
                    .refresh_memory_specifics(MemoryRefreshKind::nothing().with_swap());
                (self.sys.total_swap(), self.sys.used_swap())
            }
        };

        // Task Manager's "Cached": standby (all bands) + modified.
        let cached = match (pdh.standby_core, pdh.standby_normal, pdh.standby_reserve, pdh.modified)
        {
            (Some(a), Some(b), Some(c), Some(d)) => Some(a + b + c + d),
            _ => None,
        };

        let memory = MemoryStats {
            total: phys,
            used: self.sys.used_memory(),
            available: self.sys.available_memory(),
            swap_total,
            swap_used,
            committed: pdh.committed,
            commit_limit: pdh.commit_limit,
            cached,
            free: pdh.free_zero,
        };

        // Physical disk only. The per-process I/O sum is not a fallback: it
        // includes network and device I/O.
        let disk = DiskStats {
            active_pct: pdh.disk_active_pct.map(round2),
            read_bps: pdh.disk_read_bps.map(f64::round),
            write_bps: pdh.disk_write_bps.map(f64::round),
        };

        let gpu = GpuStats {
            usage: pdh.gpu_total.map(round2),
            dedicated_used: pdh.gpu_dedicated,
        };

        let system = SystemStats {
            uptime_secs: System::uptime(),
            process_count: processes.len() as u32,
            thread_count: pdh.threads,
            handle_count: pdh.handles,
        };

        Snapshot {
            ts: now_millis(),
            interval_ms,
            cpu: CpuStats {
                usage,
                per_core,
                freq_mhz,
            },
            memory,
            disk,
            network,
            gpu,
            system,
            disks: std::mem::take(&mut pdh.disks),
            gpu_engines: std::mem::take(&mut pdh.gpu_engines),
            battery: winenum::battery_status(),
            processes,
        }
    }

    /// Reads exe, cmd and user once for processes that were not there last
    /// tick (the per-tick refresh never re-reads them).
    fn refresh_new(&mut self) {
        let new: Vec<Pid> = self
            .sys
            .processes()
            .iter()
            .filter(|(pid, p)| self.previous.get(&pid.as_u32()) != Some(&p.start_time()))
            .map(|(pid, _)| *pid)
            .collect();
        if !new.is_empty() {
            self.sys.refresh_processes_specifics(
                ProcessesToUpdate::Some(&new),
                false,
                static_refresh_kind(),
            );
        }
    }

    /// Builds one row per process.
    fn build_rows(
        &mut self,
        pdh: &PdhSample,
        windows: &HashMap<u32, WindowInfo>,
        suspended: &HashMap<u32, u64>,
        elapsed: f64,
        cores: f64,
    ) -> Vec<ProcessRow> {
        let gpu_available = pdh.gpu_total.is_some();
        // Per-process network bytes/sec from network.rs; `None` leaves the
        // row fields null.
        let net_rates = crate::network::per_app_rates();
        let mut wts_sessions: Option<HashMap<u32, u32>> = None;
        let mut rows = Vec::with_capacity(self.sys.processes().len());
        let mut seen: HashMap<u32, u64> = HashMap::with_capacity(self.sys.processes().len());

        for (pid, p) in self.sys.processes() {
            let pid = pid.as_u32();
            let start_time = p.start_time();
            // Disk counters of a process seen for the first time hold its
            // lifetime totals, not a per-tick delta.
            let known = self.previous.get(&pid) == Some(&start_time);
            seen.insert(pid, start_time);

            let usage = p.disk_usage();
            let (read_bps, write_bps) = if known {
                (
                    usage.read_bytes as f64 / elapsed,
                    usage.written_bytes as f64 / elapsed,
                )
            } else {
                (0.0, 0.0)
            };

            let name = p.name().to_string_lossy().into_owned();
            let exe = p
                .exe()
                .map(|e| e.to_string_lossy().into_owned())
                .filter(|e| !e.is_empty())
                .or_else(|| {
                    let key = (pid, start_time);
                    match self.exe_cache.get(&key) {
                        Some((cached_name, path)) if *cached_name == name => path.clone(),
                        _ => {
                            let path = self.image_paths.lookup(pid);
                            self.exe_cache.insert(key, (name.clone(), path.clone()));
                            path
                        }
                    }
                });
            let window = windows.get(&pid);
            let kind = classify(
                pid,
                &name,
                exe.as_deref(),
                window.is_some(),
                &self.system_root,
            );

            let user = resolve_user(
                &self.users,
                &mut self.user_cache,
                &mut self.resolver,
                pid,
                start_time,
                p.user_id(),
            );

            let gpu = if gpu_available {
                Some(round2(pdh.gpu_per_pid.get(&pid).copied().unwrap_or(0.0)))
            } else {
                None
            };

            // ProcessIdToSessionId first; for processes it refuses (SYSTEM
            // ones, unelevated) one WTS process list, at most once per tick.
            let session_id = match self.session_cache.get(&(pid, start_time)) {
                Some((cached_name, id)) if *cached_name == name => *id,
                _ => {
                    let id = winenum::process_session_id(pid).or_else(|| {
                        wts_sessions
                            .get_or_insert_with(winenum::all_session_ids)
                            .get(&pid)
                            .copied()
                    });
                    self.session_cache
                        .insert((pid, start_time), (name.clone(), id));
                    id
                }
            };

            rows.push(ProcessRow {
                pid,
                parent_pid: p.parent().map(|pp| pp.as_u32()),
                name,
                exe,
                cmd: join_command_line(p.cmd()),
                user,
                cpu: round2((p.cpu_usage() as f64 / cores).clamp(0.0, 100.0)),
                memory: p.memory(),
                disk_read_bps: read_bps.round(),
                disk_write_bps: write_bps.round(),
                gpu,
                status: status_name(p.status()).to_string(),
                start_time,
                kind,
                window_title: window.map(|w| w.title.clone()),
                not_responding: window.map_or(false, |w| w.not_responding),
                suspended: suspended
                    .get(&pid)
                    .is_some_and(|&recorded| suspended_matches(recorded, start_time)),
                session_id,
                // With per-app data on, a process with no traffic reads 0, not null.
                net_rx_bps: net_rates
                    .as_ref()
                    .map(|m| m.get(&pid).map_or(0.0, |&(rx, _)| rx.round())),
                net_tx_bps: net_rates
                    .as_ref()
                    .map(|m| m.get(&pid).map_or(0.0, |&(_, tx)| tx.round())),
            });
        }

        self.user_cache
            .retain(|(pid, start), _| seen.get(pid) == Some(start));
        self.exe_cache
            .retain(|(pid, start), _| seen.get(pid) == Some(start));
        self.session_cache
            .retain(|(pid, start), _| seen.get(pid) == Some(start));
        self.previous = seen;
        rows.sort_unstable_by_key(|r| r.pid);
        rows
    }

    fn sample_network(&mut self, elapsed: f64) -> NetworkStats {
        let networks = Networks::new_with_refreshed_list();
        let mut next: HashMap<String, (u64, u64)> = HashMap::new();
        let mut interfaces = Vec::new();
        let mut rx_bps = 0.0;
        let mut tx_bps = 0.0;

        for (name, data) in networks.list() {
            if is_filter_interface(name) {
                continue;
            }
            let total_rx = data.total_received();
            let total_tx = data.total_transmitted();
            let (rx, tx) = match self.previous_net.get(name) {
                Some(&(prev_rx, prev_tx)) => (
                    total_rx.saturating_sub(prev_rx) as f64 / elapsed,
                    total_tx.saturating_sub(prev_tx) as f64 / elapsed,
                ),
                None => (0.0, 0.0),
            };
            next.insert(name.clone(), (total_rx, total_tx));
            // Interfaces that never carried traffic are noise in the UI.
            if total_rx == 0 && total_tx == 0 {
                continue;
            }
            rx_bps += rx;
            tx_bps += tx;
            interfaces.push(InterfaceStats {
                name: name.clone(),
                rx_bps: rx.round(),
                tx_bps: tx.round(),
                total_rx,
                total_tx,
            });
        }

        interfaces.sort_by(|a, b| (b.total_rx + b.total_tx).cmp(&(a.total_rx + a.total_tx)));
        self.previous_net = next;
        NetworkStats {
            rx_bps: rx_bps.round(),
            tx_bps: tx_bps.round(),
            interfaces,
            per_app: crate::network::per_app_status(),
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Task Manager style grouping. A visible window makes any process an app,
/// even explorer.exe.
fn classify(
    pid: u32,
    name: &str,
    exe: Option<&str>,
    has_window: bool,
    system_root: &str,
) -> ProcessKind {
    if has_window {
        return ProcessKind::App;
    }
    if pid == 0 || pid == 4 {
        return ProcessKind::Windows;
    }
    let lower_name = name.to_lowercase();
    if CORE_NAMES.contains(&lower_name.as_str()) {
        return ProcessKind::Windows;
    }
    if let Some(exe) = exe {
        if exe.to_lowercase().starts_with(system_root) {
            return ProcessKind::Windows;
        }
    }
    ProcessKind::Background
}

/// Owner name: sysinfo's user list first (local accounts), then the process
/// token (service and system accounts). Cached per process lifetime.
fn resolve_user(
    users: &Users,
    cache: &mut HashMap<(u32, u64), Option<String>>,
    resolver: &mut UserResolver,
    pid: u32,
    start_time: u64,
    uid: Option<&Uid>,
) -> Option<String> {
    if let Some(cached) = cache.get(&(pid, start_time)) {
        return cached.clone();
    }
    let name = uid
        .and_then(|uid| users.get_user_by_id(uid))
        .map(|user| user.name().to_string())
        .filter(|n| !n.is_empty())
        .or_else(|| resolver.resolve(pid));
    cache.insert((pid, start_time), name.clone());
    name
}

/// Joins argv back into one line, quoting arguments that contain whitespace.
/// Shared with the details drawer so both show the same command line.
pub(crate) fn join_command_line(args: &[std::ffi::OsString]) -> String {
    let parts: Vec<Cow<'_, str>> = args
        .iter()
        .map(|arg| {
            let s = arg.to_string_lossy();
            let needs_quotes = s.is_empty() || s.chars().any(char::is_whitespace);
            if needs_quotes && !(s.starts_with('"') && s.ends_with('"') && s.len() > 1) {
                Cow::Owned(format!("\"{s}\""))
            } else {
                s
            }
        })
        .collect();
    parts.join(" ")
}

fn status_name(status: ProcessStatus) -> &'static str {
    match status {
        ProcessStatus::Run => "running",
        ProcessStatus::Sleep | ProcessStatus::Idle => "sleeping",
        ProcessStatus::Stop => "stopped",
        ProcessStatus::Zombie => "zombie",
        ProcessStatus::Dead => "dead",
        _ => "unknown",
    }
}

/// Windows lists lightweight filter and scheduler layers as extra interfaces
/// that mirror a real adapter's traffic; counting them would double totals.
fn is_filter_interface(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.contains("loopback")
        || lower.contains("filter-0")
        || lower.contains("packet scheduler")
        || lower.contains("wfp ")
        || lower.contains("-wfp")
        || lower.contains("lightweight filter")
}

fn round2(v: f64) -> f64 {
    if v.is_finite() {
        (v * 100.0).round() / 100.0
    } else {
        0.0
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_like_task_manager() {
        let root = r"c:\windows\";
        assert_eq!(
            classify(4, "System", None, false, root),
            ProcessKind::Windows
        );
        assert_eq!(
            classify(900, "svchost.exe", None, false, root),
            ProcessKind::Windows
        );
        assert_eq!(
            classify(
                1200,
                "explorer.exe",
                Some(r"C:\Windows\explorer.exe"),
                true,
                root
            ),
            ProcessKind::App
        );
        assert_eq!(
            classify(
                1200,
                "explorer.exe",
                Some(r"C:\Windows\explorer.exe"),
                false,
                root
            ),
            ProcessKind::Windows
        );
        assert_eq!(
            classify(
                3000,
                "node.exe",
                Some(r"C:\Program Files\nodejs\node.exe"),
                false,
                root
            ),
            ProcessKind::Background
        );
    }

    #[test]
    fn joins_command_lines_with_quotes() {
        let args: Vec<std::ffi::OsString> = vec![
            r"C:\Program Files\App\app.exe".into(),
            "--flag".into(),
            "two words".into(),
        ];
        assert_eq!(
            join_command_line(&args),
            r#""C:\Program Files\App\app.exe" --flag "two words""#
        );
        assert_eq!(join_command_line(&[]), "");
    }

    /// Tick cost harness (reads the live machine, so not run by default):
    /// `cargo test tick_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing harness on the live machine"]
    fn tick_cost() {
        let mut sampler = Sampler::new();
        let none = HashMap::new();
        let first = Instant::now();
        sampler.sample(&none, 1000);
        println!(
            "tick_cost: first tick {:.1} ms",
            first.elapsed().as_secs_f64() * 1000.0
        );
        let ticks = 10;
        let mut total = Duration::ZERO;
        let mut worst = Duration::ZERO;
        let mut last = None;
        for _ in 0..ticks {
            std::thread::sleep(Duration::from_millis(1000));
            let started = Instant::now();
            let snapshot = sampler.sample(&none, 1000);
            let took = started.elapsed();
            total += took;
            worst = worst.max(took);
            last = Some(snapshot);
        }
        let snapshot = last.unwrap();
        println!(
            "tick_cost: avg {:.1} ms, max {:.1} ms over {ticks} ticks, {} processes",
            total.as_secs_f64() * 1000.0 / ticks as f64,
            worst.as_secs_f64() * 1000.0,
            snapshot.processes.len()
        );
        println!("disks: {:?}", snapshot.disks);
        println!("gpu_engines: {:?}", snapshot.gpu_engines);
        println!("battery: {:?}", snapshot.battery);
        let with_session = snapshot
            .processes
            .iter()
            .filter(|r| r.session_id.is_some())
            .count();
        let missing: Vec<String> = snapshot
            .processes
            .iter()
            .filter(|r| r.session_id.is_none())
            .map(|r| format!("{} ({})", r.name, r.pid))
            .take(20)
            .collect();
        let mut sessions: Vec<u32> = snapshot
            .processes
            .iter()
            .filter_map(|r| r.session_id)
            .collect();
        sessions.sort_unstable();
        sessions.dedup();
        println!("sessions seen: {sessions:?}");
        println!(
            "session ids: {with_session}/{} set; missing: {missing:?}",
            snapshot.processes.len()
        );
    }

    #[test]
    fn filters_virtual_interfaces() {
        assert!(is_filter_interface(
            "Wi-Fi-WFP Native MAC Layer LightWeight Filter-0000"
        ));
        assert!(is_filter_interface("Ethernet-QoS Packet Scheduler-0000"));
        assert!(is_filter_interface("Loopback Pseudo-Interface 1"));
        assert!(!is_filter_interface("Wi-Fi"));
        assert!(!is_filter_interface("Ethernet"));
    }
}
