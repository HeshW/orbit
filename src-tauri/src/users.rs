//! Users: Windows sessions (WTS) and the session actions.
//!
//! v0.4 scaffold: stubs. The users feature fills this file in (see
//! docs/V04-CONTRACT.md).

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub session_id: u32,
    pub user_name: String,
    pub domain: String,
    /// "active" | "disconnected" | "connected" | "idle" | "listen" | "other".
    pub state: String,
    /// Remote client name, `None` for the console.
    pub client_name: Option<String>,
    /// Seconds since the Unix epoch.
    pub logon_time: Option<u64>,
    /// The session Orbit runs in.
    pub is_current: bool,
}

#[tauri::command]
pub async fn list_sessions() -> Result<Vec<SessionInfo>, String> {
    Err("Not built yet.".into())
}

/// `action`: "disconnect" | "signout" | "message" (`message` is required then).
#[tauri::command]
pub async fn session_action(
    session_id: u32,
    action: String,
    message: Option<String>,
) -> Result<(), String> {
    let _ = (session_id, action, message);
    Err("Not built yet.".into())
}
