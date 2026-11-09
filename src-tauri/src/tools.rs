//! Tools: find which processes lock a file (Restart Manager).
//!
//! v0.4 scaffold: stub. The tools feature fills this file in (see
//! docs/V04-CONTRACT.md).

use serde::{Deserialize, Serialize};

/// A process that has the file open. `app_type`: "app" | "service" |
/// "explorer" | "console" | "critical" | "unknown".
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Locker {
    pub pid: u32,
    pub name: String,
    pub exe: Option<String>,
    pub app_type: String,
}

#[tauri::command]
pub async fn find_file_lockers(path: String) -> Result<Vec<Locker>, String> {
    let _ = path;
    Err("Not built yet.".into())
}
