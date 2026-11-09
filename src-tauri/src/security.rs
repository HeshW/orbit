//! Security: Authenticode signatures, a heuristic process scan, file hashes,
//! loaded modules and environment of a process.
//!
//! v0.4 scaffold: every command is a stub. The security feature fills this
//! file in (see docs/V04-CONTRACT.md); the command names, arguments and
//! return shapes below are fixed by the contract.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

const NOT_BUILT: &str = "Not built yet.";

/// Signature of one file. `status`: "signed" | "unsigned" | "invalid" | "unknown".
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SignatureInfo {
    pub status: String,
    /// Signer's common name, e.g. "Microsoft Windows".
    pub publisher: Option<String>,
}

/// One process worth a look. `severity`: "high" | "medium" | "info".
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SecurityFinding {
    pub pid: u32,
    pub name: String,
    pub exe: Option<String>,
    pub severity: String,
    /// Plain sentences, e.g. "Runs from a temporary folder.".
    pub reasons: Vec<String>,
    /// As in `SignatureInfo::status`.
    pub signature_status: String,
    pub publisher: Option<String>,
}

/// One DLL or other module loaded in a process.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ModuleInfo {
    pub path: String,
    /// Bytes.
    pub size: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct EnvVar {
    pub name: String,
    pub value: String,
}

/// Signature per path (path -> info). Paths that can't be read are reported
/// as "unknown".
#[tauri::command]
pub async fn get_signatures(paths: Vec<String>) -> Result<HashMap<String, SignatureInfo>, String> {
    let _ = paths;
    Err(NOT_BUILT.into())
}

/// Checks every running process and returns the ones worth a look.
#[tauri::command]
pub async fn security_scan() -> Result<Vec<SecurityFinding>, String> {
    Err(NOT_BUILT.into())
}

/// SHA-256 of a file as lowercase hex.
#[tauri::command]
pub async fn file_sha256(path: String) -> Result<String, String> {
    let _ = path;
    Err(NOT_BUILT.into())
}

/// Modules loaded in a process. `start_time` guards against pid reuse as in
/// the process actions (omitted or 0 skips the check).
#[tauri::command]
pub async fn get_process_modules(
    pid: u32,
    start_time: Option<u64>,
) -> Result<Vec<ModuleInfo>, String> {
    let _ = (pid, start_time);
    Err(NOT_BUILT.into())
}

/// Environment variables of a process.
#[tauri::command]
pub async fn get_process_environment(pid: u32) -> Result<Vec<EnvVar>, String> {
    let _ = pid;
    Err(NOT_BUILT.into())
}
