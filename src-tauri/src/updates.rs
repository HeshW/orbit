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
    // The installer exits this process without a RunEvent::Exit, so take the tray
    // icon down first or it lingers until the mouse passes over it.
    let handle = app.clone();
    let update = app
        .updater_builder()
        .on_before_exit(move || {
            let _ = handle.remove_tray_by_id(crate::shell::TRAY_ID);
        })
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
    // On Windows the installer takes over and this process exits without a
    // RunEvent::Exit, so wake anything Orbit suspended first.
    crate::actions::resume_all(&app);
    update
        .install(bytes)
        .map_err(|e| format!("The update couldn't be installed: {e}"))?;
    app.restart();
}
