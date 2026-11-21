//! Network usage over time, for the Network view's Usage tab.
//!
//! The sampler calls `record` after every tick, right after App history. Each
//! tick adds `rate * interval` bytes to a 5-minute bucket: the whole machine
//! (every adapter, from `snapshot.network`, no admin needed) and each app with
//! per-app rates (only while Orbit runs as administrator). Buckets are kept
//! for 24 hours; the machine total is also kept per local day for 30 days.
//! Per-app totals for 7 and 30 days come from App history's day files.
//!
//! It follows App history's switch (nothing is recorded while it is off) and
//! its folder: `<history dir>/network.json`, rewritten whole at most once a
//! minute, when recording is turned off and on exit. Delete history removes
//! it; Remove from history drops that app here too.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

use crate::history;
use crate::model::Snapshot;

/// Width of one bucket.
const BUCKET_SECS: u64 = 300;
/// Buckets are kept while they overlap the last 24 hours.
const KEEP_SECS: u64 = 86_400;
/// Machine totals per local day kept (today included).
const KEEP_DAYS: i64 = 30;
const FLUSH_EVERY_MS: u64 = 60_000;
const FILE_NAME: &str = "network.json";
const FILE_VERSION: u32 = 1;

static STATE: Mutex<Option<State>> = Mutex::new(None);

struct Guard(MutexGuard<'static, Option<State>>);

impl std::ops::Deref for Guard {
    type Target = State;
    fn deref(&self) -> &State {
        self.0.as_ref().expect("set in lock()")
    }
}

impl std::ops::DerefMut for Guard {
    fn deref_mut(&mut self) -> &mut State {
        self.0.as_mut().expect("set in lock()")
    }
}

fn lock() -> Guard {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    g.get_or_insert_with(State::new);
    Guard(g)
}

/// One app's bytes in a period.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UsageApp {
    /// Same key as App history: lowercased exe path, else lowercased name.
    pub key: String,
    pub name: String,
    pub exe: Option<String>,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

/// What `get_network_usage` returns.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct NetworkUsage {
    /// "hour" | "day" | "week" | "month".
    pub period: String,
    /// Start of the period, seconds since the Unix epoch.
    pub from: u64,
    /// Every adapter, while Orbit ran.
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    /// Earliest moment in the period Orbit counted anything; `None` if nothing.
    pub since: Option<u64>,
    /// App history's switch: nothing is counted while it is off.
    pub recording: bool,
    /// Busiest first. Empty when Orbit never had per-app rates in the period.
    pub apps: Vec<UsageApp>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
struct Bucket {
    /// Seconds since the epoch, a multiple of `BUCKET_SECS`.
    start: u64,
    /// When the first tick in it was counted.
    first: u64,
    rx: f64,
    tx: f64,
    /// App key -> [rx, tx] bytes.
    apps: HashMap<String, [f64; 2]>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
struct DayTotal {
    /// Local day number (days since 1970-01-01 in local time).
    day: i64,
    first: u64,
    rx: f64,
    tx: f64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
struct AppName {
    name: String,
    exe: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
struct Store {
    version: u32,
    /// App key -> how to show it.
    names: HashMap<String, AppName>,
    /// Oldest first.
    buckets: Vec<Bucket>,
    /// Oldest first.
    days: Vec<DayTotal>,
}

/// One app's bytes in one tick.
struct TickApp<'a> {
    key: String,
    name: &'a str,
    exe: Option<&'a str>,
    rx: f64,
    tx: f64,
}

impl Store {
    fn add(&mut self, now: u64, day: i64, rx: f64, tx: f64, apps: Vec<TickApp>) {
        let start = now - now % BUCKET_SECS;
        let i = match self.buckets.iter().rposition(|b| b.start == start) {
            Some(i) => i,
            None => {
                self.buckets.push(Bucket { start, first: now, ..Default::default() });
                // A clock set back can add an older bucket after a newer one.
                if self.buckets.len() > 1 && self.buckets[self.buckets.len() - 2].start > start {
                    self.buckets.sort_by_key(|b| b.start);
                    self.buckets.iter().position(|b| b.start == start).unwrap_or(0)
                } else {
                    self.buckets.len() - 1
                }
            }
        };
        let b = &mut self.buckets[i];
        b.rx += rx;
        b.tx += tx;
        b.first = b.first.min(now);
        for a in apps {
            if !self.names.contains_key(&a.key) {
                self.names.insert(a.key.clone(), AppName { name: a.name.to_string(), exe: a.exe.map(str::to_string) });
            }
            let v = b.apps.entry(a.key).or_insert([0.0, 0.0]);
            v[0] += a.rx;
            v[1] += a.tx;
        }
        match self.days.iter_mut().rev().find(|d| d.day == day) {
            Some(d) => {
                d.rx += rx;
                d.tx += tx;
                d.first = d.first.min(now);
            }
            None => {
                self.days.push(DayTotal { day, first: now, rx, tx });
                self.days.sort_by_key(|d| d.day);
            }
        }
    }

    /// Drops buckets past 24 hours, days past 30 and names no bucket uses.
    fn prune(&mut self, now: u64, today: i64) {
        let oldest = now.saturating_sub(KEEP_SECS);
        self.buckets.retain(|b| b.start + BUCKET_SECS > oldest);
        self.days.retain(|d| d.day > today - KEEP_DAYS);
        let buckets = &self.buckets;
        self.names.retain(|k, _| buckets.iter().any(|b| b.apps.contains_key(k)));
    }

    /// Adds what was in the file to what was recorded before it was read.
    fn merge(&mut self, other: Store) {
        for (k, v) in other.names {
            self.names.entry(k).or_insert(v);
        }
        for ob in other.buckets {
            match self.buckets.iter_mut().find(|b| b.start == ob.start) {
                Some(b) => {
                    b.rx += ob.rx;
                    b.tx += ob.tx;
                    b.first = b.first.min(ob.first);
                    for (k, v) in ob.apps {
                        let e = b.apps.entry(k).or_insert([0.0, 0.0]);
                        e[0] += v[0];
                        e[1] += v[1];
                    }
                }
                None => self.buckets.push(ob),
            }
        }
        self.buckets.sort_by_key(|b| b.start);
        for od in other.days {
            match self.days.iter_mut().find(|d| d.day == od.day) {
                Some(d) => {
                    d.rx += od.rx;
                    d.tx += od.tx;
                    d.first = d.first.min(od.first);
                }
                None => self.days.push(od),
            }
        }
        self.days.sort_by_key(|d| d.day);
    }

    fn forget(&mut self, key: &str) {
        self.names.remove(key);
        for b in &mut self.buckets {
            b.apps.remove(key);
        }
    }

    /// Totals of the buckets that overlap the last `span` seconds.
    fn recent(&self, now: u64, span: u64) -> (f64, f64, Option<u64>, Vec<UsageApp>) {
        let from = now.saturating_sub(span);
        let (mut rx, mut tx, mut since) = (0.0, 0.0, None::<u64>);
        let mut apps: HashMap<&str, [f64; 2]> = HashMap::new();
        for b in self.buckets.iter().filter(|b| b.start + BUCKET_SECS > from && b.start <= now) {
            rx += b.rx;
            tx += b.tx;
            since = Some(since.map_or(b.first, |s| s.min(b.first)));
            for (k, v) in &b.apps {
                let e = apps.entry(k.as_str()).or_insert([0.0, 0.0]);
                e[0] += v[0];
                e[1] += v[1];
            }
        }
        let apps = apps
            .into_iter()
            .map(|(k, v)| {
                let n = self.names.get(k);
                UsageApp {
                    key: k.to_string(),
                    name: n.map_or_else(|| k.to_string(), |n| n.name.clone()),
                    exe: n.and_then(|n| n.exe.clone()),
                    rx_bytes: whole(v[0]),
                    tx_bytes: whole(v[1]),
                }
            })
            .collect();
        (rx, tx, since, apps)
    }

    /// Machine totals of local days `from..=to`.
    fn days_total(&self, from: i64, to: i64) -> (f64, f64, Option<u64>) {
        let (mut rx, mut tx, mut since) = (0.0, 0.0, None::<u64>);
        for d in self.days.iter().filter(|d| (from..=to).contains(&d.day)) {
            rx += d.rx;
            tx += d.tx;
            since = Some(since.map_or(d.first, |s| s.min(d.first)));
        }
        (rx, tx, since)
    }
}

struct State {
    store: Store,
    /// The file has been read into `store` (or there is none to read).
    loaded: bool,
    last_write_ms: u64,
}

impl State {
    fn new() -> Self {
        State { store: Store { version: FILE_VERSION, ..Default::default() }, loaded: false, last_write_ms: 0 }
    }

    /// Reads the file once the folder is known. Ticks recorded before that
    /// are kept and added to what the file holds.
    fn ensure_loaded(&mut self) {
        if self.loaded {
            return;
        }
        let Some(dir) = history::history_dir() else { return };
        self.loaded = true;
        if let Some(file) = read_file(&dir) {
            self.store.merge(file);
        }
    }

    fn write(&mut self, now_ms: u64) {
        self.last_write_ms = now_ms;
        let Some(dir) = history::history_dir() else { return };
        let now = now_ms / 1000;
        self.store.prune(now, history::local_day(now as i64));
        if let Err(err) = write_file(&dir, &mut self.store) {
            eprintln!("Orbit network usage: could not write {FILE_NAME}: {err}");
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

fn finite(v: f64) -> f64 {
    if v.is_finite() && v > 0.0 {
        v
    } else {
        0.0
    }
}

fn read_file(dir: &Path) -> Option<Store> {
    let path = dir.join(FILE_NAME);
    let bytes = fs::read(&path).ok()?;
    match serde_json::from_slice::<Store>(&bytes) {
        Ok(s) => Some(s),
        Err(err) => {
            eprintln!("Orbit network usage: skipping {}: {err}", path.display());
            None
        }
    }
}

/// Writes the store atomically (temp file, then rename).
fn write_file(dir: &Path, store: &mut Store) -> std::io::Result<()> {
    store.version = FILE_VERSION;
    fs::create_dir_all(dir)?;
    let json = serde_json::to_vec(store).map_err(std::io::Error::other)?;
    let path = dir.join(FILE_NAME);
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    fs::write(&tmp, json)?;
    fs::rename(&tmp, &path)
}

/// The bytes each app moved in this tick (rows with per-app rates only).
fn tick_apps(snapshot: &Snapshot, interval: f64) -> Vec<TickApp<'_>> {
    let mut out: Vec<TickApp> = Vec::new();
    for row in &snapshot.processes {
        let rx = row.net_rx_bps.map_or(0.0, finite) * interval;
        let tx = row.net_tx_bps.map_or(0.0, finite) * interval;
        if row.pid == 0 || rx + tx <= 0.0 {
            continue;
        }
        let key = match row.exe.as_deref() {
            Some(p) if !p.is_empty() => p.to_lowercase(),
            _ => row.name.to_lowercase(),
        };
        match out.iter_mut().find(|a| a.key == key) {
            Some(a) => {
                a.rx += rx;
                a.tx += tx;
            }
            None => out.push(TickApp { key, name: &row.name, exe: row.exe.as_deref(), rx, tx }),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Hooks called by the sampler, lib.rs and history.rs
// ---------------------------------------------------------------------------

/// Called by the sampler after every tick. `interval_secs` is how many
/// seconds this sample stands for (0 for the first one).
pub fn record(snapshot: &Snapshot, interval_secs: f64) {
    if !history::is_enabled() {
        return;
    }
    let interval = finite(interval_secs);
    if interval == 0.0 {
        return;
    }
    let now_ms = snapshot.ts;
    let now = now_ms / 1000;
    let rx = finite(snapshot.network.rx_bps) * interval;
    let tx = finite(snapshot.network.tx_bps) * interval;
    let apps = tick_apps(snapshot, interval);
    let mut st = lock();
    st.ensure_loaded();
    st.store.add(now, history::local_day(now as i64), rx, tx, apps);
    if st.last_write_ms == 0 {
        st.last_write_ms = now_ms;
    } else if st.loaded && now_ms.saturating_sub(st.last_write_ms) >= FLUSH_EVERY_MS {
        st.write(now_ms);
    }
}

/// On exit and when recording is turned off: write what is in memory.
pub fn flush() {
    let mut st = lock();
    st.ensure_loaded();
    // Nothing recorded (recording never on, or just deleted): leave no empty file.
    if st.loaded && !(st.store.buckets.is_empty() && st.store.days.is_empty()) {
        st.write(now_ms());
    }
}

/// Delete history: forget everything and delete the file.
pub(crate) fn clear(dir: Option<&Path>) -> std::io::Result<()> {
    let mut st = lock();
    st.store = State::new().store;
    st.loaded = true;
    let Some(dir) = dir else { return Ok(()) };
    // A half-written "network.json.<pid>.tmp" left by a crash holds usage too.
    if let Ok(entries) = fs::read_dir(dir) {
        for e in entries.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("network.json.") && name.ends_with(".tmp") {
                let _ = fs::remove_file(e.path());
            }
        }
    }
    match fs::remove_file(dir.join(FILE_NAME)) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

/// Remove from history: drop one app (`key` is already lowercased).
pub(crate) fn forget(key: &str) {
    let mut st = lock();
    st.ensure_loaded();
    st.store.forget(key);
    if st.loaded {
        st.write(now_ms());
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn usage(period: &str, now: u64) -> Result<NetworkUsage, String> {
    let recording = history::is_enabled();
    let mut st = lock();
    st.ensure_loaded();
    let (from, rx, tx, since, mut apps) = match period {
        "hour" | "day" => {
            let span = if period == "hour" { 3600 } else { 86_400 };
            let (rx, tx, since, apps) = st.store.recent(now, span);
            (now.saturating_sub(span), rx, tx, since, apps)
        }
        "week" | "month" => {
            let days: i64 = if period == "week" { 7 } else { 30 };
            let today = history::local_day(now as i64);
            let first = today - days + 1;
            let (rx, tx, since) = st.store.days_total(first, today);
            drop(st);
            let from = (first * 86_400 - history::local_offset_secs()).max(0) as u64;
            let apps = history::history_rows(days as u32)
                .into_iter()
                .filter(|r| r.net_rx_bytes + r.net_tx_bytes > 0)
                .map(|r| UsageApp { key: r.key, name: r.name, exe: r.exe, rx_bytes: r.net_rx_bytes, tx_bytes: r.net_tx_bytes })
                .collect();
            (from, rx, tx, since, apps)
        }
        _ => return Err("Pick a period: last hour, 24 hours, 7 days or 30 days.".to_string()),
    };
    apps.sort_by(|a, b| {
        (b.rx_bytes + b.tx_bytes).cmp(&(a.rx_bytes + a.tx_bytes)).then_with(|| a.key.cmp(&b.key))
    });
    Ok(NetworkUsage { period: period.to_string(), from, rx_bytes: whole(rx), tx_bytes: whole(tx), since, recording, apps })
}

/// Network > Usage. `period` is "hour", "day", "week" or "month".
#[tauri::command]
pub async fn get_network_usage(period: String) -> Result<NetworkUsage, String> {
    tauri::async_runtime::spawn_blocking(move || usage(&period, now_ms() / 1000))
        .await
        .map_err(|_| "Could not read network usage.".to_string())?
}

#[cfg(test)]
mod tests;
