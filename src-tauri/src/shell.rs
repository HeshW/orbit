//! Shell: tray icon, close to tray, start with Windows, the Ctrl+Shift+O
//! global hotkey, single instance and the mini window.
//!
//! v0.4 scaffold: stubs. The shell feature fills this file in (see
//! docs/V04-CONTRACT.md), including registering the global-shortcut,
//! autostart and single-instance plugins in `configure`.

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Builder, RunEvent, Wry};

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ShellState {
    pub tray_available: bool,
    /// Orbit starts with Windows.
    pub autostart: bool,
    /// Ctrl+Shift+O is registered.
    pub hotkey_registered: bool,
    /// Why the hotkey could not be registered, e.g. another app holds it.
    pub hotkey_error: Option<String>,
}

/// Called by lib.rs on the fresh builder, before any other plugin (the
/// single-instance plugin must be registered first).
pub fn configure(builder: Builder<Wry>) -> Builder<Wry> {
    builder
}

/// Called by lib.rs in the setup hook, after the sampler starts.
pub fn setup(app: &AppHandle) {
    let _ = app;
}

/// Called by lib.rs for every run event, before Orbit's own exit handling.
pub fn on_run_event(app: &AppHandle, event: &RunEvent) {
    let _ = (app, event);
}

#[tauri::command]
pub fn get_shell_state() -> ShellState {
    ShellState::default()
}

#[tauri::command]
pub fn set_autostart(enabled: bool) -> Result<(), String> {
    let _ = enabled;
    Err("Not built yet.".into())
}

#[tauri::command]
pub fn set_hotkey_enabled(enabled: bool) -> Result<(), String> {
    let _ = enabled;
    Err("Not built yet.".into())
}

/// Shows the mini window and hides the main one, or the other way round.
#[tauri::command]
pub fn toggle_mini() -> Result<(), String> {
    Err("Not built yet.".into())
}
