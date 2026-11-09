//! App history: Orbit's own per-app usage totals, kept by day.
//!
//! The sampler calls `record` after every tick. Each process row is added to
//! its app (key: lowercased exe path, else lowercased name) in memory. What
//! was recorded since the last write is merged into one JSON file per local
//! day, `<dir>/YYYY-MM-DD.json`, at most once a minute, at midnight and on
//! exit. Files older than 90 days are deleted at start and at each new day.
//! No admin rights and no SRUM: only what Orbit saw while it ran.
//!
//! Units, per tick of `interval` seconds, summed over the app's processes:
//! - CPU time: seconds of one logical core. A row's `cpu` is percent of the
//!   whole machine, so a tick adds `cpu / 100 * logical cores * interval`
//!   (one core busy for an hour is 3600 s on any PC).
//! - Memory: the sum of the working sets. Max is the largest tick; average is
//!   weighted by time over the seconds the app was running.
//! - I/O and network bytes: `rate * interval` (network only while per-app
//!   rates are available, so 0 without admin).
//! - Active time: seconds when the app used more than 0.5% CPU or had a window.
//!
//! `dir` is `ORBIT_HISTORY_DIR` when set (tests use a scratch folder), else
//! `<app local data>/history`, i.e. `%LOCALAPPDATA%\me.legacytech.orbit\history`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::model::{ProcessKind, Snapshot};

/// Days of files kept (today included).
pub const KEEP_DAYS: i64 = 90;
/// Minimum time between two writes while recording.
const FLUSH_EVERY_MS: u64 = 60_000;
/// An app counts as active above this CPU (percent of the whole machine).
const ACTIVE_CPU_PCT: f64 = 0.5;
/// Forget exited processes every this many ticks.
const PRUNE_EVERY_TICKS: u64 = 64;
const FILE_VERSION: u32 = 1;
const NO_DAY: i64 = i64::MIN;

static ENABLED: AtomicBool = AtomicBool::new(true);
static APP_DIR: OnceLock<PathBuf> = OnceLock::new();
/// When the pending totals were last written (ms since the epoch, 0 = not yet).
static LAST_FLUSH_MS: AtomicU64 = AtomicU64::new(0);
/// The day the in-memory totals belong to, `NO_DAY` before the first tick.
static PENDING_DAY: AtomicI64 = AtomicI64::new(NO_DAY);
static STATE: Mutex<Option<Recorder>> = Mutex::new(None);
/// Held around every file read or write. Always taken before `STATE`.
static IO: Mutex<()> = Mutex::new(());

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Totals for one app over the requested days.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct HistoryRow {
    /// Stable key: lowercased exe path, or the lowercased name when the path
    /// is unknown.
    pub key: String,
    pub name: String,
    pub exe: Option<String>,
    /// Seconds of one logical core.
    pub cpu_seconds: f64,
    /// Bytes.
    pub max_memory: u64,
    pub avg_memory: u64,
    pub disk_read_bytes: u64,
    pub disk_write_bytes: u64,
    /// 0 while per-app network data is unavailable.
    pub net_rx_bytes: u64,
    pub net_tx_bytes: u64,
    /// Seconds the app used more than 0.5% CPU or had a window.
    pub active_seconds: f64,
    /// Seconds since the Unix epoch.
    pub first_seen: u64,
    pub last_seen: u64,
}

/// One app's totals for one day, as stored in the day file.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
struct DayApp {
    key: String,
    name: String,
    exe: Option<String>,
    cpu_seconds: f64,
    /// Working-set bytes times seconds, for the time-weighted average.
    mem_byte_seconds: f64,
    /// Seconds the app had at least one process.
    run_seconds: f64,
    max_memory: f64,
    disk_read_bytes: f64,
    disk_write_bytes: f64,
    net_rx_bytes: f64,
    net_tx_bytes: f64,
    active_seconds: f64,
    first_seen: u64,
    last_seen: u64,
}

impl DayApp {
    fn blank(key: String, name: String, exe: Option<String>) -> Self {
        DayApp { key, name, exe, ..Default::default() }
    }

    /// Nothing recorded since it was created or last written.
    fn is_empty(&self) -> bool {
        self.last_seen == 0
    }

    fn add(&mut self, o: &DayApp) {
        if o.is_empty() {
            return;
        }
        if o.last_seen >= self.last_seen {
            // The newest sighting names the app.
            self.name.clone_from(&o.name);
            self.exe.clone_from(&o.exe);
        }
        self.cpu_seconds += o.cpu_seconds;
        self.mem_byte_seconds += o.mem_byte_seconds;
        self.run_seconds += o.run_seconds;
        self.max_memory = self.max_memory.max(o.max_memory);
        self.disk_read_bytes += o.disk_read_bytes;
        self.disk_write_bytes += o.disk_write_bytes;
        self.net_rx_bytes += o.net_rx_bytes;
        self.net_tx_bytes += o.net_tx_bytes;
        self.active_seconds += o.active_seconds;
        if self.first_seen == 0 || (o.first_seen != 0 && o.first_seen < self.first_seen) {
            self.first_seen = o.first_seen;
        }
        self.last_seen = self.last_seen.max(o.last_seen);
    }

    /// Zero the counters, keep the identity.
    fn clear_counts(&mut self) {
        let key = std::mem::take(&mut self.key);
        let name = std::mem::take(&mut self.name);
        let exe = self.exe.take();
        *self = DayApp::blank(key, name, exe);
    }

    fn to_row(&self) -> HistoryRow {
        let avg = if self.run_seconds > 0.0 {
            self.mem_byte_seconds / self.run_seconds
        } else {
            self.max_memory
        };
        HistoryRow {
            key: self.key.clone(),
            name: self.name.clone(),
            exe: self.exe.clone(),
            cpu_seconds: (self.cpu_seconds * 1000.0).round() / 1000.0,
            max_memory: whole(self.max_memory),
            avg_memory: whole(avg.min(self.max_memory)),
            disk_read_bytes: whole(self.disk_read_bytes),
            disk_write_bytes: whole(self.disk_write_bytes),
            net_rx_bytes: whole(self.net_rx_bytes),
            net_tx_bytes: whole(self.net_tx_bytes),
            active_seconds: self.active_seconds.round(),
            first_seen: self.first_seen,
            last_seen: self.last_seen,
        }
    }
}

fn whole(v: f64) -> u64 {
    if v.is_finite() && v > 0.0 {
        v.round() as u64
    } else {
        0
    }
}

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct DayFile {
    version: u32,
    date: String,
    apps: Vec<DayApp>,
}

// ---------------------------------------------------------------------------
// In-memory recorder
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Slot {
    app: usize,
    seen: u64,
}

/// One app's sums within the current tick.
#[derive(Clone, Copy, Default)]
struct TickAcc {
    tick: u64,
    cpu: f64,
    mem: u64,
    rd: f64,
    wr: f64,
    rx: f64,
    tx: f64,
    window: bool,
}

/// Totals since the last write, for one day. Allocation-free per tick once
/// the processes are known: a process is looked up by (pid, start time) and
/// only a new one builds its key.
struct Recorder {
    day: i64,
    apps: Vec<DayApp>,
    index: HashMap<String, usize>,
    procs: HashMap<(u32, u64), Slot>,
    ticks: Vec<TickAcc>,
    touched: Vec<usize>,
    tick: u64,
}

impl Recorder {
    fn new(day: i64) -> Self {
        Recorder {
            day,
            apps: Vec::new(),
            index: HashMap::new(),
            procs: HashMap::new(),
            ticks: Vec::new(),
            touched: Vec::new(),
            tick: 0,
        }
    }

    fn reset(&mut self, day: i64) {
        self.day = day;
        self.apps.clear();
        self.index.clear();
        self.procs.clear();
        self.ticks.clear();
        self.touched.clear();
    }

    fn app_for(&mut self, pid: u32, start: u64, name: &str, exe: Option<&str>) -> usize {
        let tick = self.tick;
        if let Some(slot) = self.procs.get_mut(&(pid, start)) {
            slot.seen = tick;
            return slot.app;
        }
        let key = match exe {
            Some(p) if !p.is_empty() => p.to_lowercase(),
            _ => name.to_lowercase(),
        };
        let app = match self.index.get(&key) {
            Some(&i) => i,
            None => {
                let i = self.apps.len();
                self.apps.push(DayApp::blank(key.clone(), name.to_string(), exe.map(str::to_string)));
                self.ticks.push(TickAcc::default());
                self.index.insert(key, i);
                i
            }
        };
        self.procs.insert((pid, start), Slot { app, seen: tick });
        app
    }

    /// Adds one snapshot standing for `interval` seconds, seen at `now`.
    fn add_tick(&mut self, snapshot: &Snapshot, interval: f64, now: u64) {
        self.tick += 1;
        let tick = self.tick;
        let cores = snapshot.cpu.per_core.len().max(1) as f64;
        self.touched.clear();
        for row in &snapshot.processes {
            if row.pid == 0 {
                // System Idle Process: its "CPU" is idle time.
                continue;
            }
            let app = self.app_for(row.pid, row.start_time, &row.name, row.exe.as_deref());
            let t = &mut self.ticks[app];
            if t.tick != tick {
                *t = TickAcc { tick, ..Default::default() };
                self.touched.push(app);
            }
            t.cpu += finite(row.cpu);
            t.mem += row.memory;
            t.rd += finite(row.disk_read_bps);
            t.wr += finite(row.disk_write_bps);
            t.rx += row.net_rx_bps.map_or(0.0, finite);
            t.tx += row.net_tx_bps.map_or(0.0, finite);
            t.window |= row.kind == ProcessKind::App;
        }
        for &i in &self.touched {
            let t = self.ticks[i];
            let a = &mut self.apps[i];
            a.cpu_seconds += t.cpu / 100.0 * cores * interval;
            a.mem_byte_seconds += t.mem as f64 * interval;
            a.run_seconds += interval;
            a.max_memory = a.max_memory.max(t.mem as f64);
            a.disk_read_bytes += t.rd * interval;
            a.disk_write_bytes += t.wr * interval;
            a.net_rx_bytes += t.rx * interval;
            a.net_tx_bytes += t.tx * interval;
            if t.cpu > ACTIVE_CPU_PCT || t.window {
                a.active_seconds += interval;
            }
            if a.first_seen == 0 {
                a.first_seen = now;
            }
            a.last_seen = now;
        }
        if tick % PRUNE_EVERY_TICKS == 0 {
            self.procs.retain(|_, s| s.seen == tick);
        }
    }

    /// The totals since the last call (apps seen since then), zeroing them.
    fn take(&mut self) -> Vec<DayApp> {
        let mut out = Vec::new();
        for a in &mut self.apps {
            if !a.is_empty() {
                out.push(a.clone());
                a.clear_counts();
            }
        }
        out
    }

    fn pending(&self) -> impl Iterator<Item = &DayApp> {
        self.apps.iter().filter(|a| !a.is_empty())
    }
}

fn finite(v: f64) -> f64 {
    if v.is_finite() && v > 0.0 {
        v
    } else {
        0.0
    }
}

// ---------------------------------------------------------------------------
// Dates (local days, no date crate)
// ---------------------------------------------------------------------------

/// Days since 1970-01-01 for a proleptic Gregorian date (H. Hinnant).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn file_name(day: i64) -> String {
    let (y, m, d) = civil_from_days(day);
    format!("{y:04}-{m:02}-{d:02}.json")
}

/// The day of a "YYYY-MM-DD.json" file name, `None` for anything else.
fn parse_file_name(name: &str) -> Option<i64> {
    let stem = name.strip_suffix(".json")?;
    let b = stem.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    if !b.iter().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit()) {
        return None;
    }
    let y: i64 = stem[0..4].parse().ok()?;
    let m: u32 = stem[5..7].parse().ok()?;
    let d: u32 = stem[8..10].parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let day = days_from_civil(y, m, d);
    // Reject 2026-02-31 and the like.
    (civil_from_days(day) == (y, m, d)).then_some(day)
}

fn systemtime_secs(t: &windows::Win32::Foundation::SYSTEMTIME) -> i64 {
    days_from_civil(t.wYear as i64, t.wMonth as u32, t.wDay as u32) * 86_400
        + t.wHour as i64 * 3600
        + t.wMinute as i64 * 60
        + t.wSecond as i64
}

/// Local time minus UTC, in seconds (follows daylight saving changes).
fn local_offset_secs() -> i64 {
    use windows::Win32::System::SystemInformation::{GetLocalTime, GetSystemTime};
    // SAFETY: both only fill a SYSTEMTIME.
    let (local, utc) = unsafe { (GetLocalTime(), GetSystemTime()) };
    let diff = systemtime_secs(&local) - systemtime_secs(&utc);
    // The two calls are microseconds apart: round to whole minutes.
    (diff as f64 / 60.0).round() as i64 * 60
}

fn local_day(unix_secs: i64) -> i64 {
    (unix_secs + local_offset_secs()).div_euclid(86_400)
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

fn history_dir() -> Option<PathBuf> {
    match std::env::var_os("ORBIT_HISTORY_DIR") {
        Some(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ => APP_DIR.get().cloned(),
    }
}

fn read_day(dir: &Path, day: i64) -> Option<DayFile> {
    let path = dir.join(file_name(day));
    let text = fs::read(&path).ok()?;
    match serde_json::from_slice::<DayFile>(&text) {
        Ok(f) => Some(f),
        Err(err) => {
            eprintln!("Orbit history: skipping {}: {err}", path.display());
            None
        }
    }
}

/// Adds `apps` to the day's file (created if missing), written atomically.
fn merge_into_file(dir: &Path, day: i64, apps: &[DayApp]) -> std::io::Result<()> {
    if apps.is_empty() {
        return Ok(());
    }
    fs::create_dir_all(dir)?;
    let mut file = read_day(dir, day).unwrap_or_default();
    let mut index: HashMap<String, usize> = HashMap::with_capacity(file.apps.len() + apps.len());
    for (i, a) in file.apps.iter().enumerate() {
        index.insert(a.key.clone(), i);
    }
    for a in apps {
        match index.get(&a.key) {
            Some(&i) => file.apps[i].add(a),
            None => {
                index.insert(a.key.clone(), file.apps.len());
                let mut fresh = DayApp::blank(a.key.clone(), a.name.clone(), a.exe.clone());
                fresh.add(a);
                file.apps.push(fresh);
            }
        }
    }
    file.version = FILE_VERSION;
    file.date = file_name(day).trim_end_matches(".json").to_string();
    let json = serde_json::to_vec(&file).map_err(std::io::Error::other)?;
    let path = dir.join(file_name(day));
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json)?;
    fs::rename(&tmp, &path)
}

/// Merges the files of days `from..=to` into `out` by key.
fn read_range(dir: &Path, from: i64, to: i64, out: &mut HashMap<String, DayApp>) {
    for day in from..=to {
        if let Some(file) = read_day(dir, day) {
            for a in &file.apps {
                merge_app(out, a);
            }
        }
    }
}

fn merge_app(out: &mut HashMap<String, DayApp>, a: &DayApp) {
    if a.is_empty() {
        return;
    }
    out.entry(a.key.clone())
        .or_insert_with(|| DayApp::blank(a.key.clone(), a.name.clone(), a.exe.clone()))
        .add(a);
}

/// Day files in `dir`: (day, path). Ignores everything else.
fn day_files(dir: &Path) -> Vec<(i64, PathBuf)> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            let day = parse_file_name(name.to_str()?)?;
            Some((day, e.path()))
        })
        .collect()
}

/// Deletes the day files older than `KEEP_DAYS` (today counts as one).
fn prune(dir: &Path, today: i64) {
    for (day, path) in day_files(dir) {
        if day <= today - KEEP_DAYS {
            let _ = fs::remove_file(path);
        }
    }
}

/// Deletes every day file (and a half-written one) in `dir`.
fn delete_all(dir: &Path) -> std::io::Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    let mut failed = None;
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        let ours = parse_file_name(name).is_some()
            || name.strip_suffix(".tmp").is_some_and(|n| parse_file_name(n).is_some());
        if ours {
            if let Err(err) = fs::remove_file(e.path()) {
                failed = Some(err);
            }
        }
    }
    failed.map_or(Ok(()), Err)
}

/// Writes what is pending. The caller holds `IO`.
fn write_pending(dir: &Path) {
    let (day, apps) = {
        let mut guard = lock(&STATE);
        match guard.as_mut() {
            Some(rec) => (rec.day, rec.take()),
            None => return,
        }
    };
    if let Err(err) = merge_into_file(dir, day, &apps) {
        eprintln!("Orbit history: could not write {}: {err}", file_name(day));
    }
}

// ---------------------------------------------------------------------------
// Hooks called by lib.rs and the sampler
// ---------------------------------------------------------------------------

/// Called once from the Tauri setup hook. The sampler may record a tick
/// before this runs, so `record` must cope with being called first.
pub fn init(app: &AppHandle) {
    if let Ok(base) = app.path().app_local_data_dir() {
        let _ = APP_DIR.set(base.join("history"));
    }
    if let Some(dir) = history_dir() {
        let today = local_day(now_secs() as i64);
        let _ = std::thread::Builder::new()
            .name("orbit-history-prune".into())
            .spawn(move || {
                let _io = lock(&IO);
                prune(&dir, today);
            });
    }
}

/// Called by the sampler after every tick. `interval_secs` is how many
/// seconds this sample stands for (0 for the first one).
pub fn record(snapshot: &Snapshot, interval_secs: f64) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let interval = finite(interval_secs);
    let now_ms = snapshot.ts;
    let now = now_ms / 1000;
    let day = local_day(now as i64);

    let last = LAST_FLUSH_MS.load(Ordering::Relaxed);
    if last == 0 {
        LAST_FLUSH_MS.store(now_ms, Ordering::Relaxed);
    }
    let time_due = last != 0 && now_ms.saturating_sub(last) >= FLUSH_EVERY_MS;
    let pending_day = PENDING_DAY.load(Ordering::Relaxed);
    let day_due = pending_day != NO_DAY && pending_day != day;
    // Only a tick that writes reads the folder setting or takes the file lock.
    let dir = if time_due || day_due { history_dir() } else { None };
    let _io = dir.as_ref().map(|_| lock(&IO));

    let mut writes: Vec<(i64, Vec<DayApp>)> = Vec::new();
    {
        let mut guard = lock(&STATE);
        let rec = guard.get_or_insert_with(|| Recorder::new(day));
        if rec.day != day {
            // A new day: what is pending belongs to the old one.
            writes.push((rec.day, rec.take()));
            rec.reset(day);
        }
        rec.add_tick(snapshot, interval, now);
        if dir.is_some() && time_due {
            writes.push((day, rec.take()));
        }
    }
    PENDING_DAY.store(day, Ordering::Relaxed);

    if let Some(dir) = &dir {
        for (d, apps) in &writes {
            if let Err(err) = merge_into_file(dir, *d, apps) {
                eprintln!("Orbit history: could not write {}: {err}", file_name(*d));
            }
        }
        if day_due {
            prune(dir, day);
        }
        LAST_FLUSH_MS.store(now_ms, Ordering::Relaxed);
    }
}

/// Called once when Orbit exits (RunEvent::Exit): write anything pending.
pub fn flush() {
    let Some(dir) = history_dir() else { return };
    let _io = lock(&IO);
    write_pending(&dir);
}

fn history_rows(days: u32) -> Vec<HistoryRow> {
    let days = days.clamp(1, KEEP_DAYS as u32) as i64;
    let today = local_day(now_secs() as i64);
    let from = today - days + 1;
    let _io = lock(&IO);
    let mut merged: HashMap<String, DayApp> = HashMap::new();
    if let Some(dir) = history_dir() {
        read_range(&dir, from, today, &mut merged);
    }
    {
        let guard = lock(&STATE);
        if let Some(rec) = guard.as_ref() {
            if (from..=today).contains(&rec.day) {
                for a in rec.pending() {
                    merge_app(&mut merged, a);
                }
            }
        }
    }
    let mut rows: Vec<HistoryRow> = merged.values().map(DayApp::to_row).collect();
    rows.sort_by(|a, b| {
        b.cpu_seconds
            .total_cmp(&a.cpu_seconds)
            .then_with(|| b.last_seen.cmp(&a.last_seen))
            .then_with(|| a.key.cmp(&b.key))
    });
    rows
}

fn clear_all() -> Result<(), String> {
    let _io = lock(&IO);
    {
        let mut guard = lock(&STATE);
        if let Some(rec) = guard.as_mut() {
            let day = rec.day;
            rec.reset(day);
        }
    }
    match history_dir() {
        Some(dir) => delete_all(&dir).map_err(|err| format!("Could not delete every history file: {err}.")),
        None => Ok(()),
    }
}

#[tauri::command]
pub async fn get_history(days: u32) -> Result<Vec<HistoryRow>, String> {
    tauri::async_runtime::spawn_blocking(move || history_rows(days))
        .await
        .map_err(|_| "Could not read app history.".to_string())
}

#[tauri::command]
pub async fn clear_history() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(clear_all)
        .await
        .map_err(|_| "Could not delete app history.".to_string())?
}

/// The UI calls this at boot and whenever the setting changes. Turning it
/// off writes what was recorded so far.
#[tauri::command]
pub async fn set_history_enabled(enabled: bool) -> Result<(), String> {
    let was = ENABLED.swap(enabled, Ordering::Relaxed);
    if was && !enabled {
        let _ = tauri::async_runtime::spawn_blocking(flush).await;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    use std::time::Instant;

    fn row(pid: u32, name: &str, exe: Option<&str>, cpu: f64, memory: u64, kind: ProcessKind) -> ProcessRow {
        ProcessRow {
            pid,
            parent_pid: None,
            name: name.into(),
            exe: exe.map(Into::into),
            cmd: String::new(),
            user: None,
            cpu,
            memory,
            disk_read_bps: 1000.0,
            disk_write_bps: 500.0,
            gpu: None,
            status: "running".into(),
            start_time: 1_700_000_000 + pid as u64,
            kind,
            window_title: None,
            not_responding: false,
            suspended: false,
            session_id: Some(1),
            net_rx_bps: None,
            net_tx_bps: None,
        }
    }

    fn snapshot(ts_secs: u64, cores: usize, processes: Vec<ProcessRow>) -> Snapshot {
        Snapshot {
            ts: ts_secs * 1000,
            interval_ms: 1000,
            cpu: CpuStats { usage: 10.0, per_core: vec![10.0; cores], freq_mhz: 3000 },
            memory: MemoryStats {
                total: 1,
                used: 1,
                available: 0,
                swap_total: 0,
                swap_used: 0,
                committed: None,
                commit_limit: None,
                cached: None,
                free: None,
            },
            disk: DiskStats { active_pct: None, read_bps: None, write_bps: None },
            network: NetworkStats { rx_bps: 0.0, tx_bps: 0.0, interfaces: vec![], per_app: PerAppStatus::default() },
            gpu: GpuStats { usage: None, dedicated_used: None },
            system: SystemStats { uptime_secs: 1, process_count: 0, thread_count: None, handle_count: None },
            disks: vec![],
            gpu_engines: vec![],
            battery: None,
            processes,
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("orbit-history-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn dates_round_trip() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        let nov9 = days_from_civil(2026, 11, 9);
        for day in [-1000, 0, 59, 60, 11_016, nov9, 30_000] {
            let (y, m, d) = civil_from_days(day);
            assert_eq!(days_from_civil(y, m, d), day);
        }
        assert_eq!(file_name(nov9), "2026-11-09.json");
        assert_eq!(parse_file_name("2026-11-09.json"), Some(nov9));
        assert_eq!(parse_file_name("2024-02-29.json"), Some(days_from_civil(2024, 2, 29)));
        for bad in ["2026-02-30.json", "2026-13-01.json", "2026-1-01.json", "notes.json", "2026-11-09.json.tmp", "2026-11-0x.json"] {
            assert_eq!(parse_file_name(bad), None, "{bad}");
        }
        // The offset is whole minutes and within the real range.
        let off = local_offset_secs();
        assert_eq!(off % 60, 0);
        assert!(off.abs() <= 14 * 3600);
    }

    #[test]
    fn records_per_app_totals() {
        let mut rec = Recorder::new(10);
        let chrome = r"C:\Program Files\Google\Chrome\Application\chrome.exe";
        let rows = || {
            vec![
                row(0, "System Idle Process", None, 90.0, 8, ProcessKind::Windows),
                // Two processes of one app; the path differs only in case.
                row(10, "chrome.exe", Some(chrome), 2.0, 100 << 20, ProcessKind::App),
                row(11, "chrome.exe", Some(&chrome.to_uppercase()), 1.0, 50 << 20, ProcessKind::Background),
                // No path: keyed by name. Idle, no window: not active.
                row(20, "Helper.exe", None, 0.1, 10 << 20, ProcessKind::Background),
            ]
        };
        // First tick stands for 0 seconds: seen, but no time.
        rec.add_tick(&snapshot(1000, 8, rows()), 0.0, 1000);
        rec.add_tick(&snapshot(1002, 8, rows()), 2.0, 1002);
        let mut second = rows();
        second[1].memory = 200 << 20;
        rec.add_tick(&snapshot(1003, 8, second), 1.0, 1003);

        let apps: HashMap<String, DayApp> = rec.pending().map(|a| (a.key.clone(), a.clone())).collect();
        assert_eq!(apps.len(), 2, "idle process skipped, chrome merged");
        let c = &apps[&chrome.to_lowercase()];
        // 3% of an 8-core machine for 3 s = 0.03 * 8 * 3 = 0.72 core-seconds.
        assert!((c.cpu_seconds - 0.72).abs() < 1e-9, "{}", c.cpu_seconds);
        assert_eq!(c.max_memory, (250u64 << 20) as f64);
        // 150 MB for 2 s, 250 MB for 1 s.
        let avg = c.to_row().avg_memory as f64 / (1u64 << 20) as f64;
        assert!((avg - (150.0 * 2.0 + 250.0) / 3.0).abs() < 0.01, "{avg}");
        assert_eq!(c.disk_read_bytes, 2000.0 * 3.0);
        assert_eq!(c.disk_write_bytes, 1000.0 * 3.0);
        assert_eq!(c.net_rx_bytes, 0.0);
        assert_eq!(c.active_seconds, 3.0);
        assert_eq!((c.first_seen, c.last_seen), (1000, 1003));
        let h = &apps["helper.exe"];
        assert_eq!(h.active_seconds, 0.0);
        assert_eq!(h.name, "Helper.exe");
        assert!(h.exe.is_none());

        // take() hands the totals over once and keeps the identities.
        let taken = rec.take();
        assert_eq!(taken.len(), 2);
        assert_eq!(rec.pending().count(), 0);
        assert!(rec.take().is_empty());
        assert_eq!(rec.apps.len(), 2);
    }

    #[test]
    fn counts_network_and_activity() {
        let mut rec = Recorder::new(1);
        let mut r = row(5, "node.exe", Some(r"C:\node\node.exe"), 0.6, 1, ProcessKind::Background);
        r.net_rx_bps = Some(1000.0);
        r.net_tx_bps = Some(f64::NAN);
        rec.add_tick(&snapshot(50, 4, vec![r.clone()]), 1.5, 50);
        let a = rec.pending().next().unwrap();
        assert_eq!(a.net_rx_bytes, 1500.0);
        assert_eq!(a.net_tx_bytes, 0.0);
        assert_eq!(a.active_seconds, 1.5, "above 0.5% counts as active");
    }

    #[test]
    fn forgets_exited_processes() {
        let mut rec = Recorder::new(1);
        let a = row(1, "a.exe", None, 0.0, 1, ProcessKind::Background);
        let b = row(2, "b.exe", None, 0.0, 1, ProcessKind::Background);
        rec.add_tick(&snapshot(1, 1, vec![a.clone(), b]), 1.0, 1);
        for i in 0..PRUNE_EVERY_TICKS {
            rec.add_tick(&snapshot(2 + i, 1, vec![a.clone()]), 1.0, 2 + i);
        }
        assert_eq!(rec.procs.len(), 1);
        assert_eq!(rec.apps.len(), 2, "the day's apps stay");
    }

    #[test]
    fn files_merge_by_day_and_prune() {
        let dir = scratch("files");
        let today = days_from_civil(2026, 11, 9);
        let mut rec = Recorder::new(today);
        rec.add_tick(&snapshot(1, 2, vec![row(1, "a.exe", Some(r"C:\A.exe"), 50.0, 100, ProcessKind::App)]), 0.0, 100);
        rec.add_tick(&snapshot(2, 2, vec![row(1, "a.exe", Some(r"C:\A.exe"), 50.0, 100, ProcessKind::App)]), 10.0, 110);
        merge_into_file(&dir, today, &rec.take()).unwrap();
        // A later minute with more memory.
        rec.add_tick(&snapshot(3, 2, vec![row(1, "a.exe", Some(r"C:\A.exe"), 50.0, 300, ProcessKind::App)]), 10.0, 120);
        merge_into_file(&dir, today, &rec.take()).unwrap();
        // Yesterday, written by another run.
        let mut old = DayApp::blank(r"c:\a.exe".into(), "a.exe".into(), Some(r"C:\A.exe".into()));
        old.cpu_seconds = 5.0;
        old.run_seconds = 20.0;
        old.mem_byte_seconds = 20.0 * 1000.0;
        old.max_memory = 1000.0;
        old.first_seen = 50;
        old.last_seen = 60;
        merge_into_file(&dir, today - 1, std::slice::from_ref(&old)).unwrap();

        let f = read_day(&dir, today).unwrap();
        assert_eq!(f.version, FILE_VERSION);
        assert_eq!(f.date, "2026-11-09");
        assert_eq!(f.apps.len(), 1);
        // 50% of 2 cores for 20 s = 20 core-seconds.
        assert!((f.apps[0].cpu_seconds - 20.0).abs() < 1e-9);
        assert_eq!(f.apps[0].max_memory, 300.0);
        assert_eq!((f.apps[0].first_seen, f.apps[0].last_seen), (100, 120));

        let mut out = HashMap::new();
        read_range(&dir, today - 1, today, &mut out);
        let r = out[r"c:\a.exe"].to_row();
        assert!((r.cpu_seconds - 25.0).abs() < 1e-9);
        assert_eq!(r.max_memory, 1000);
        // (100*10 + 300*10 + 1000*20) / 40 = 600
        assert_eq!(r.avg_memory, 600);
        assert_eq!((r.first_seen, r.last_seen), (50, 120));
        // Yesterday's entry had no active time.
        assert_eq!(r.active_seconds, 20.0);

        // Only today.
        let mut out = HashMap::new();
        read_range(&dir, today, today, &mut out);
        assert!((out[r"c:\a.exe"].cpu_seconds - 20.0).abs() < 1e-9);

        // A broken file is skipped, not fatal.
        fs::write(dir.join(file_name(today - 2)), b"{not json").unwrap();
        let mut out = HashMap::new();
        read_range(&dir, today - 2, today, &mut out);
        assert_eq!(out.len(), 1);

        // Prune keeps 90 days and never touches other files.
        merge_into_file(&dir, today - KEEP_DAYS, std::slice::from_ref(&old)).unwrap();
        merge_into_file(&dir, today - KEEP_DAYS + 1, std::slice::from_ref(&old)).unwrap();
        fs::write(dir.join("notes.txt"), b"keep").unwrap();
        prune(&dir, today);
        assert!(!dir.join(file_name(today - KEEP_DAYS)).exists());
        assert!(dir.join(file_name(today - KEEP_DAYS + 1)).exists());
        assert!(dir.join("notes.txt").exists());

        fs::write(dir.join(format!("{}.tmp", file_name(today))), b"x").unwrap();
        delete_all(&dir).unwrap();
        let left: Vec<_> = fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left, vec![std::ffi::OsString::from("notes.txt")]);
        delete_all(&dir.join("missing")).unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    /// The only test that uses the global recorder and ORBIT_HISTORY_DIR.
    #[test]
    fn global_record_flush_read_clear() {
        let dir = scratch("global");
        std::env::set_var("ORBIT_HISTORY_DIR", &dir);
        let now = now_secs();
        let app = r"C:\Tools\Global.exe";
        let rows = |cpu| vec![row(77, "Global.exe", Some(app), cpu, 4096, ProcessKind::App)];
        record(&snapshot(now - 2, 4, rows(25.0)), 0.0);
        record(&snapshot(now - 1, 4, rows(25.0)), 1.0);
        // Pending data is visible before any write.
        let r = history_rows(1);
        let g = r.iter().find(|r| r.key == app.to_lowercase()).expect("row");
        assert!((g.cpu_seconds - 1.0).abs() < 1e-9, "{}", g.cpu_seconds);
        assert_eq!(g.name, "Global.exe");

        flush();
        let today = local_day(now as i64);
        assert!(dir.join(file_name(today)).exists());
        // Written once: not counted twice.
        let r = history_rows(7);
        let g = r.iter().find(|r| r.key == app.to_lowercase()).unwrap();
        assert!((g.cpu_seconds - 1.0).abs() < 1e-9);

        // Disabled: ticks are ignored.
        ENABLED.store(false, Ordering::Relaxed);
        record(&snapshot(now, 4, rows(100.0)), 1.0);
        ENABLED.store(true, Ordering::Relaxed);
        let g = history_rows(1).into_iter().find(|r| r.key == app.to_lowercase()).unwrap();
        assert!((g.cpu_seconds - 1.0).abs() < 1e-9);

        clear_all().unwrap();
        assert!(history_rows(90).iter().all(|r| r.key != app.to_lowercase()));
        assert!(!dir.join(file_name(today)).exists());
        std::env::remove_var("ORBIT_HISTORY_DIR");
        let _ = fs::remove_dir_all(&dir);
    }

    /// Cost of one tick on a busy PC: 600 processes in 150 apps.
    #[test]
    fn record_tick_is_cheap() {
        let rows: Vec<ProcessRow> = (1..=600)
            .map(|i| {
                let exe = format!(r"C:\Program Files\App{}\app{}.exe", i % 150, i % 150);
                row(i, &format!("app{}.exe", i % 150), Some(&exe), (i % 7) as f64 * 0.3, 50 << 20, ProcessKind::Background)
            })
            .collect();
        let snap = snapshot(1000, 16, rows);
        let mut rec = Recorder::new(1);
        rec.add_tick(&snap, 0.0, 1000); // builds the keys once
        let n = 2000;
        let started = Instant::now();
        for i in 0..n {
            rec.add_tick(&snap, 1.0, 1001 + i);
        }
        let per_tick = started.elapsed().as_secs_f64() * 1e6 / n as f64;
        println!("history record: {per_tick:.1} us per tick (600 rows, 150 apps)");
        assert_eq!(rec.apps.len(), 150);
        assert!(per_tick < 5000.0, "{per_tick} us per tick");
        let started = Instant::now();
        let taken = rec.take();
        println!("history take: {:.1} us for {} apps", started.elapsed().as_secs_f64() * 1e6, taken.len());
        // The once-a-minute write: read the day file, merge, write it back.
        let dir = scratch("cost");
        merge_into_file(&dir, 1, &taken).unwrap();
        let started = Instant::now();
        merge_into_file(&dir, 1, &taken).unwrap();
        println!("history write: {:.2} ms for {} apps", started.elapsed().as_secs_f64() * 1e3, taken.len());
        let _ = fs::remove_dir_all(&dir);
    }
}
