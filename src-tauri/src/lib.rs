//! Orbit backend: Tauri builder, shared state and command registration.

pub mod actions;
pub mod icons;
pub mod impact;
pub mod model;
pub mod pdh;
pub mod sampler;
pub mod services;
pub mod startup;
pub mod updates;
pub mod winenum;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tauri::Manager;

pub use sampler::AppState;

/// Builds and runs the Orbit application. Called from `main.rs`.
/// `orbit.exe --measure-startup` (scheduled by "Measure at next sign-in")
/// opens no window: it measures startup impact, writes the result and exits.
pub fn run() {
    if std::env::args().skip(1).any(|a| a == impact::MEASURE_FLAG) {
        impact::run_measurement();
        return;
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(AppState::default())
        .manage(model::SuspendedSet(Default::default()))
        .manage(icons::IconCache(Default::default()))
        .invoke_handler(tauri::generate_handler![
            // core
            sampler::get_snapshot,
            sampler::get_static_info,
            sampler::set_sampling,
            // actions
            actions::end_task,
            actions::end_tree,
            actions::suspend_process,
            actions::resume_process,
            actions::get_priority,
            actions::set_priority,
            actions::get_process_details,
            actions::open_location,
            actions::search_online,
            actions::open_url,
            actions::restart_as_admin,
            actions::export_report,
            actions::restart_process,
            actions::run_task,
            actions::get_affinity,
            actions::set_affinity,
            // icons
            icons::get_icons,
            // startup
            startup::list_startup,
            startup::set_startup_enabled,
            startup::get_startup_measure,
            startup::schedule_startup_measure,
            startup::cancel_startup_measure,
            // services
            services::list_services,
            services::control_service,
            // updates
            updates::check_update,
            updates::install_update,
        ])
        .setup(|app| {
            sampler::start(app.handle().clone());
            if let Some(window) = app.get_webview_window("main") {
                hide_webview_when_minimized(&window);
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("Orbit failed to start")
        .run(|app, event| {
            // Closing the window, app.exit(0) and Restart as administrator all
            // end here: wake whatever Orbit suspended before the process goes.
            if let tauri::RunEvent::Exit = event {
                actions::resume_all(app);
            }
        });
}

/// Minimizing does not tell WebView2 it is hidden, so it would keep drawing
/// frames and running timers at full rate. Windows sends a 0x0 resize on
/// minimize and a normal one on restore; on each change, set the WebView2
/// controller's visibility so the page reports `document.hidden`, stops
/// producing frames and throttles timers while minimized.
fn hide_webview_when_minimized(window: &tauri::WebviewWindow) {
    let hidden = Arc::new(AtomicBool::new(false));
    let w = window.clone();
    window.on_window_event(move |event| {
        if let tauri::WindowEvent::Resized(_) = event {
            let minimized = w.is_minimized().unwrap_or(false);
            if hidden.swap(minimized, Ordering::Relaxed) != minimized {
                let _ = w.with_webview(move |pw| {
                    // SAFETY: runs on the main thread with the live controller.
                    unsafe {
                        let _ = pw.controller().SetIsVisible(!minimized);
                    }
                });
            }
        }
    });
}
