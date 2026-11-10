//! Windows services: list (status, PID, start type, description) and
//! start / stop / restart through the Service Control Manager, and the start
//! type for Optimize > Debloat (Manual or back to Automatic, never Disabled).

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::System::Services::{
    CloseServiceHandle, ControlService, EnumServicesStatusExW, OpenSCManagerW, OpenServiceW,
    QueryServiceConfig2W, QueryServiceConfigW, QueryServiceStatusEx, StartServiceW,
    ENUM_SERVICE_STATUS_PROCESSW, QUERY_SERVICE_CONFIGW, SC_ENUM_PROCESS_INFO, SC_HANDLE,
    SC_MANAGER_CONNECT, SC_MANAGER_ENUMERATE_SERVICE, SC_STATUS_PROCESS_INFO,
    SERVICE_AUTO_START, SERVICE_BOOT_START, SERVICE_CONFIG_DELAYED_AUTO_START_INFO,
    SERVICE_CONFIG_DESCRIPTION, SERVICE_CONTINUE_PENDING, SERVICE_CONTROL_STOP,
    SERVICE_DELAYED_AUTO_START_INFO, SERVICE_DEMAND_START, SERVICE_DESCRIPTIONW,
    SERVICE_DISABLED, SERVICE_PAUSED, SERVICE_PAUSE_PENDING, SERVICE_QUERY_CONFIG,
    SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_START, SERVICE_START_PENDING,
    SERVICE_STATE_ALL, SERVICE_STATUS, SERVICE_STATUS_CURRENT_STATE, SERVICE_STATUS_PROCESS,
    SERVICE_STOP, SERVICE_STOPPED, SERVICE_STOP_PENDING, SERVICE_SYSTEM_START, SERVICE_WIN32,
};

use crate::model::*;

const ACCESS_DENIED_MSG: &str = "Access denied. Try Restart as administrator.";

// Win32 error codes we translate into friendly text.
const ERR_ACCESS_DENIED: u32 = 5;
const ERR_MORE_DATA: u32 = 234;
const ERR_DEPENDENT_SERVICES_RUNNING: u32 = 1051;
const ERR_INVALID_SERVICE_CONTROL: u32 = 1052;
const ERR_SERVICE_REQUEST_TIMEOUT: u32 = 1053;
const ERR_SERVICE_ALREADY_RUNNING: u32 = 1056;
const ERR_SERVICE_DISABLED: u32 = 1058;
const ERR_SERVICE_DOES_NOT_EXIST: u32 = 1060;
const ERR_SERVICE_CANNOT_ACCEPT_CTRL: u32 = 1061;
const ERR_SERVICE_NOT_ACTIVE: u32 = 1062;
const ERR_SERVICE_DEPENDENCY_FAIL: u32 = 1068;
const ERR_SERVICE_LOGON_FAILED: u32 = 1069;
const ERR_SERVICE_MARKED_FOR_DELETE: u32 = 1072;
const ERR_SERVICE_DEPENDENCY_DELETED: u32 = 1075;

const WAIT_STEP: Duration = Duration::from_millis(250);
const WAIT_BASE: Duration = Duration::from_secs(15);
const WAIT_MAX: Duration = Duration::from_secs(30);

/// How long cached start types and descriptions are trusted. Status and pid
/// are read fresh on every list; config rarely changes.
const CONFIG_TTL: Duration = Duration::from_secs(60);

/// Service name -> (start type, description).
type ConfigMap = HashMap<String, (&'static str, Option<String>)>;

/// Start type and description per service, so a list refresh does one
/// EnumServicesStatusExW call instead of several SCM calls per service.
static CONFIG_CACHE: Mutex<Option<(Instant, ConfigMap)>> = Mutex::new(None);

fn config_cache() -> MutexGuard<'static, Option<(Instant, ConfigMap)>> {
    CONFIG_CACHE.lock().unwrap_or_else(|p| p.into_inner())
}

/// Drops one service from the cache so the next list re-reads its config.
pub(crate) fn forget_config(name: &str) {
    if let Some((_, map)) = config_cache().as_mut() {
        map.remove(name);
    }
}

// ---------------------------------------------------------------------------
// Handles and small helpers
// ---------------------------------------------------------------------------

/// Owns an SCM or service handle and closes it on drop.
struct ScHandle(SC_HANDLE);

impl Drop for ScHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseServiceHandle(self.0);
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn pwstr_to_string(p: PWSTR) -> Option<String> {
    if p.is_null() {
        None
    } else {
        unsafe { p.to_string().ok() }
    }
}

/// Views a u64 buffer (8-byte aligned, as the SCM structures need) as bytes.
fn as_bytes_mut(buf: &mut [u64]) -> &mut [u8] {
    unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, buf.len() * 8) }
}

fn words_for(bytes: u32) -> usize {
    (bytes as usize + 7) / 8
}

/// Extracts the Win32 error code from an HRESULT_FROM_WIN32 value.
fn win32_code(e: &windows::core::Error) -> u32 {
    let hr = e.code().0 as u32;
    if hr & 0xFFFF_0000 == 0x8007_0000 {
        hr & 0xFFFF
    } else {
        hr
    }
}

fn friendly_error(e: &windows::core::Error, context: &str) -> String {
    let code = win32_code(e);
    match code {
        ERR_ACCESS_DENIED => ACCESS_DENIED_MSG.to_string(),
        ERR_SERVICE_ALREADY_RUNNING => "The service is already running.".to_string(),
        ERR_SERVICE_NOT_ACTIVE => "The service is not running.".to_string(),
        ERR_DEPENDENT_SERVICES_RUNNING => {
            "Other running services depend on this one. Stop them first.".to_string()
        }
        ERR_INVALID_SERVICE_CONTROL => "This service cannot be stopped.".to_string(),
        ERR_SERVICE_CANNOT_ACCEPT_CTRL => {
            "The service cannot accept that request right now. Try again shortly.".to_string()
        }
        ERR_SERVICE_REQUEST_TIMEOUT => "The service did not respond in time.".to_string(),
        ERR_SERVICE_DISABLED => {
            "The service is disabled. Set its start type to Manual or Automatic first.".to_string()
        }
        ERR_SERVICE_DOES_NOT_EXIST => "Service not found. Refresh the list.".to_string(),
        ERR_SERVICE_DEPENDENCY_FAIL | ERR_SERVICE_DEPENDENCY_DELETED => {
            "A service this one depends on could not start.".to_string()
        }
        ERR_SERVICE_LOGON_FAILED => "The service could not sign in with its account.".to_string(),
        ERR_SERVICE_MARKED_FOR_DELETE => "The service is being removed.".to_string(),
        _ => format!("{context} (error {code})."),
    }
}

fn state_name(state: SERVICE_STATUS_CURRENT_STATE) -> &'static str {
    if state == SERVICE_RUNNING {
        "running"
    } else if state == SERVICE_STOPPED {
        "stopped"
    } else if state == SERVICE_START_PENDING {
        "startPending"
    } else if state == SERVICE_STOP_PENDING {
        "stopPending"
    } else if state == SERVICE_PAUSED {
        "paused"
    } else if state == SERVICE_PAUSE_PENDING {
        "pausePending"
    } else if state == SERVICE_CONTINUE_PENDING {
        "continuePending"
    } else {
        "unknown"
    }
}

fn open_scm(access: u32) -> Result<ScHandle, String> {
    unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), access) }
        .map(ScHandle)
        .map_err(|e| friendly_error(&e, "Could not open the service manager"))
}

// ---------------------------------------------------------------------------
// Listing
// ---------------------------------------------------------------------------

struct RawService {
    name: String,
    display_name: String,
    state: SERVICE_STATUS_CURRENT_STATE,
    pid: u32,
}

/// EnumServicesStatusExW with the grow-buffer + resume-handle loop.
fn enumerate(scm: &ScHandle) -> Result<Vec<RawService>, String> {
    let mut out = Vec::new();
    let mut resume: u32 = 0;
    let mut buf: Vec<u64> = vec![0; words_for(64 * 1024)];
    // The SCM returns at most ~256 KB per call; a handful of rounds covers any machine.
    for _ in 0..256 {
        let mut needed = 0u32;
        let mut returned = 0u32;
        let result = unsafe {
            EnumServicesStatusExW(
                scm.0,
                SC_ENUM_PROCESS_INFO,
                SERVICE_WIN32,
                SERVICE_STATE_ALL,
                Some(as_bytes_mut(&mut buf)),
                &mut needed,
                &mut returned,
                Some(&mut resume),
                PCWSTR::null(),
            )
        };
        let more = match result {
            Ok(()) => false,
            Err(e) if win32_code(&e) == ERR_MORE_DATA => true,
            Err(e) => return Err(friendly_error(&e, "Could not list services")),
        };

        let items = unsafe {
            std::slice::from_raw_parts(
                buf.as_ptr() as *const ENUM_SERVICE_STATUS_PROCESSW,
                returned as usize,
            )
        };
        for item in items {
            let Some(name) = pwstr_to_string(item.lpServiceName) else {
                continue;
            };
            let display_name = pwstr_to_string(item.lpDisplayName)
                .filter(|d| !d.trim().is_empty())
                .unwrap_or_else(|| name.clone());
            out.push(RawService {
                name,
                display_name,
                state: item.ServiceStatusProcess.dwCurrentState,
                pid: item.ServiceStatusProcess.dwProcessId,
            });
        }

        if !more {
            return Ok(out);
        }
        if returned == 0 {
            let want = words_for(needed);
            if want <= buf.len() {
                return Err("Could not list services.".to_string());
            }
            buf = vec![0; want];
        }
    }
    Ok(out)
}

fn is_delayed_auto_start(svc: &ScHandle) -> bool {
    let mut info = SERVICE_DELAYED_AUTO_START_INFO::default();
    let mut needed = 0u32;
    let ok = unsafe {
        let bytes = std::slice::from_raw_parts_mut(
            &mut info as *mut SERVICE_DELAYED_AUTO_START_INFO as *mut u8,
            std::mem::size_of::<SERVICE_DELAYED_AUTO_START_INFO>(),
        );
        QueryServiceConfig2W(
            svc.0,
            SERVICE_CONFIG_DELAYED_AUTO_START_INFO,
            Some(bytes),
            &mut needed,
        )
        .is_ok()
    };
    ok && info.fDelayedAutostart.as_bool()
}

fn start_type(svc: &ScHandle) -> &'static str {
    let mut needed = 0u32;
    unsafe {
        let _ = QueryServiceConfigW(svc.0, None, 0, &mut needed);
    }
    if needed == 0 {
        return "unknown";
    }
    let mut buf: Vec<u64> = vec![0; words_for(needed)];
    let cfg = buf.as_mut_ptr() as *mut QUERY_SERVICE_CONFIGW;
    let size = (buf.len() * 8) as u32;
    if unsafe { QueryServiceConfigW(svc.0, Some(cfg), size, &mut needed) }.is_err() {
        return "unknown";
    }
    let st = unsafe { (*cfg).dwStartType };
    if st == SERVICE_AUTO_START {
        if is_delayed_auto_start(svc) {
            "autoDelayed"
        } else {
            "auto"
        }
    } else if st == SERVICE_DEMAND_START {
        "manual"
    } else if st == SERVICE_DISABLED {
        "disabled"
    } else if st == SERVICE_BOOT_START {
        "boot"
    } else if st == SERVICE_SYSTEM_START {
        "system"
    } else {
        "unknown"
    }
}

fn description(svc: &ScHandle) -> Option<String> {
    let mut needed = 0u32;
    unsafe {
        let _ = QueryServiceConfig2W(svc.0, SERVICE_CONFIG_DESCRIPTION, None, &mut needed);
    }
    if (needed as usize) < std::mem::size_of::<SERVICE_DESCRIPTIONW>() {
        return None;
    }
    let mut buf: Vec<u64> = vec![0; words_for(needed)];
    unsafe {
        QueryServiceConfig2W(
            svc.0,
            SERVICE_CONFIG_DESCRIPTION,
            Some(as_bytes_mut(&mut buf)),
            &mut needed,
        )
    }
    .ok()?;
    let desc = unsafe { &*(buf.as_ptr() as *const SERVICE_DESCRIPTIONW) };
    let text = pwstr_to_string(desc.lpDescription)?;
    let text = text.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

pub(crate) fn collect_services() -> Result<Vec<ServiceEntry>, String> {
    let scm = open_scm(SC_MANAGER_CONNECT | SC_MANAGER_ENUMERATE_SERVICE)?;
    let raw = enumerate(&scm)?;

    let mut cache = config_cache();
    if cache.as_ref().map_or(true, |(at, _)| at.elapsed() >= CONFIG_TTL) {
        *cache = Some((Instant::now(), HashMap::new()));
    }
    let (_, configs) = cache.as_mut().expect("cache was just filled");
    // Forget services that no longer exist.
    {
        let names: std::collections::HashSet<&str> = raw.iter().map(|r| r.name.as_str()).collect();
        configs.retain(|name, _| names.contains(name.as_str()));
    }

    let mut list = Vec::with_capacity(raw.len());
    for r in raw {
        let (start, desc) = match configs.get(&r.name) {
            Some((start, desc)) => (*start, desc.clone()),
            None => {
                let wname = wide(&r.name);
                let config = match unsafe {
                    OpenServiceW(scm.0, PCWSTR(wname.as_ptr()), SERVICE_QUERY_CONFIG)
                } {
                    Ok(h) => {
                        let svc = ScHandle(h);
                        Some((start_type(&svc), description(&svc)))
                    }
                    Err(_) => None,
                };
                match config {
                    Some((start, desc)) => {
                        configs.insert(r.name.clone(), (start, desc.clone()));
                        (start, desc)
                    }
                    // Not cached, so a transient failure is retried next time.
                    None => ("unknown", None),
                }
            }
        };
        list.push(ServiceEntry {
            name: r.name,
            display_name: r.display_name,
            status: state_name(r.state).to_string(),
            pid: if r.pid != 0 { Some(r.pid) } else { None },
            start_type: start.to_string(),
            description: desc,
        });
    }
    list.sort_by_cached_key(|s| s.display_name.to_lowercase());
    Ok(list)
}

// ---------------------------------------------------------------------------
// Control
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum ServiceAction {
    Start,
    Stop,
    Restart,
}

fn query_status(svc: &ScHandle) -> Result<SERVICE_STATUS_PROCESS, String> {
    let mut status = SERVICE_STATUS_PROCESS::default();
    let mut needed = 0u32;
    unsafe {
        let bytes = std::slice::from_raw_parts_mut(
            &mut status as *mut SERVICE_STATUS_PROCESS as *mut u8,
            std::mem::size_of::<SERVICE_STATUS_PROCESS>(),
        );
        QueryServiceStatusEx(svc.0, SC_STATUS_PROCESS_INFO, Some(bytes), &mut needed)
    }
    .map_err(|e| friendly_error(&e, "Could not read the service status"))?;
    Ok(status)
}

/// Polls in 250 ms steps while the service sits in `pending`. Gives up after
/// 15 s, or longer (up to 30 s) when the service's own wait hint asks for it.
/// Returns the first non-pending status.
fn wait_while_pending(
    svc: &ScHandle,
    pending: SERVICE_STATUS_CURRENT_STATE,
) -> Result<Option<SERVICE_STATUS_PROCESS>, String> {
    let started = Instant::now();
    let mut limit = WAIT_BASE;
    loop {
        let status = query_status(svc)?;
        if status.dwCurrentState != pending {
            return Ok(Some(status));
        }
        let hint = Duration::from_millis(u64::from(status.dwWaitHint));
        if hint > limit {
            limit = hint.min(WAIT_MAX);
        }
        if started.elapsed() >= limit {
            return Ok(None);
        }
        std::thread::sleep(WAIT_STEP);
    }
}

fn stop_service(svc: &ScHandle, tolerate_not_running: bool) -> Result<(), String> {
    let current = query_status(svc)?;
    if current.dwCurrentState == SERVICE_STOPPED {
        return if tolerate_not_running {
            Ok(())
        } else {
            Err("The service is not running.".to_string())
        };
    }
    if current.dwCurrentState != SERVICE_STOP_PENDING {
        let mut status = SERVICE_STATUS::default();
        if let Err(e) = unsafe { ControlService(svc.0, SERVICE_CONTROL_STOP, &mut status) } {
            if tolerate_not_running && win32_code(&e) == ERR_SERVICE_NOT_ACTIVE {
                return Ok(());
            }
            return Err(friendly_error(&e, "Could not stop the service"));
        }
    }
    match wait_while_pending(svc, SERVICE_STOP_PENDING)? {
        Some(s) if s.dwCurrentState == SERVICE_STOPPED => Ok(()),
        Some(_) => Err("The service did not stop.".to_string()),
        None => Err("The service is taking a long time to stop. Refresh to check it.".to_string()),
    }
}

fn start_service(svc: &ScHandle) -> Result<(), String> {
    unsafe { StartServiceW(svc.0, None) }
        .map_err(|e| friendly_error(&e, "Could not start the service"))?;
    match wait_while_pending(svc, SERVICE_START_PENDING)? {
        Some(s) if s.dwCurrentState == SERVICE_STOPPED => {
            Err("The service started and then stopped again.".to_string())
        }
        Some(_) => Ok(()),
        None => {
            Err("The service is taking a long time to start. Refresh to check it.".to_string())
        }
    }
}

fn control_blocking(name: &str, action: ServiceAction) -> Result<(), String> {
    // The next list re-reads this service's config, whatever happens here.
    forget_config(name);
    let scm = open_scm(SC_MANAGER_CONNECT)?;
    let wname = wide(name);
    // Ask only for the rights the action uses: many services let standard
    // users start them but not stop them.
    let access = match action {
        ServiceAction::Start => SERVICE_START | SERVICE_QUERY_STATUS,
        ServiceAction::Stop => SERVICE_STOP | SERVICE_QUERY_STATUS,
        ServiceAction::Restart => SERVICE_START | SERVICE_STOP | SERVICE_QUERY_STATUS,
    };
    let svc = unsafe { OpenServiceW(scm.0, PCWSTR(wname.as_ptr()), access) }
        .map(ScHandle)
        .map_err(|e| friendly_error(&e, "Could not open the service"))?;

    match action {
        ServiceAction::Start => start_service(&svc),
        ServiceAction::Stop => stop_service(&svc, false),
        ServiceAction::Restart => {
            stop_service(&svc, true)?;
            start_service(&svc)
        }
    }
}

// ---------------------------------------------------------------------------
// Start type (Optimize > Debloat only)
// ---------------------------------------------------------------------------

/// Sets a service's start type: "manual", "auto" or "autoDelayed". Never
/// Disabled, and the service is not stopped or started. Needs admin.
pub(crate) fn set_start_type(name: &str, start: &str) -> Result<(), String> {
    use windows::Win32::System::Services::{
        ChangeServiceConfig2W, ChangeServiceConfigW, ENUM_SERVICE_TYPE, SERVICE_CHANGE_CONFIG,
        SERVICE_ERROR, SERVICE_NO_CHANGE,
    };
    let (start_type, delayed) = match start {
        "manual" => (SERVICE_DEMAND_START, None),
        "auto" => (SERVICE_AUTO_START, Some(false)),
        "autoDelayed" => (SERVICE_AUTO_START, Some(true)),
        _ => return Err("Unknown start type.".to_string()),
    };
    forget_config(name);
    let scm = open_scm(SC_MANAGER_CONNECT)?;
    let wname = wide(name);
    let svc = unsafe { OpenServiceW(scm.0, PCWSTR(wname.as_ptr()), SERVICE_CHANGE_CONFIG | SERVICE_QUERY_CONFIG) }
        .map(ScHandle)
        .map_err(|e| friendly_error(&e, "Could not open the service"))?;
    unsafe {
        ChangeServiceConfigW(
            svc.0,
            ENUM_SERVICE_TYPE(SERVICE_NO_CHANGE),
            start_type,
            SERVICE_ERROR(SERVICE_NO_CHANGE),
            PCWSTR::null(),
            PCWSTR::null(),
            None,
            PCWSTR::null(),
            PCWSTR::null(),
            PCWSTR::null(),
            PCWSTR::null(),
        )
    }
    .map_err(|e| friendly_error(&e, "Could not change the start type"))?;
    if let Some(delayed) = delayed {
        let info = SERVICE_DELAYED_AUTO_START_INFO { fDelayedAutostart: delayed.into() };
        unsafe {
            ChangeServiceConfig2W(
                svc.0,
                SERVICE_CONFIG_DELAYED_AUTO_START_INFO,
                Some(&info as *const SERVICE_DELAYED_AUTO_START_INFO as *const core::ffi::c_void),
            )
        }
        .map_err(|e| friendly_error(&e, "Could not change the delayed start"))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn list_services() -> Result<Vec<ServiceEntry>, String> {
    tauri::async_runtime::spawn_blocking(collect_services)
        .await
        .map_err(|e| format!("Could not list services: {e}"))?
}

#[tauri::command]
pub async fn control_service(name: String, action: String) -> Result<(), String> {
    let action = match action.as_str() {
        "start" => ServiceAction::Start,
        "stop" => ServiceAction::Stop,
        "restart" => ServiceAction::Restart,
        _ => return Err("Unknown service action.".to_string()),
    };
    if name.trim().is_empty() {
        return Err("Service not found. Refresh the list.".to_string());
    }
    tauri::async_runtime::spawn_blocking(move || control_blocking(&name, action))
        .await
        .map_err(|e| format!("Service action failed: {e}"))?
}
