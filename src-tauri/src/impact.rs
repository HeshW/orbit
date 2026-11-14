//! Startup impact (High / Medium / Low), measured by Orbit itself.
//!
//! Windows keeps Task Manager's startup-impact numbers to itself, so Orbit
//! measures with the same thresholds: CPU time and disk reads in an app's
//! first 60 seconds. "Measure at next sign-in" (startup.rs) writes a one-shot
//! RunOnce value that starts `orbit.exe --measure-startup`. That run opens no
//! window: for about 120 s it samples every 2 s, tracks each process whose
//! image is an enabled startup entry's resolved path, keeps the sample closest
//! to 60 s after the process started, writes
//! `%LOCALAPPDATA%\Orbit\startup-impact.json` and exits. `list_startup` reads
//! that file. `ORBIT_MEASURE_SECS` shortens the run (for testing).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{FILETIME, HANDLE};
use windows::Win32::System::Threading::{
    GetProcessIoCounters, GetProcessTimes, IO_COUNTERS, PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::actions::{
    filetime_now, handle_creation_filetime, image_path, normalize_path, open_process,
    snapshot_processes, OwnedHandle,
};
use crate::model::StartupEntry;

/// Command-line flag for the measurement run.
pub const MEASURE_FLAG: &str = "--measure-startup";

const FILE_NAME: &str = "startup-impact.json";
const DEFAULT_SECS: u64 = 120;
const SAMPLE_EVERY: Duration = Duration::from_secs(2);
/// The sample closest to this age (seconds after the process started) counts.
const TARGET_AGE_SECS: f64 = 60.0;
/// Processes already older than this when first seen are not measurable.
const MAX_FIRST_AGE_SECS: f64 = 90.0;
const FILETIME_TICKS_PER_SEC: f64 = 10_000_000.0;

// Task Manager's thresholds.
const HIGH_CPU_MS: u64 = 1_000;
const MEDIUM_CPU_MS: u64 = 300;
const HIGH_DISK_BYTES: u64 = 3 * 1024 * 1024;
const MEDIUM_DISK_BYTES: u64 = 300 * 1024;

/// High = more than 1 s CPU or more than 3 MB read; Medium = 300 ms to 1 s CPU
/// or 300 KB to 3 MB read; Low = below both.
pub fn classify(cpu_ms: u64, disk_read_bytes: u64) -> &'static str {
    if cpu_ms > HIGH_CPU_MS || disk_read_bytes > HIGH_DISK_BYTES {
        "high"
    } else if cpu_ms >= MEDIUM_CPU_MS || disk_read_bytes >= MEDIUM_DISK_BYTES {
        "medium"
    } else {
        "low"
    }
}

/// The file written by a measurement run.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ImpactFile {
    /// Seconds since the Unix epoch.
    pub measured_at: u64,
    /// Lowercased exe path -> what its processes used in their first 60 s.
    pub entries: HashMap<String, ImpactRecord>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ImpactRecord {
    pub cpu_ms: u64,
    pub disk_read_bytes: u64,
    pub impact: String,
}

/// `%LOCALAPPDATA%\Orbit\startup-impact.json`.
pub fn file_path() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("AppData").join("Local"))
        })?;
    Some(base.join("Orbit").join(FILE_NAME))
}

/// The last measurement, if there is a readable one.
pub fn load() -> Option<ImpactFile> {
    let text = std::fs::read_to_string(file_path()?).ok()?;
    serde_json::from_str(&text).ok()
}

/// Fills `impact` and `impact_measured_at` for entries the measurement saw.
pub fn apply(entries: &mut [StartupEntry], file: Option<&ImpactFile>) {
    let Some(file) = file else {
        return;
    };
    for e in entries.iter_mut() {
        let Some(path) = e.path.as_deref() else {
            continue;
        };
        if let Some(r) = file.entries.get(&normalize_path(path)) {
            // Recomputed from the numbers, so a hand-edited label cannot leak through.
            e.impact = Some(classify(r.cpu_ms, r.disk_read_bytes).to_string());
            e.impact_measured_at = Some(file.measured_at);
        }
    }
}

// ---------------------------------------------------------------------------
// Measurement run
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
struct Sample {
    /// |age - 60 s|
    off_target: f64,
    cpu_ms: u64,
    disk_read: u64,
}

struct Tracked {
    handle: OwnedHandle,
    key: String,
    created: u64,
    best: Option<Sample>,
}

fn age_secs(now: u64, created: u64) -> f64 {
    now.saturating_sub(created) as f64 / FILETIME_TICKS_PER_SEC
}

/// A process can be measured when Orbit first sees it within 90 s of its start.
fn measurable(first_seen_age_secs: f64) -> bool {
    first_seen_age_secs <= MAX_FIRST_AGE_SECS
}

/// A sample at `age` seconds; replaces `best` when it is closer to 60 s.
fn keep_closer(best: Option<Sample>, age: f64, cpu_ms: u64, disk_read: u64) -> Option<Sample> {
    let candidate = Sample {
        off_target: (age - TARGET_AGE_SECS).abs(),
        cpu_ms,
        disk_read,
    };
    match best {
        Some(b) if b.off_target <= candidate.off_target => Some(b),
        _ => Some(candidate),
    }
}

/// Sums the processes of each exe (an app with helper processes counts once).
fn aggregate<I: IntoIterator<Item = (String, u64, u64)>>(samples: I) -> HashMap<String, ImpactRecord> {
    let mut sums: HashMap<String, (u64, u64)> = HashMap::new();
    for (key, cpu, disk) in samples {
        let e = sums.entry(key).or_default();
        e.0 = e.0.saturating_add(cpu);
        e.1 = e.1.saturating_add(disk);
    }
    sums.into_iter()
        .map(|(key, (cpu_ms, disk_read_bytes))| {
            let impact = classify(cpu_ms, disk_read_bytes).to_string();
            (key, ImpactRecord { cpu_ms, disk_read_bytes, impact })
        })
        .collect()
}

fn filetime_ticks(ft: FILETIME) -> u64 {
    ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
}

/// (CPU ms, bytes read) used so far by the process behind `handle`. Works on
/// a process that has exited, as long as the handle is open.
fn usage(handle: HANDLE) -> Option<(u64, u64)> {
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) }.ok()?;
    let cpu_ms = (filetime_ticks(kernel) + filetime_ticks(user)) / 10_000;
    let mut io = IO_COUNTERS::default();
    unsafe { GetProcessIoCounters(handle, &mut io) }.ok()?;
    Some((cpu_ms, io.ReadTransferCount))
}

fn file_name_lower(path: &str) -> String {
    path.rsplit('\\').next().unwrap_or(path).to_string()
}

/// Opens every new process whose image is one of `targets`.
fn discover(
    names: &HashSet<String>,
    targets: &HashSet<String>,
    tracked: &mut HashMap<u32, Tracked>,
    ignored: &mut HashSet<u32>,
) {
    let Ok(entries) = snapshot_processes() else {
        return;
    };
    let now = filetime_now();
    for e in entries {
        if tracked.contains_key(&e.pid) || ignored.contains(&e.pid) {
            continue;
        }
        if !names.contains(&e.name.to_lowercase()) {
            continue;
        }
        let Ok(handle) = open_process(e.pid, PROCESS_QUERY_LIMITED_INFORMATION) else {
            ignored.insert(e.pid);
            continue;
        };
        let (Some(path), Some(created)) = (image_path(handle.0), handle_creation_filetime(handle.0))
        else {
            ignored.insert(e.pid);
            continue;
        };
        let key = normalize_path(&path);
        if !targets.contains(&key) || !measurable(age_secs(now, created)) {
            ignored.insert(e.pid);
            continue;
        }
        tracked.insert(e.pid, Tracked { handle, key, created, best: None });
    }
}

fn measure(targets: &HashSet<String>, duration: Duration) -> ImpactFile {
    let names: HashSet<String> = targets.iter().map(|p| file_name_lower(p)).collect();
    let mut tracked: HashMap<u32, Tracked> = HashMap::new();
    let mut ignored: HashSet<u32> = HashSet::new();
    let started = Instant::now();
    loop {
        if !targets.is_empty() {
            discover(&names, targets, &mut tracked, &mut ignored);
        }
        let now = filetime_now();
        for t in tracked.values_mut() {
            if let Some((cpu, disk)) = usage(t.handle.0) {
                t.best = keep_closer(t.best, age_secs(now, t.created), cpu, disk);
            }
        }
        let elapsed = started.elapsed();
        if elapsed >= duration {
            break;
        }
        std::thread::sleep(SAMPLE_EVERY.min(duration - elapsed));
    }
    let samples = tracked
        .into_values()
        .filter_map(|t| t.best.map(|b| (t.key, b.cpu_ms, b.disk_read)));
    ImpactFile {
        measured_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        entries: aggregate(samples),
    }
}

fn save(file: &ImpactFile) -> std::io::Result<()> {
    let path = file_path().ok_or_else(|| std::io::Error::other("no LOCALAPPDATA"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(file).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)
}

fn run_secs() -> u64 {
    std::env::var("ORBIT_MEASURE_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|s| (1..=900).contains(s))
        .unwrap_or(DEFAULT_SECS)
}

/// The `--measure-startup` run: no window, measures, writes the file, returns.
pub fn run_measurement() {
    let targets: HashSet<String> = crate::startup::collect_startup()
        .into_iter()
        .filter(|e| e.enabled)
        .filter_map(|e| e.path)
        .map(|p| normalize_path(&p))
        .collect();
    let file = measure(&targets, Duration::from_secs(run_secs()));
    let _ = save(&file);
}

#[cfg(test)]
mod tests {
    use super::*;

    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;

    #[test]
    fn classifies_like_task_manager() {
        assert_eq!(classify(0, 0), "low");
        assert_eq!(classify(299, 299 * KB), "low");
        assert_eq!(classify(300, 0), "medium");
        assert_eq!(classify(0, 300 * KB), "medium");
        assert_eq!(classify(1_000, 3 * MB), "medium");
        assert_eq!(classify(1_001, 0), "high");
        assert_eq!(classify(0, 3 * MB + 1), "high");
        assert_eq!(classify(50, 40 * MB), "high");
    }

    #[test]
    fn keeps_the_sample_closest_to_60_s() {
        let mut best = None;
        for (age, cpu) in [(2.0, 10), (30.0, 200), (58.5, 400), (62.0, 410), (90.0, 900)] {
            best = keep_closer(best, age, cpu, 0);
        }
        assert_eq!(best.map(|b| b.cpu_ms), Some(400));
        // A tie keeps the earlier sample.
        let tie = keep_closer(keep_closer(None, 59.0, 1, 0), 61.0, 2, 0);
        assert_eq!(tie.map(|b| b.cpu_ms), Some(1));
    }

    #[test]
    fn only_young_processes_are_measurable() {
        assert!(measurable(0.0));
        assert!(measurable(90.0));
        assert!(!measurable(90.5));
        assert_eq!(age_secs(30 * 10_000_000, 10 * 10_000_000), 20.0);
        assert_eq!(age_secs(5, 10), 0.0);
    }

    #[test]
    fn sums_processes_of_the_same_exe() {
        let out = aggregate(vec![
            ("c:\\a\\app.exe".to_string(), 200, 100 * KB),
            ("c:\\a\\app.exe".to_string(), 200, 100 * KB),
            ("c:\\b\\tool.exe".to_string(), 5, 0),
        ]);
        let app = &out["c:\\a\\app.exe"];
        assert_eq!((app.cpu_ms, app.disk_read_bytes), (400, 200 * KB));
        assert_eq!(app.impact, "medium");
        assert_eq!(out["c:\\b\\tool.exe"].impact, "low");
    }

    #[test]
    fn applies_measurements_to_entries() {
        let entry = |path: Option<&str>| StartupEntry {
            id: "HKCU\\Run\\X".into(),
            name: "X".into(),
            command: String::new(),
            location: "HKCU Run".into(),
            enabled: true,
            path: path.map(str::to_string),
            needs_admin: false,
            impact: None,
            impact_measured_at: None,
            packaged_state: None,
        };
        let mut list = vec![entry(Some(r"C:\Apps\Big.exe")), entry(Some(r"C:\Apps\Other.exe")), entry(None)];
        let file = ImpactFile {
            measured_at: 1_700_000_000,
            entries: aggregate(vec![(r"c:\apps\big.exe".to_string(), 1_500, 0)]),
        };
        apply(&mut list, Some(&file));
        assert_eq!(list[0].impact.as_deref(), Some("high"));
        assert_eq!(list[0].impact_measured_at, Some(1_700_000_000));
        assert_eq!(list[1].impact, None);
        assert_eq!(list[2].impact, None);
        apply(&mut list[1..], None);
        assert_eq!(list[1].impact_measured_at, None);
    }

    #[test]
    fn file_round_trips_as_camel_case() {
        let file = ImpactFile {
            measured_at: 42,
            entries: aggregate(vec![("c:\\x.exe".to_string(), 1, 2)]),
        };
        let text = serde_json::to_string(&file).unwrap();
        assert!(text.contains("\"measuredAt\":42"));
        assert!(text.contains("\"cpuMs\":1"));
        assert!(text.contains("\"diskReadBytes\":2"));
        let back: ImpactFile = serde_json::from_str(&text).unwrap();
        assert_eq!(back.entries["c:\\x.exe"].impact, "low");
    }

    #[test]
    fn file_lives_under_local_app_data() {
        let p = file_path().unwrap();
        assert!(p.ends_with(r"Orbit\startup-impact.json"));
    }
}
