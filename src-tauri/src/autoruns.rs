//! Autoruns: scheduled tasks, services, logon keys and image hijacks, the
//! places programs use to start themselves beyond the Run keys.
//!
//! v0.4 scaffold: the command is a stub. The autoruns feature fills this file
//! in (see docs/V04-CONTRACT.md). Read only: nothing here writes the
//! Winlogon or Image File Execution Options keys.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Autoruns {
    pub tasks: Vec<AutorunTask>,
    pub services: Vec<AutorunService>,
    pub logon: LogonKeys,
    pub image_hijacks: Vec<ImageHijack>,
}

/// One scheduled task.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AutorunTask {
    /// Full task path, e.g. "\\Microsoft\\Windows\\Defrag\\ScheduledDefrag".
    pub name: String,
    /// Resolved executable, "" if the action is not an exe.
    pub path: String,
    pub author: Option<String>,
    pub enabled: bool,
    /// Plain words, e.g. "At log on", "Daily at 03:00".
    pub triggers: Vec<String>,
    /// Executable plus arguments.
    pub command: String,
    pub publisher: Option<String>,
    /// "signed" | "unsigned" | "invalid" | "unknown", `None` if not checked.
    pub signature_status: Option<String>,
}

/// One automatic service or driver.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AutorunService {
    pub name: String,
    pub display_name: String,
    /// Image path from the service config.
    pub path: String,
    /// As in `ServiceEntry::start_type`.
    pub start_type: String,
    pub publisher: Option<String>,
    pub signature_status: Option<String>,
    /// Signed by Microsoft.
    pub microsoft: bool,
}

/// HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon values.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct LogonKeys {
    pub shell: String,
    pub userinit: String,
    /// True when `shell` is the Windows default (explorer.exe).
    pub shell_ok: bool,
    /// True when `userinit` is the Windows default (userinit.exe).
    pub userinit_ok: bool,
}

/// An Image File Execution Options "Debugger" value.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ImageHijack {
    /// Image name, e.g. "taskmgr.exe".
    pub image: String,
    pub debugger: String,
}

#[tauri::command]
pub async fn list_autoruns() -> Result<Autoruns, String> {
    Err("Not built yet.".into())
}
