//! App updates from GitHub Releases (same approach as RiftLens).
//!
//! The release workflow publishes `latest.json` plus a signed installer; the
//! updater plugin reads the endpoint and public key from `tauri.conf.json`
//! (`plugins.updater`) and only installs updates signed with the matching key.

use serde::Serialize;
use tauri::AppHandle;
use tauri_plugin_updater::UpdaterExt;

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub version: String,
    pub current_version: String,
    pub notes: Option<String>,
    pub date: Option<String>,
}

/// Returns the newer release if there is one, or `None` when Orbit is up to date.
#[tauri::command]
pub async fn check_update(app: AppHandle) -> Result<Option<UpdateInfo>, String> {
    let update = app
        .updater()
        .map_err(|e| format!("Couldn't check for updates: {e}"))?
        .check()
        .await
        .map_err(|e| format!("Couldn't check for updates: {e}"))?;
    Ok(update.map(|u| UpdateInfo {
        version: u.version.clone(),
        current_version: u.current_version.clone(),
        notes: u.body.clone(),
        date: u.date.map(|d| d.to_string()),
    }))
}

/// Downloads and installs the newest release, then restarts into it.
#[tauri::command]
pub async fn install_update(app: AppHandle) -> Result<(), String> {
    // On Windows the installer exits this process without a RunEvent::Exit,
    // so run the exit cleanup (tray icon, suspended processes, network trace
    // session, pending history) right before it starts. If the installer
    // then fails to start, per-app network stays off until Orbit restarts.
    let handle = app.clone();
    let update = app
        .updater_builder()
        .on_before_exit(move || crate::exit_cleanup(&handle))
        .build()
        .map_err(|e| format!("Couldn't check for updates: {e}"))?
        .check()
        .await
        .map_err(|e| format!("Couldn't check for updates: {e}"))?
        .ok_or("Orbit is already up to date.")?;
    let bytes = update
        .download(|_, _| {}, || {})
        .await
        .map_err(|e| format!("The update couldn't be downloaded: {e}"))?;
    update
        .install(bytes)
        .map_err(|e| format!("The update couldn't be installed: {e}"))?;
    app.restart();
}
