//! Shell: tray icon, close to tray, start with Windows, the Ctrl+Shift+O
//! global hotkey, single instance and the mini window.
//!
//! The UI owns the settings (`store.settings` in localStorage). `shell.js`
//! sends the ones this module needs as the `orbit://shell-settings` event
//! (`{ showTray, closeToTray, startMinimized, paused }`) at boot and on every
//! change, and `set_hotkey_enabled` for the hotkey. They are also kept in
//! `shell.json` (app local data folder, or `ORBIT_SHELL_DIR`) so the next
//! start knows them before the page loads: whether to start hidden in the
//! tray, whether to show the tray icon and register the hotkey, and where the
//! mini window was.
//!
//! Other events: `orbit://shell-show` (from the mini window's "Open Orbit")
//! shows the main window; `orbit://shell-action` `{ action: "togglePause" }`
//! goes to the main window when the tray's Pause item is clicked (the UI owns
//! pausing); `orbit://shell-mini` `{ open }` goes to every window when the
//! mini window opens or closes.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Builder, Emitter, Listener, Manager, PhysicalPosition, RunEvent, WebviewUrl,
    WebviewWindow, WebviewWindowBuilder, WindowEvent, Wry,
};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt as _};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

/// Argument the Start with Windows entry passes: start hidden in the tray.
pub const MINIMIZED_FLAG: &str = "--minimized";
/// `--await-pid <pid>`: wait for that process to exit before taking the
/// single-instance lock (Restart as administrator passes Orbit's own pid).
pub const AWAIT_PID_FLAG: &str = "--await-pid";
const AWAIT_PID_TIMEOUT_MS: u32 = 10_000;

const MAIN: &str = "main";
const MINI: &str = "mini";
const TRAY_ID: &str = "orbit";
const HOTKEY_NAME: &str = "Ctrl+Shift+O";

const SETTINGS_EVENT: &str = "orbit://shell-settings";
const SHOW_EVENT: &str = "orbit://shell-show";
const ACTION_EVENT: &str = "orbit://shell-action";
const MINI_EVENT: &str = "orbit://shell-mini";

const MINI_WIDTH: f64 = 300.0;
const MINI_HEIGHT: f64 = 170.0;
/// Gap between the mini window and the screen edge on its first open (logical px).
const MINI_MARGIN: f64 = 16.0;
const TOOLTIP_EVERY: Duration = Duration::from_secs(2);

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

/// What `shell.json` keeps between runs.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
struct Prefs {
    show_tray: bool,
    close_to_tray: bool,
    start_minimized: bool,
    hotkey_enabled: bool,
    /// Mini window top-left corner, physical pixels.
    mini_x: Option<i32>,
    mini_y: Option<i32>,
}

impl Default for Prefs {
    fn default() -> Self {
        // Same defaults as store.js.
        Self {
            show_tray: true,
            close_to_tray: false,
            start_minimized: true,
            hotkey_enabled: true,
            mini_x: None,
            mini_y: None,
        }
    }
}

/// The `orbit://shell-settings` payload.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct SettingsMsg {
    show_tray: bool,
    close_to_tray: bool,
    start_minimized: bool,
    #[serde(default)]
    paused: bool,
}

#[derive(Default)]
struct Hotkey {
    registered: bool,
    error: Option<String>,
}

#[derive(Default)]
struct Shell {
    prefs: Mutex<Prefs>,
    file: Mutex<Option<PathBuf>>,
    hotkey: Mutex<Hotkey>,
    pause_item: Mutex<Option<MenuItem<Wry>>>,
    paused: AtomicBool,
    tray_failed: AtomicBool,
    exiting: AtomicBool,
}

fn shell() -> &'static Shell {
    static SHELL: OnceLock<Shell> = OnceLock::new();
    SHELL.get_or_init(Shell::default)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn hotkey() -> Shortcut {
    Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyO)
}

// ---------------------------------------------------------------------------
// Hooks called by lib.rs
// ---------------------------------------------------------------------------

/// Called by lib.rs on the fresh builder, before any other plugin (the
/// single-instance plugin must be registered first).
pub fn configure(builder: Builder<Wry>) -> Builder<Wry> {
    // Restart as administrator: the old Orbit is still exiting and holds the
    // single-instance lock. Without this wait the elevated Orbit would hand
    // its arguments to the old one and quit.
    if let Some(pid) = await_pid_arg(std::env::args().skip(1)) {
        wait_for_exit(pid, AWAIT_PID_TIMEOUT_MS);
    }
    builder
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main(app)))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state == ShortcutState::Pressed {
                        toggle_main(app);
                    }
                })
                .build(),
        )
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec![MINIMIZED_FLAG]),
        ))
}

/// Called by lib.rs in the setup hook, after the sampler starts.
pub fn setup(app: &AppHandle) {
    let file = prefs_file(app);
    let prefs = file.as_ref().map(|f| load_prefs(f)).unwrap_or_default();
    *lock(&shell().file) = file;
    *lock(&shell().prefs) = prefs.clone();

    if prefs.show_tray {
        ensure_tray(app);
    }
    if prefs.hotkey_enabled {
        register_hotkey(app);
    }
    allow_copydata_when_elevated(app);

    let handle = app.clone();
    app.listen_any(SETTINGS_EVENT, move |event| {
        match serde_json::from_str::<SettingsMsg>(event.payload()) {
            Ok(msg) => apply_settings(&handle, msg),
            Err(err) => eprintln!("[orbit] bad {SETTINGS_EVENT} payload: {err}"),
        }
    });
    let handle = app.clone();
    app.listen_any(SHOW_EVENT, move |_| show_main(&handle));

    if let Some(window) = app.get_webview_window(MAIN) {
        let handle = app.clone();
        let w = window.clone();
        window.on_window_event(move |event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                if close_to_tray(&handle) {
                    api.prevent_close();
                    hide_main(&w);
                } else if handle.get_webview_window(MINI).is_some() {
                    // Closing the main window quits Orbit, mini window included.
                    api.prevent_close();
                    handle.exit(0);
                }
            }
        });
        // The window starts hidden (tauri.conf.json) so a start in the tray never flashes.
        let launched_minimized = std::env::args().skip(1).any(|a| a == MINIMIZED_FLAG);
        if launched_minimized && prefs.start_minimized && tray_visible(app) {
            set_webview_visible(&window, false);
        } else {
            let _ = window.show();
            let _ = window.set_focus();
        }
    }

    spawn_tooltip_thread(app.clone());
}

/// Called by lib.rs for every run event, before Orbit's own exit handling.
pub fn on_run_event(app: &AppHandle, event: &RunEvent) {
    if let RunEvent::Exit = event {
        shell().exiting.store(true, Ordering::Relaxed);
        if let Some(mini) = app.get_webview_window(MINI) {
            remember_mini_position(&mini);
        }
        save_prefs();
        // Remove the icon now so it does not linger in the tray after exit.
        let _ = app.remove_tray_by_id(TRAY_ID);
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn get_shell_state(app: AppHandle) -> ShellState {
    let s = shell();
    let hk = lock(&s.hotkey);
    ShellState {
        tray_available: !s.tray_failed.load(Ordering::Relaxed),
        autostart: app.autolaunch().is_enabled().unwrap_or(false),
        hotkey_registered: hk.registered,
        hotkey_error: hk.error.clone(),
    }
}

#[tauri::command]
pub fn set_autostart(app: AppHandle, enabled: bool) -> Result<(), String> {
    let manager = app.autolaunch();
    if enabled {
        // Elevated, the plugin would write the machine-wide (HKLM) Run key,
        // which starts Orbit for every user. Keep it per user.
        if is_elevated() {
            return Err(
                "Orbit is running as administrator. Open it normally to turn this on.".into(),
            );
        }
        manager
            .enable()
            .map_err(|e| format!("Windows didn't save the startup entry: {e}."))
    } else {
        manager
            .disable()
            .map_err(|e| format!("Windows didn't remove the startup entry: {e}."))?;
        // enable() also writes an "enabled" StartupApproved value; remove it
        // too so turning this off leaves nothing behind.
        remove_startup_approved(&app.package_info().name);
        Ok(())
    }
}

/// Registers or unregisters Ctrl+Shift+O. Always Ok: a hotkey another app
/// holds is reported by `get_shell_state`.
#[tauri::command]
pub fn set_hotkey_enabled(app: AppHandle, enabled: bool) -> Result<(), String> {
    let changed = {
        let mut prefs = lock(&shell().prefs);
        let changed = prefs.hotkey_enabled != enabled;
        prefs.hotkey_enabled = enabled;
        changed
    };
    if changed {
        save_prefs();
    }
    if enabled {
        register_hotkey(&app);
    } else {
        unregister_hotkey(&app);
    }
    Ok(())
}

/// Opens the mini window, or closes it when it is open.
#[tauri::command]
pub async fn toggle_mini(app: AppHandle) -> Result<(), String> {
    // async: creating a window from a sync command can deadlock on Windows.
    toggle_mini_window(&app)
}

// ---------------------------------------------------------------------------
// Main window
// ---------------------------------------------------------------------------

/// Shows, unminimizes and focuses the main window.
fn show_main(app: &AppHandle) {
    let Some(w) = app.get_webview_window(MAIN) else { return };
    set_webview_visible(&w, true);
    let _ = w.show();
    if w.is_minimized().unwrap_or(false) {
        let _ = w.unminimize();
    }
    let _ = w.set_focus();
}

/// Hides the main window to the tray. WebView2 is told it is hidden too, so
/// the page stops drawing frames while Orbit sits in the tray.
fn hide_main(w: &WebviewWindow) {
    set_webview_visible(w, false);
    let _ = w.hide();
}

/// The hotkey: show and focus Orbit, or put it away when it is already in front.
fn toggle_main(app: &AppHandle) {
    let Some(w) = app.get_webview_window(MAIN) else { return };
    let in_front = w.is_visible().unwrap_or(false)
        && !w.is_minimized().unwrap_or(false)
        && w.is_focused().unwrap_or(false);
    if !in_front {
        show_main(app);
    } else if tray_visible(app) {
        hide_main(&w);
    } else {
        // No tray icon to come back from: minimize to the taskbar instead.
        let _ = w.minimize();
    }
}

fn set_webview_visible(w: &WebviewWindow, visible: bool) {
    let _ = w.with_webview(move |pw| {
        // SAFETY: runs on the main thread with the live controller.
        unsafe {
            let _ = pw.controller().SetIsVisible(visible);
        }
    });
}

fn close_to_tray(app: &AppHandle) -> bool {
    lock(&shell().prefs).close_to_tray && tray_visible(app)
}

// ---------------------------------------------------------------------------
// Tray
// ---------------------------------------------------------------------------

fn tray_visible(app: &AppHandle) -> bool {
    lock(&shell().prefs).show_tray && app.tray_by_id(TRAY_ID).is_some()
}

fn pause_label(paused: bool) -> &'static str {
    if paused {
        "Resume updates"
    } else {
        "Pause updates"
    }
}

/// Creates the tray icon once. False if Windows refused it.
fn ensure_tray(app: &AppHandle) -> bool {
    if app.tray_by_id(TRAY_ID).is_some() {
        return true;
    }
    match build_tray(app) {
        Ok(()) => {
            shell().tray_failed.store(false, Ordering::Relaxed);
            true
        }
        Err(err) => {
            eprintln!("[orbit] tray icon failed: {err}");
            shell().tray_failed.store(true, Ordering::Relaxed);
            false
        }
    }
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let paused = shell().paused.load(Ordering::Relaxed);
    let show = MenuItem::with_id(app, "show", "Show Orbit", true, None::<&str>)?;
    let mini = MenuItem::with_id(app, "mini", "Mini mode", true, None::<&str>)?;
    let pause = MenuItem::with_id(app, "pause", pause_label(paused), true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &mini, &pause, &sep, &quit])?;
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip(tooltip_text(app))
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_main(app),
            "mini" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(err) = toggle_mini_window(&app) {
                        eprintln!("[orbit] {err}");
                    }
                });
            }
            "pause" => {
                let _ = app.emit_to(MAIN, ACTION_EVENT, serde_json::json!({ "action": "togglePause" }));
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    *lock(&shell().pause_item) = Some(pause);
    Ok(())
}

fn set_tray_shown(app: &AppHandle, shown: bool) {
    if shown {
        if ensure_tray(app) {
            if let Some(tray) = app.tray_by_id(TRAY_ID) {
                let _ = tray.set_visible(true);
            }
        }
    } else if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_visible(false);
    }
}

/// "Orbit · CPU 23% · Memory 61%" from the sampler's latest snapshot.
fn tooltip_text(app: &AppHandle) -> String {
    let state = app.state::<crate::AppState>();
    let numbers = state
        .latest
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|s| {
            let mem = if s.memory.total > 0 {
                s.memory.used as f64 * 100.0 / s.memory.total as f64
            } else {
                0.0
            };
            (s.cpu.usage, mem)
        });
    format_tooltip(numbers, shell().paused.load(Ordering::Relaxed))
}

fn format_tooltip(numbers: Option<(f64, f64)>, paused: bool) -> String {
    let mut out = String::from("Orbit");
    if paused {
        out.push_str(" · Paused");
    }
    if let Some((cpu, mem)) = numbers {
        let cpu = cpu.clamp(0.0, 100.0).round();
        let mem = mem.clamp(0.0, 100.0).round();
        out.push_str(&format!(" · CPU {cpu}% · Memory {mem}%"));
    }
    out
}

fn spawn_tooltip_thread(app: AppHandle) {
    let spawned = std::thread::Builder::new()
        .name("orbit-tray".into())
        .spawn(move || {
            let mut last = String::new();
            loop {
                std::thread::sleep(TOOLTIP_EVERY);
                if shell().exiting.load(Ordering::Relaxed) {
                    break;
                }
                if !tray_visible(&app) {
                    continue;
                }
                let text = tooltip_text(&app);
                if text != last {
                    if let Some(tray) = app.tray_by_id(TRAY_ID) {
                        let _ = tray.set_tooltip(Some(&text));
                    }
                    last = text;
                }
            }
        });
    if let Err(err) = spawned {
        eprintln!("[orbit] tray tooltip thread failed: {err}");
    }
}

/// The UI's settings arrived (boot and every change).
fn apply_settings(app: &AppHandle, msg: SettingsMsg) {
    let was_paused = shell().paused.swap(msg.paused, Ordering::Relaxed);
    if was_paused != msg.paused {
        if let Some(item) = lock(&shell().pause_item).as_ref() {
            let _ = item.set_text(pause_label(msg.paused));
        }
    }
    let changed = {
        let mut prefs = lock(&shell().prefs);
        let next = Prefs {
            show_tray: msg.show_tray,
            close_to_tray: msg.close_to_tray,
            start_minimized: msg.start_minimized,
            ..prefs.clone()
        };
        let changed = *prefs != next;
        *prefs = next;
        changed
    };
    set_tray_shown(app, msg.show_tray);
    if changed {
        save_prefs();
    }
}

// ---------------------------------------------------------------------------
// Hotkey
// ---------------------------------------------------------------------------

fn register_hotkey(app: &AppHandle) {
    let gs = app.global_shortcut();
    let result = if gs.is_registered(hotkey()) {
        Ok(())
    } else {
        gs.register(hotkey())
    };
    let mut hk = lock(&shell().hotkey);
    match result {
        Ok(()) => {
            hk.registered = true;
            hk.error = None;
        }
        Err(err) => {
            hk.registered = false;
            hk.error = Some(hotkey_error_text(&err.to_string()));
        }
    }
}

fn unregister_hotkey(app: &AppHandle) {
    let gs = app.global_shortcut();
    if gs.is_registered(hotkey()) {
        let _ = gs.unregister(hotkey());
    }
    let mut hk = lock(&shell().hotkey);
    hk.registered = false;
    hk.error = None;
}

/// global-hotkey reports a combination another app holds as
/// "HotKey already registered: ...".
fn hotkey_error_text(raw: &str) -> String {
    if raw.to_ascii_lowercase().contains("already registered") {
        format!("Another app is using {HOTKEY_NAME}.")
    } else {
        format!("Windows didn't accept {HOTKEY_NAME}: {raw}.")
    }
}

// ---------------------------------------------------------------------------
// Mini window
// ---------------------------------------------------------------------------

fn emit_mini(app: &AppHandle, open: bool) {
    let _ = app.emit(MINI_EVENT, serde_json::json!({ "open": open }));
}

fn toggle_mini_window(app: &AppHandle) -> Result<(), String> {
    if let Some(mini) = app.get_webview_window(MINI) {
        // The Destroyed handler saves the position and tells the windows.
        remember_mini_position(&mini);
        return mini
            .destroy()
            .map_err(|e| format!("Orbit couldn't close mini mode: {e}."));
    }
    let mini = WebviewWindowBuilder::new(app, MINI, WebviewUrl::App("mini.html".into()))
        .title("Orbit mini")
        .inner_size(MINI_WIDTH, MINI_HEIGHT)
        .resizable(false)
        .maximizable(false)
        .minimizable(false)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible(false)
        .background_color(tauri::window::Color(5, 11, 24, 255))
        .build()
        .map_err(|e| format!("Orbit couldn't open mini mode: {e}."))?;

    let pos = mini_position(app, &mini);
    let _ = mini.set_position(pos);
    let w = mini.clone();
    let handle = app.clone();
    mini.on_window_event(move |event| match event {
        WindowEvent::Moved(_) => remember_mini_position(&w),
        WindowEvent::Destroyed => {
            save_prefs();
            if !shell().exiting.load(Ordering::Relaxed) {
                emit_mini(&handle, false);
            }
        }
        _ => {}
    });
    let _ = mini.show();
    let _ = mini.set_focus();
    emit_mini(app, true);
    Ok(())
}

fn remember_mini_position(mini: &WebviewWindow) {
    // Skip the off-screen position Windows reports for minimized windows.
    if mini.is_minimized().unwrap_or(false) {
        return;
    }
    if let Ok(p) = mini.outer_position() {
        let mut prefs = lock(&shell().prefs);
        prefs.mini_x = Some(p.x);
        prefs.mini_y = Some(p.y);
    }
}

/// The saved position when it is still on a screen, else the bottom-right
/// corner of the work area of the monitor Orbit is on.
fn mini_position(app: &AppHandle, mini: &WebviewWindow) -> PhysicalPosition<i32> {
    let size = mini
        .outer_size()
        .map(|s| (s.width as i32, s.height as i32))
        .unwrap_or((MINI_WIDTH as i32, MINI_HEIGHT as i32));
    let monitors: Vec<Rect> = app
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|m| {
            let wa = m.work_area();
            Rect { x: wa.position.x, y: wa.position.y, w: wa.size.width as i32, h: wa.size.height as i32 }
        })
        .collect();
    let saved = {
        let prefs = lock(&shell().prefs);
        prefs.mini_x.zip(prefs.mini_y)
    };
    if let Some((x, y)) = saved {
        if on_screen(x, y, size, &monitors) {
            return PhysicalPosition::new(x, y);
        }
    }
    let monitor = app
        .get_webview_window(MAIN)
        .and_then(|w| w.current_monitor().ok().flatten())
        .or_else(|| app.primary_monitor().ok().flatten());
    match monitor {
        Some(m) => {
            let wa = m.work_area();
            let area = Rect { x: wa.position.x, y: wa.position.y, w: wa.size.width as i32, h: wa.size.height as i32 };
            let margin = (MINI_MARGIN * m.scale_factor()).round() as i32;
            let (x, y) = corner_position(&area, size, margin);
            PhysicalPosition::new(x, y)
        }
        None => PhysicalPosition::new(100, 100),
    }
}

#[derive(Clone, Copy, Debug)]
struct Rect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

/// True when the window's title strip (top 32 px, middle half) is on some
/// screen, so it can still be dragged.
fn on_screen(x: i32, y: i32, size: (i32, i32), screens: &[Rect]) -> bool {
    let (w, _) = size;
    let px = x + w / 2;
    let py = y + 16.min(size.1 / 2);
    screens
        .iter()
        .any(|r| px >= r.x && px < r.x + r.w && py >= r.y && py < r.y + r.h)
}

fn corner_position(area: &Rect, size: (i32, i32), margin: i32) -> (i32, i32) {
    let x = (area.x + area.w - size.0 - margin).max(area.x);
    let y = (area.y + area.h - size.1 - margin).max(area.y);
    (x, y)
}

// ---------------------------------------------------------------------------
// shell.json
// ---------------------------------------------------------------------------

fn prefs_file(app: &AppHandle) -> Option<PathBuf> {
    let dir = match std::env::var_os("ORBIT_SHELL_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => app.path().app_local_data_dir().ok()?,
    };
    Some(dir.join("shell.json"))
}

fn load_prefs(file: &PathBuf) -> Prefs {
    std::fs::read_to_string(file)
        .ok()
        .and_then(|text| serde_json::from_str(text.trim_start_matches('\u{feff}')).ok())
        .unwrap_or_default()
}

fn save_prefs() {
    let Some(file) = lock(&shell().file).clone() else { return };
    let prefs = lock(&shell().prefs).clone();
    if load_prefs(&file) == prefs && file.exists() {
        return;
    }
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(&prefs) {
        if let Err(err) = std::fs::write(&file, text) {
            eprintln!("[orbit] couldn't save {}: {err}", file.display());
        }
    }
}

// ---------------------------------------------------------------------------
// Windows helpers
// ---------------------------------------------------------------------------

/// The pid after `--await-pid`, if any.
fn await_pid_arg<I: IntoIterator<Item = String>>(args: I) -> Option<u32> {
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        if arg == AWAIT_PID_FLAG {
            return it.next().and_then(|v| v.trim().parse::<u32>().ok()).filter(|&p| p != 0);
        }
        if let Some(v) = arg.strip_prefix("--await-pid=") {
            return v.trim().parse::<u32>().ok().filter(|&p| p != 0);
        }
    }
    None
}

/// Waits up to `timeout_ms` for `pid` to exit. Returns at once when the
/// process is already gone (or is this one).
fn wait_for_exit(pid: u32, timeout_ms: u32) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
    if pid == std::process::id() {
        return;
    }
    // SAFETY: plain Win32 calls; the handle is closed before returning.
    unsafe {
        if let Ok(handle) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            let _ = WaitForSingleObject(handle, timeout_ms);
            let _ = CloseHandle(handle);
        }
    }
}

fn is_elevated() -> bool {
    // SAFETY: IsUserAnAdmin takes no arguments and only reads the process token.
    unsafe { windows::Win32::UI::Shell::IsUserAnAdmin() }.as_bool()
}

/// When Orbit runs elevated, Windows drops WM_COPYDATA from normal
/// processes, so a second (normal) launch could not reach this one. Let the
/// single-instance plugin's message window accept it.
fn allow_copydata_when_elevated(app: &AppHandle) {
    use windows::core::PCWSTR;
    use windows::Win32::UI::WindowsAndMessaging::{
        ChangeWindowMessageFilterEx, FindWindowW, GetWindowThreadProcessId, MSGFLT_ALLOW,
        WM_COPYDATA,
    };
    if !is_elevated() {
        return;
    }
    let id = &app.config().identifier;
    let wide = |s: String| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let class = wide(format!("{id}-sic"));
    let title = wide(format!("{id}-siw"));
    // SAFETY: plain Win32 calls on a window this process owns (checked by pid).
    unsafe {
        let Ok(hwnd) = FindWindowW(PCWSTR(class.as_ptr()), PCWSTR(title.as_ptr())) else { return };
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == std::process::id() {
            let _ = ChangeWindowMessageFilterEx(hwnd, WM_COPYDATA, MSGFLT_ALLOW, None);
        }
    }
}

/// Removes HKCU ...\Explorer\StartupApproved\Run\<name> (absent is fine).
fn remove_startup_approved(name: &str) {
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::{RegDeleteKeyValueW, HKEY_CURRENT_USER};
    let key: Vec<u16> = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let value: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: both strings are NUL-terminated and outlive the call.
    unsafe {
        let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, PCWSTR(key.as_ptr()), PCWSTR(value.as_ptr()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn await_pid_parsing() {
        assert_eq!(await_pid_arg(args(&["--await-pid", "1234"])), Some(1234));
        assert_eq!(await_pid_arg(args(&["--minimized", "--await-pid", "88"])), Some(88));
        assert_eq!(await_pid_arg(args(&["--await-pid=77"])), Some(77));
        assert_eq!(await_pid_arg(args(&["--await-pid"])), None);
        assert_eq!(await_pid_arg(args(&["--await-pid", "x"])), None);
        assert_eq!(await_pid_arg(args(&["--await-pid", "0"])), None);
        assert_eq!(await_pid_arg(args(&["--minimized"])), None);
    }

    #[test]
    fn wait_for_exit_returns_for_gone_and_own_pid() {
        let t = std::time::Instant::now();
        wait_for_exit(std::process::id(), 5_000);
        // A pid that is not running (pids are multiples of 4, so 3 never is).
        wait_for_exit(3, 5_000);
        assert!(t.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn wait_for_exit_waits_for_a_child() {
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "ping -n 2 127.0.0.1 >NUL"])
            .spawn()
            .expect("start cmd");
        let t = std::time::Instant::now();
        wait_for_exit(child.id(), 10_000);
        assert!(t.elapsed() >= Duration::from_millis(500), "returned before the child exited");
        assert!(child.try_wait().expect("try_wait").is_some(), "child still running");
    }

    #[test]
    fn tooltip_text() {
        assert_eq!(format_tooltip(Some((23.4, 61.2)), false), "Orbit · CPU 23% · Memory 61%");
        assert_eq!(format_tooltip(Some((0.0, 100.4)), true), "Orbit · Paused · CPU 0% · Memory 100%");
        assert_eq!(format_tooltip(None, false), "Orbit");
    }

    #[test]
    fn hotkey_errors() {
        assert_eq!(
            hotkey_error_text("HotKey already registered: HotKey { .. }"),
            "Another app is using Ctrl+Shift+O."
        );
        assert!(hotkey_error_text("Access is denied. (os error 5)").starts_with("Windows didn't accept"));
    }

    #[test]
    fn mini_placement() {
        let screens = [Rect { x: 0, y: 0, w: 1920, h: 1040 }, Rect { x: 1920, y: 0, w: 2560, h: 1400 }];
        assert!(on_screen(1600, 860, (300, 170), &screens));
        assert!(on_screen(1800, 10, (300, 170), &screens)); // straddles the two screens
        assert!(!on_screen(5000, 100, (300, 170), &screens));
        assert!(!on_screen(-400, 100, (300, 170), &screens));
        assert!(!on_screen(100, 1030, (300, 170), &screens)); // title strip below the work area
        assert_eq!(corner_position(&screens[0], (300, 170), 16), (1604, 854));
        assert_eq!(corner_position(&Rect { x: 0, y: 0, w: 200, h: 100 }, (300, 170), 16), (0, 0));
    }

    #[test]
    fn prefs_defaults_and_partial_file() {
        let p: Prefs = serde_json::from_str(r#"{"closeToTray":true}"#).unwrap();
        assert!(p.show_tray && p.close_to_tray && p.start_minimized && p.hotkey_enabled);
        assert_eq!(p.mini_x, None);
        let msg: SettingsMsg =
            serde_json::from_str(r#"{"showTray":false,"closeToTray":true,"startMinimized":false}"#).unwrap();
        assert!(!msg.show_tray && msg.close_to_tray && !msg.start_minimized && !msg.paused);
    }
}
