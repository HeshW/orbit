//! App history: Orbit's own per-app usage totals, kept by day.
//!
//! v0.4 scaffold: stubs. The history feature fills this file in (see
//! docs/V04-CONTRACT.md). `record` runs on the sampler thread every tick and
//! must stay cheap (no disk write on every call).

use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use crate::model::Snapshot;

/// Totals for one app over the requested days.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct HistoryRow {
    /// Stable key: lowercased exe path, or the lowercased name when the path
    /// is unknown.
    pub key: String,
    pub name: String,
    pub exe: Option<String>,
    pub cpu_seconds: f64,
    /// Bytes.
    pub max_memory: u64,
    pub avg_memory: u64,
    pub disk_read_bytes: u64,
    pub disk_write_bytes: u64,
    /// 0 while per-app network data is unavailable.
    pub net_rx_bytes: u64,
    pub net_tx_bytes: u64,
    /// Seconds the app was running while Orbit sampled.
    pub active_seconds: f64,
    /// Seconds since the Unix epoch.
    pub first_seen: u64,
    pub last_seen: u64,
}

/// Called once from the Tauri setup hook. The sampler may record a tick
/// before this runs, so `record` must cope with being called first.
pub fn init(app: &AppHandle) {
    let _ = app;
}

/// Called by the sampler after every tick. `interval_secs` is how many
/// seconds this sample stands for (0 for the first one).
pub fn record(snapshot: &Snapshot, interval_secs: f64) {
    let _ = (snapshot, interval_secs);
}

/// Called once when Orbit exits (RunEvent::Exit): write anything pending.
pub fn flush() {}

#[tauri::command]
pub async fn get_history(days: u32) -> Result<Vec<HistoryRow>, String> {
    let _ = days;
    Err("Not built yet.".into())
}

#[tauri::command]
pub async fn clear_history() -> Result<(), String> {
    Err("Not built yet.".into())
}

/// The UI calls this at boot and whenever the setting changes.
#[tauri::command]
pub fn set_history_enabled(enabled: bool) -> Result<(), String> {
    let _ = enabled;
    Ok(())
}
