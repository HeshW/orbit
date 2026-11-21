//! Windows' own per-app data usage: the record behind Settings > Network > Data usage
//! (`ConnectionProfile.GetAttributedNetworkUsageAsync`). 7 and 30 days use it because it
//! covers every hour the PC was on, Orbit running or not. It needs no admin and only reads.
//! Windows updates it about once an hour, so a read is kept for a few minutes. Every
//! connection profile is summed, like Settings does across Wi-Fi and Ethernet networks.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::UsageApp;

/// Bytes Windows tied to one attribution id, summed over every connection profile.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Raw {
    /// An NT exe path ("\device\harddiskvolume3\...\app.exe"), a Store package family
    /// name ("Claude_pzs8sxrjxfjjc"), a name like "System", or empty for traffic Windows
    /// tied to no app.
    pub id: String,
    pub rx: u64,
    pub tx: u64,
}

/// Key of the row for traffic Windows tied to no app.
pub(super) const UNATTRIBUTED: &str = "windows:unattributed";
const CACHE_FOR: Duration = Duration::from_secs(300);
/// A failed or slow read is not tried again for this long, so 7 / 30 days fall back at once.
const FAILED_FOR: Duration = Duration::from_secs(60);
/// Longest wait for Windows' answer.
const READ_LIMIT: Duration = Duration::from_secs(10);

type Read = Result<Vec<Raw>, String>;

/// Reads (and failures), keyed by the period start. Held during a read, so requests that
/// overlap wait for it (at most `READ_LIMIT`) instead of asking Windows again.
static CACHE: Mutex<Option<HashMap<u64, (Instant, Read)>>> = Mutex::new(None);

/// Windows' record from `from` to `to` (seconds since the epoch), at most 5 minutes old.
pub(super) fn read_cached(from: u64, to: u64) -> Read {
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let cache = guard.get_or_insert_with(HashMap::new);
    cache.retain(|_, (at, read)| at.elapsed() < if read.is_ok() { CACHE_FOR } else { FAILED_FOR });
    if let Some((_, read)) = cache.get(&from) {
        return read.clone();
    }
    let read = read_with_limit(from, to);
    if let Err(err) = &read {
        eprintln!("Orbit network usage: {err} 7 and 30 days use Orbit's own counts.");
    }
    cache.insert(from, (Instant::now(), read.clone()));
    read
}

/// `read` on its own thread, given up after `READ_LIMIT` (the thread then ends on its own).
fn read_with_limit(from: u64, to: u64) -> Read {
    let (send, recv) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = send.send(read(from, to));
    });
    recv.recv_timeout(READ_LIMIT)
        .unwrap_or_else(|_| Err("Windows' data usage did not answer in time.".to_string()))
}

/// Seconds since the Unix epoch to a WinRT DateTime (100 ns ticks since 1601).
fn to_datetime(secs: u64) -> windows::Foundation::DateTime {
    windows::Foundation::DateTime { UniversalTime: secs as i64 * 10_000_000 + 116_444_736_000_000_000 }
}

fn read(from: u64, to: u64) -> Result<Vec<Raw>, String> {
    use windows::Networking::Connectivity::{NetworkInformation, NetworkUsageStates};

    let profiles = NetworkInformation::GetConnectionProfiles()
        .map_err(|e| format!("Windows' data usage could not be read ({e})."))?;
    let (start, end) = (to_datetime(from), to_datetime(to.max(from + 1)));
    let states = NetworkUsageStates::default();
    let mut sums: HashMap<String, (u64, u64)> = HashMap::new();
    for profile in &profiles {
        // A profile Windows has no record for fails; the others still count.
        let Ok(list) = profile.GetAttributedNetworkUsageAsync(start, end, states).and_then(|op| op.get()) else {
            continue;
        };
        for usage in &list {
            let id = usage.AttributionId().map(|h| h.to_string()).unwrap_or_default();
            let sum = sums.entry(id).or_default();
            sum.0 += usage.BytesReceived().unwrap_or(0);
            sum.1 += usage.BytesSent().unwrap_or(0);
        }
    }
    let mut raw: Vec<Raw> = sums.into_iter().map(|(id, (rx, tx))| Raw { id, rx, tx }).collect();
    // Sorted, so when two ids make one row the same one names it every time.
    raw.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(raw)
}

/// Windows' ids as rows keyed like App history: exe paths become drive paths (named
/// and cased as App history knows them, from `known`), Store apps go by package name,
/// and traffic tied to no app gets one row. Rows without bytes are dropped.
pub(super) fn to_apps(
    raw: &[Raw],
    devices: &[(String, String)],
    known: &HashMap<String, (String, Option<String>)>,
) -> Vec<UsageApp> {
    let mut by_key: HashMap<String, UsageApp> = HashMap::new();
    for r in raw {
        if r.rx + r.tx == 0 {
            continue;
        }
        let id = r.id.trim();
        let (key, name, exe) = if id.is_empty() {
            (UNATTRIBUTED.to_string(), "Not tied to an app".to_string(), None)
        } else if id.starts_with('\\') {
            match crate::winenum::nt_to_dos(id, devices) {
                Some(path) => {
                    let key = path.to_lowercase();
                    match known.get(&key) {
                        Some((name, exe)) => (key, name.clone(), exe.clone().or(Some(path))),
                        None => (key, file_name(&path), Some(path)),
                    }
                }
                None => (id.to_lowercase(), file_name(id), None),
            }
        } else if !id.contains('\\') && id.contains('_') {
            // Store app: "Publisher.App_hash" is shown as "Publisher.App".
            let name = id.rsplit_once('_').map_or(id, |(n, _)| n);
            (id.to_lowercase(), name.to_string(), None)
        } else {
            (id.to_lowercase(), id.to_string(), None)
        };
        let app = by_key
            .entry(key.clone())
            .or_insert_with(|| UsageApp { key, name, exe, rx_bytes: 0, tx_bytes: 0 });
        app.rx_bytes += r.rx;
        app.tx_bytes += r.tx;
    }
    by_key.into_values().collect()
}

fn file_name(path: &str) -> String {
    path.rsplit('\\').next().filter(|s| !s.is_empty()).unwrap_or(path).to_string()
}
