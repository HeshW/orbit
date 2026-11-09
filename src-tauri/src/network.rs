//! Network: the connection table (TCP and UDP, IPv4 and IPv6, with the
//! owning pid) and per-app network rates for the sampler.
//!
//! v0.4 scaffold: stubs. The network feature fills this file in (see
//! docs/V04-CONTRACT.md).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::model::PerAppStatus;

/// One socket. `protocol`: "tcp" | "udp"; `family`: "ipv4" | "ipv6".
/// UDP rows have no remote end and no state.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    pub protocol: String,
    pub family: String,
    pub local_address: String,
    pub local_port: u16,
    pub remote_address: Option<String>,
    pub remote_port: Option<u16>,
    /// TCP state in camelCase: "listen" | "established" | "timeWait" |
    /// "closeWait" | "synSent" | ...
    pub state: Option<String>,
    pub pid: u32,
}

#[tauri::command]
pub async fn list_connections() -> Result<Vec<Connection>, String> {
    Err("Not built yet.".into())
}

#[tauri::command]
pub fn per_app_network_status() -> PerAppStatus {
    per_app_status()
}

/// Called by the sampler every tick for `NetworkStats::per_app`.
pub fn per_app_status() -> PerAppStatus {
    PerAppStatus {
        available: false,
        reason: Some("Per-app network is not built yet.".into()),
    }
}

/// Called by the sampler every tick: pid -> (received, sent) bytes/sec since
/// the previous call, or `None` when per-app data is unavailable (the row
/// fields then stay null). Must return quickly.
pub fn per_app_rates() -> Option<HashMap<u32, (f64, f64)>> {
    None
}

/// Called once when Orbit exits (RunEvent::Exit). Stop any trace session here.
pub fn shutdown() {}
